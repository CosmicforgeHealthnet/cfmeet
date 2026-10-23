//! Shared harness for API Gateway tests: the REAL gateway router with a scripted customer
//! authority, and REAL upstream HTTP servers playing the owning services. Each upstream
//! verifies the gateway's Ed25519 service tokens with its OWN trust bundle and receiver
//! permission (as Calling / Messaging / the Developer Platform do), or records the
//! transported end-user token (Meeting Service), and records exactly what reached it.
//! No database (the gateway has none).
#![allow(dead_code)]

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use api_gateway::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags},
    http,
    identity::{
        AuthenticatedCustomer, Authentication, AuthorityUnavailable, CustomerAuthority, Throttle,
        TrustClassification,
    },
    quota::QuotaEngine,
    routing::{AuthMode, Protocol, RouteDefinition, RouteTable, RoutingControls},
    state::AppState,
};
use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{ws::WebSocketUpgrade, Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    services, ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext,
    InternalPermission, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

pub const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;

pub fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

pub fn customer(scopes: &[&str]) -> AuthenticatedCustomer {
    AuthenticatedCustomer {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
        environment_kind: ApplicationEnvironmentKind::Sandbox,
        service_account_id: Uuid::new_v4(),
        credential_id: Uuid::new_v4(),
        classification: TrustClassification::External,
        full_authority: true,
        scopes: scopes
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
    }
}

/// A scripted Platform Access.
#[derive(Default)]
pub struct FakeAuthority {
    pub tokens: Mutex<HashMap<String, Authentication>>,
    pub down: Mutex<bool>,
    pub calls: Mutex<usize>,
}

impl FakeAuthority {
    pub fn add(&self, token: &str, c: AuthenticatedCustomer) {
        self.tokens
            .lock()
            .unwrap()
            .insert(token.into(), Authentication::Authenticated(Box::new(c)));
    }

    pub fn throttle(&self, token: &str, seconds: u64) {
        self.tokens.lock().unwrap().insert(
            token.into(),
            Authentication::Rejected {
                reason: "throttled".into(),
                throttle: Some(Throttle {
                    retry_after_seconds: seconds,
                    threshold_crossed: true,
                }),
            },
        );
    }
}

#[async_trait]
impl CustomerAuthority for FakeAuthority {
    async fn authenticate(
        &self,
        presented: &str,
        _correlation: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        *self.calls.lock().unwrap() += 1;
        if *self.down.lock().unwrap() {
            return Err(AuthorityUnavailable("down".into()));
        }
        Ok(self
            .tokens
            .lock()
            .unwrap()
            .get(presented)
            .cloned()
            .unwrap_or(Authentication::Rejected {
                reason: "unknown".into(),
                throttle: None,
            }))
    }
}

/// What reached an upstream.
#[derive(Debug, Clone)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: HeaderMap,
    pub body: Value,
    /// `Some` when a gateway service token was presented and verified.
    pub issuer: Option<String>,
    pub correlation_id: Option<String>,
    pub request_id: Option<String>,
    pub delegated: Option<DelegatedPlatformContext>,
    pub expires_in: i64,
}

#[derive(Clone)]
struct UpstreamState {
    /// `None`: an end-user-token service (no gateway token expected).
    verifier: Option<(Arc<ServiceTokenVerifier>, InternalPermission)>,
    received: Arc<Mutex<Vec<Received>>>,
    delay: Arc<Mutex<Duration>>,
    rejected: Arc<Mutex<usize>>,
}

/// A real HTTP upstream.
pub struct Upstream {
    pub base_url: String,
    pub received: Arc<Mutex<Vec<Received>>>,
    pub delay: Arc<Mutex<Duration>>,
    pub rejected: Arc<Mutex<usize>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn upstream_handler(
    State(s): State<UpstreamState>,
    request: Request,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_string();
    let (issuer, correlation, request_id, delegated, expires_in) = match &s.verifier {
        Some((verifier, permission))
            if parts.headers.contains_key("x-cf-service-authorization") =>
        {
            match verifier.verify_headers(&parts.headers) {
                Ok(ctx) => {
                    if ctx.require_permission(*permission).is_err() {
                        return StatusCode::FORBIDDEN.into_response();
                    }
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;
                    (
                        Some(ctx.authenticated_service().as_str().to_string()),
                        Some(ctx.correlation_id().as_str().to_string()),
                        Some(ctx.request_id().as_str().to_string()),
                        ctx.delegated_platform_context().cloned(),
                        ctx.expires_at() - now,
                    )
                }
                Err(_) => {
                    *s.rejected.lock().unwrap() += 1;
                    return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": {"code": "INTERNAL_AUTHENTICATION_FAILED", "message": "x", "requestId": null}}))).into_response();
                }
            }
        }
        _ => (None, None, None, None, 0),
    };
    let bytes = body.collect().await.unwrap().to_bytes();
    s.received.lock().unwrap().push(Received {
        method: parts.method.to_string(),
        path: path.clone(),
        query: parts.uri.query().map(str::to_string),
        headers: parts.headers.clone(),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        issuer: issuer.clone(),
        correlation_id: correlation,
        request_id,
        delegated: delegated.clone(),
        expires_in,
    });
    let delay = *s.delay.lock().unwrap();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    if path.ends_with("/not-yours") {
        // The domain service keeps final resource authorization.
        return (
            StatusCode::FORBIDDEN,
            [("x-cf-internal-detail", "upstream-only")],
            axum::Json(json!({"error": {"code": "CALL_NOT_FOUND_OR_FORBIDDEN", "message": "Not yours.", "requestId": "req_upstream"}})),
        )
            .into_response();
    }
    if path.ends_with("/big") {
        let chunk = vec![b'x'; 64 * 1024];
        let stream = futures_util::stream::iter(
            (0..48).map(move |_| Ok::<_, std::io::Error>(bytes::Bytes::from(chunk.clone()))),
        );
        return (
            [("content-type", "application/octet-stream")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    if path.ends_with("/created") {
        return (
            StatusCode::CREATED,
            [
                ("location", "/v1/test/items/abc"),
                ("idempotent-replayed", "true"),
                ("x-cf-internal-detail", "upstream-only"),
                ("set-cookie", "session=leak"),
            ],
            axum::Json(json!({ "id": "abc" })),
        )
            .into_response();
    }
    if path.ends_with("/text") {
        return (
            [("content-type", "text/plain; charset=utf-8")],
            "plain upstream text",
        )
            .into_response();
    }
    let tenant = delegated.as_ref().map(|d| d.tenant_id().to_string());
    (
        StatusCode::OK,
        [
            ("x-cf-internal-detail", "upstream-only"),
            ("cache-control", "no-store"),
        ],
        axum::Json(json!({
            "tenantId": tenant,
            "path": path,
            "query": parts.uri.query(),
            "auth": if issuer.is_some() { "service" } else { "passthrough" },
            "authorization": parts.headers.get("authorization").and_then(|v| v.to_str().ok()),
            "credentialId": delegated.as_ref().and_then(|d| d.initiating_credential_id()),
            "body": serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
        })),
    )
        .into_response()
}

/// A WebSocket upstream: requires the `cfmeet.ticket.good` subprotocol ticket (refuses
/// 401 otherwise), selects `cfmeet.messaging.v1`, echoes frames, closes with 4001 on
/// "bye".
async fn ws_handler(headers: HeaderMap, upgrade: WebSocketUpgrade) -> axum::response::Response {
    use axum::extract::ws::{CloseFrame, Message};
    let offered = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !offered.split(',').any(|p| p.trim() == "cfmeet.ticket.good") {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(
                json!({"error": {"code": "TICKET_INVALID", "message": "x", "requestId": null}}),
            ),
        )
            .into_response();
    }
    upgrade
        .protocols(["cfmeet.messaging.v1"])
        .on_upgrade(|mut socket| async move {
            while let Some(Ok(m)) = socket.recv().await {
                match m {
                    Message::Text(t) if t == "bye" => {
                        let _ = socket
                            .send(Message::Close(Some(CloseFrame {
                                code: 4001,
                                reason: "ticket expired".into(),
                            })))
                            .await;
                        break;
                    }
                    Message::Close(_) => break,
                    other => {
                        if socket.send(other).await.is_err() {
                            break;
                        }
                    }
                }
            }
        })
}

impl Upstream {
    /// An upstream receiving `permission` from the gateway as `receiver` (or, with
    /// `None`, an end-user-token service).
    pub async fn start(gateway_key: &ServiceSigningKey, receiver: Option<(&str, &str)>) -> Self {
        let verifier = receiver.map(|(receiver, permission)| {
            let bundle = json!({
                "bundle_version": 1, "receiver": receiver, "runtime_environment": ENV,
                "trusted_issuers": [{
                    "issuer": services::API_GATEWAY, "audience": receiver, "runtime_environment": ENV,
                    "keys": [gateway_key.public_jwk()], "permissions": [permission],
                }],
            })
            .to_string();
            let policy = TrustPolicy::from_json(&bundle, &ServiceId::parse(receiver).unwrap(), ENV).unwrap();
            (
                Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock))),
                InternalPermission::parse(permission).unwrap(),
            )
        });
        let state = UpstreamState {
            verifier,
            received: Arc::default(),
            delay: Arc::new(Mutex::new(Duration::ZERO)),
            rejected: Arc::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (received, delay, rejected) = (
            state.received.clone(),
            state.delay.clone(),
            state.rejected.clone(),
        );
        let router = Router::new()
            .route("/v1/test/realtime", axum::routing::get(ws_handler))
            .fallback(upstream_handler)
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            base_url,
            received,
            delay,
            rejected,
            task,
        }
    }

    pub fn last(&self) -> Received {
        self.received
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a request reached the upstream")
    }

    pub fn count(&self) -> usize {
        self.received.lock().unwrap().len()
    }
}

pub fn route(
    id: &str,
    domain: &str,
    method: Method,
    pattern: &str,
    upstream: &str,
    auth: &[AuthMode],
    scope: Option<&str>,
) -> RouteDefinition {
    RouteDefinition {
        id: id.into(),
        domain: domain.into(),
        method,
        pattern: pattern.into(),
        protocol: Protocol::Rest,
        upstream: upstream.into(),
        upstream_path: pattern.into(),
        auth: auth.to_vec(),
        required_scope: scope.map(str::to_string),
        forward_headers: if domain == "messaging" {
            vec!["cf-acting-principal".into()]
        } else {
            vec![]
        },
        timeout: None,
    }
}

/// Routes of the test table.
pub fn test_routes() -> Vec<RouteDefinition> {
    use AuthMode::*;
    let c = services::CALLING;
    let mut context = route(
        "gateway.context",
        "gateway",
        Method::GET,
        "/v1/developer-platform/context",
        services::DEVELOPER_PLATFORM,
        &[PlatformCredential],
        None,
    );
    context.upstream_path = "/internal/v1/gateway/context".into();
    let mut ws = route(
        "messaging.realtime",
        "messaging",
        Method::GET,
        "/v1/test/realtime",
        services::MESSAGING,
        &[RealtimeTicket],
        None,
    );
    ws.protocol = Protocol::WebSocket;
    vec![
        context,
        route(
            "calling.items.get",
            "calling",
            Method::GET,
            "/v1/test/items/{item_id}",
            c,
            &[PlatformCredential],
            Some("test.read"),
        ),
        route(
            "calling.items.create",
            "calling",
            Method::POST,
            "/v1/test/items",
            c,
            &[PlatformCredential],
            Some("test.write"),
        ),
        route(
            "calling.items.created",
            "calling",
            Method::POST,
            "/v1/test/created",
            c,
            &[PlatformCredential],
            None,
        ),
        route(
            "calling.history",
            "calling",
            Method::GET,
            "/v1/test/items/history",
            c,
            &[PlatformCredential],
            None,
        ),
        route(
            "calling.not-yours",
            "calling",
            Method::GET,
            "/v1/test/not-yours",
            c,
            &[PlatformCredential],
            None,
        ),
        route(
            "calling.big",
            "calling",
            Method::GET,
            "/v1/test/big",
            c,
            &[PlatformCredential],
            None,
        ),
        route(
            "calling.text",
            "calling",
            Method::GET,
            "/v1/test/text",
            c,
            &[PlatformCredential],
            None,
        ),
        route(
            "calling.signaling",
            "calling",
            Method::GET,
            "/v1/test/devices",
            c,
            &[SignalingCredential],
            None,
        ),
        route(
            "messaging.mixed",
            "messaging",
            Method::GET,
            "/v1/test/mixed",
            services::MESSAGING,
            &[PlatformCredential, ClientToken],
            None,
        ),
        route(
            "meetings.spaces",
            "meetings",
            Method::GET,
            "/api/v2/test-spaces/{id}",
            services::MEETING_SERVICE,
            &[MeetingSession, ParticipantToken],
            None,
        ),
        route(
            "developer-platform.orgs",
            "developer-platform",
            Method::GET,
            "/v1/test-orgs",
            services::DEVELOPER_PLATFORM,
            &[DeveloperSession, PlatformCredential],
            None,
        ),
        ws,
    ]
}

pub struct Gateway {
    pub router: Router,
    pub state: AppState,
    pub authority: Arc<FakeAuthority>,
    pub calling: Option<Upstream>,
    pub messaging: Option<Upstream>,
    pub meeting: Option<Upstream>,
    pub dp: Option<Upstream>,
    pub gateway_key_pem: String,
}

pub struct Options {
    pub upstream: bool,
    pub routes: Option<Vec<RouteDefinition>>,
    pub timeout: Duration,
    pub max_body: usize,
    pub quota: Option<Arc<QuotaEngine>>,
    pub cors: Vec<String>,
    pub controls: RoutingControls,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            upstream: true,
            routes: None,
            timeout: Duration::from_secs(5),
            max_body: 4096,
            quota: None,
            cors: vec![],
            controls: RoutingControls::default(),
        }
    }
}

pub fn runtime(key: ServiceSigningKey, targets: &[(ServiceId, String)]) -> ServiceAuthRuntime {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::api_gateway(),
        ENV,
        key,
        TokenTtl::seconds(60).unwrap(),
        Arc::new(SystemClock),
    ));
    let mut t = InternalTargets::new(ENV);
    let mut upstreams = std::collections::BTreeMap::new();
    for (s, u) in targets {
        t.insert(s.clone(), u).unwrap();
        upstreams.insert(s.as_str().to_string(), url::Url::parse(u).unwrap());
    }
    let client =
        Arc::new(InternalServiceClient::new(signer.clone(), t, Duration::from_secs(2)).unwrap());
    ServiceAuthRuntime::new(signer, client)
        .unwrap()
        .with_upstreams(upstreams)
}

impl Gateway {
    pub async fn new() -> Self {
        Self::with(Options::default()).await
    }

    pub async fn with(o: Options) -> Self {
        let k = key("api-gateway-test-1");
        let pem = k.to_pkcs8_pem().to_string();
        let (calling, messaging, meeting, dp) = if o.upstream {
            (
                Some(
                    Upstream::start(&k, Some((services::CALLING, "calling-request.forward"))).await,
                ),
                Some(
                    Upstream::start(&k, Some((services::MESSAGING, "messaging-request.forward")))
                        .await,
                ),
                Some(Upstream::start(&k, None).await),
                Some(
                    Upstream::start(
                        &k,
                        Some((services::DEVELOPER_PLATFORM, "developer-request.forward")),
                    )
                    .await,
                ),
            )
        } else {
            (None, None, None, None)
        };
        let mut targets = Vec::new();
        for (svc, up) in [
            (ServiceId::calling(), &calling),
            (ServiceId::messaging(), &messaging),
            (ServiceId::meeting_service(), &meeting),
            (ServiceId::developer_platform(), &dp),
        ] {
            if let Some(u) = up {
                targets.push((svc, u.base_url.clone()));
            }
        }
        let routes = RouteTable::new(o.routes.unwrap_or_else(test_routes), true).unwrap();
        let authority = Arc::new(FakeAuthority::default());
        let mut state = AppState::new(authority.clone(), runtime(k, &targets), routes)
            .with_flags(FeatureFlags {
                public_routing: true,
                websockets: true,
            })
            .with_controls(o.controls)
            .with_upstream_timeout(o.timeout)
            .with_max_body_bytes(o.max_body)
            .with_cors_allowed_origins(o.cors);
        if let Some(q) = o.quota {
            state = state.with_quota_engine(q);
        }
        Self {
            router: http::app(state.clone()),
            state,
            authority,
            calling,
            messaging,
            meeting,
            dp,
            gateway_key_pem: pem,
        }
    }

    pub fn calling(&self) -> &Upstream {
        self.calling.as_ref().unwrap()
    }

    pub fn messaging(&self) -> &Upstream {
        self.messaging.as_ref().unwrap()
    }

    pub fn meeting(&self) -> &Upstream {
        self.meeting.as_ref().unwrap()
    }

    pub fn dp(&self) -> &Upstream {
        self.dp.as_ref().unwrap()
    }

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Resp {
        let mut request = axum::http::Request::builder().method(method).uri(path);
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let body = match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Resp {
            status,
            headers,
            json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            bytes: bytes.to_vec(),
        }
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> Resp {
        let auth = token.map(|t| format!("Bearer {t}"));
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("authorization", a.as_str())).collect();
        self.call(Method::GET, path, &headers, None).await
    }

    /// Serves the gateway on a real socket (WebSocket tests).
    pub async fn serve(&self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = self.router.clone();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("127.0.0.1:{}", addr.port())
    }
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: Value,
    pub bytes: Vec<u8>,
}

impl Resp {
    pub fn code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }
}

/// Validates `value` against a definition of a checked-in contract.
#[track_caller]
pub fn conforms(reference: &str, value: &Value) {
    use jsonschema::{Registry, Resource};
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
    let read = |rel: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(rel)).unwrap()).unwrap()
    };
    let manifest = read("manifest.json");
    let mut builder = Registry::new();
    for c in manifest["contracts"].as_array().unwrap() {
        if c["kind"] == "json-schema" {
            let doc = read(c["path"].as_str().unwrap());
            let id = doc["$id"].as_str().unwrap().to_string();
            builder = builder.add(id, Resource::from_contents(doc)).unwrap();
        }
    }
    let registry = builder.prepare().unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .build(&json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "$ref": format!("https://schemas.cfmeet.com/{reference}") }))
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{reference} violated: {errors:?}\n{value:#}"
    );
}

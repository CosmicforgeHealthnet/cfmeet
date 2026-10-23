//! Shared harness for API Gateway tests: the REAL gateway router with a scripted customer
//! authority and a REAL upstream HTTP server that verifies the gateway's Ed25519 service
//! tokens with its own trust bundle (as the Developer Platform does) and records exactly
//! what reached it. No database (the gateway has none).
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
    routing::{Protocol, RouteDefinition, RouteTable},
    state::AppState,
};
use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext, KeyId, RuntimeEnvironment,
    ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl,
    TrustPolicy,
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

/// What reached the upstream (after its own token verification).
#[derive(Debug, Clone)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: HeaderMap,
    pub body: Value,
    pub issuer: String,
    pub correlation_id: String,
    pub request_id: String,
    pub delegated: Option<DelegatedPlatformContext>,
    pub expires_in: i64,
}

#[derive(Clone)]
struct UpstreamState {
    verifier: Arc<ServiceTokenVerifier>,
    received: Arc<Mutex<Vec<Received>>>,
    delay: Arc<Mutex<Duration>>,
    rejected: Arc<Mutex<usize>>,
}

/// A real HTTP upstream (identity `cfmeet.developer-platform`) verifying every request.
pub struct Upstream {
    pub base_url: String,
    pub received: Arc<Mutex<Vec<Received>>>,
    pub delay: Arc<Mutex<Duration>>,
    pub rejected: Arc<Mutex<usize>>,
    pub verifier: Arc<ServiceTokenVerifier>,
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
    let ctx = match s.verifier.verify_headers(&parts.headers) {
        Ok(c) => c,
        Err(_) => {
            *s.rejected.lock().unwrap() += 1;
            return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": {"code": "INTERNAL_AUTHENTICATION_FAILED", "message": "x", "requestId": null}}))).into_response();
        }
    };
    if ctx
        .require_permission(cf_service_auth::InternalPermission::DeveloperRequestForward)
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let bytes = body.collect().await.unwrap().to_bytes();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let delay = *s.delay.lock().unwrap();
    s.received.lock().unwrap().push(Received {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().map(str::to_string),
        headers: parts.headers.clone(),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        issuer: ctx.authenticated_service().as_str().to_string(),
        correlation_id: ctx.correlation_id().as_str().to_string(),
        request_id: ctx.request_id().as_str().to_string(),
        delegated: ctx.delegated_platform_context().cloned(),
        expires_in: ctx.expires_at() - now,
    });
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    if parts.uri.path().ends_with("/not-yours") {
        // The domain service keeps final resource authorization.
        return (
            StatusCode::FORBIDDEN,
            [("x-cf-internal-detail", "upstream-only")],
            axum::Json(
                json!({"error": {"code": "FORBIDDEN", "message": "Not yours.", "requestId": null}}),
            ),
        )
            .into_response();
    }
    let tenant = ctx
        .delegated_platform_context()
        .map(|d| d.tenant_id().to_string());
    (
        StatusCode::OK,
        [
            ("x-cf-internal-detail", "upstream-only"),
            ("cache-control", "no-store"),
        ],
        axum::Json(json!({ "tenantId": tenant, "path": parts.uri.path() })),
    )
        .into_response()
}

impl Upstream {
    pub async fn start(gateway_key: &ServiceSigningKey) -> Self {
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::DEVELOPER_PLATFORM,
            "runtime_environment": ENV,
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::API_GATEWAY,
                "audience": cf_service_auth::services::DEVELOPER_PLATFORM,
                "runtime_environment": ENV,
                "keys": [gateway_key.public_jwk()],
                "permissions": ["developer-request.forward"],
            }],
        })
        .to_string();
        let policy =
            TrustPolicy::from_json(&bundle, &ServiceId::developer_platform(), ENV).unwrap();
        let verifier = Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)));
        let state = UpstreamState {
            verifier: verifier.clone(),
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
        let router = Router::new().fallback(upstream_handler).with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            base_url,
            received,
            delay,
            rejected,
            verifier,
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
    id: &'static str,
    method: Method,
    pattern: &'static str,
    upstream_path: &'static str,
    scope: Option<&'static str>,
) -> RouteDefinition {
    RouteDefinition {
        id,
        method,
        pattern,
        protocol: Protocol::Rest,
        upstream: cf_service_auth::services::DEVELOPER_PLATFORM,
        upstream_path,
        required_scope: scope,
        timeout: None,
    }
}

/// Routes of the test table.
pub fn test_routes() -> Vec<RouteDefinition> {
    let mut ws = route(
        "test.stream",
        Method::GET,
        "/v1/test/stream",
        "/internal/v1/test/stream",
        None,
    );
    ws.protocol = Protocol::WebSocket;
    vec![
        route(
            "test.context",
            Method::GET,
            "/v1/developer-platform/context",
            "/internal/v1/gateway/context",
            None,
        ),
        route(
            "test.items.get",
            Method::GET,
            "/v1/test/items/{item_id}",
            "/internal/v1/test/items/{item_id}",
            Some("test.read"),
        ),
        route(
            "test.items.create",
            Method::POST,
            "/v1/test/items",
            "/internal/v1/test/items",
            Some("test.write"),
        ),
        route(
            "test.not-yours",
            Method::GET,
            "/v1/test/not-yours",
            "/internal/v1/test/not-yours",
            None,
        ),
        ws,
    ]
}

pub struct Gateway {
    pub router: Router,
    pub authority: Arc<FakeAuthority>,
    pub upstream: Option<Upstream>,
    pub gateway_key_pem: String,
}

pub struct Options {
    pub upstream: bool,
    pub routes: Option<Vec<RouteDefinition>>,
    pub timeout: Duration,
    pub max_body: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            upstream: true,
            routes: None,
            timeout: Duration::from_secs(5),
            max_body: 4096,
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
    for (s, u) in targets {
        t.insert(s.clone(), u).unwrap();
    }
    let client =
        Arc::new(InternalServiceClient::new(signer.clone(), t, Duration::from_secs(2)).unwrap());
    ServiceAuthRuntime::new(signer, client).unwrap()
}

impl Gateway {
    pub async fn new() -> Self {
        Self::with(Options::default()).await
    }

    pub async fn with(o: Options) -> Self {
        let k = key("api-gateway-test-1");
        let pem = k.to_pkcs8_pem().to_string();
        let upstream = if o.upstream {
            Some(Upstream::start(&k).await)
        } else {
            None
        };
        let mut targets = Vec::new();
        if let Some(u) = &upstream {
            targets.push((ServiceId::developer_platform(), u.base_url.clone()));
        }
        let routes = RouteTable::new(o.routes.unwrap_or_else(test_routes), true).unwrap();
        let authority = Arc::new(FakeAuthority::default());
        let state = AppState::new(authority.clone(), runtime(k, &targets), routes)
            .with_flags(FeatureFlags {
                public_routing: true,
                websockets: true,
            })
            .with_upstream_timeout(o.timeout)
            .with_max_body_bytes(o.max_body);
        Self {
            router: http::app(state),
            authority,
            upstream,
            gateway_key_pem: pem,
        }
    }

    pub fn upstream(&self) -> &Upstream {
        self.upstream.as_ref().unwrap()
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
        }
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> Resp {
        let auth = token.map(|t| format!("Bearer {t}"));
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("authorization", a.as_str())).collect();
        self.call(Method::GET, path, &headers, None).await
    }
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: Value,
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
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("https://schemas.cfmeet.com/{reference}")
        }))
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

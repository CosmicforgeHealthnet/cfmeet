//! Part VI §51 — requests the public API Gateway authenticated and forwarded to
//! Messaging's REAL public routes: no `Authorization`, a fresh gateway service token
//! (`messaging-request.forward`) whose signed delegated context names the initiating
//! credential, re-authenticated LIVE by a scripted Platform Access over real signed
//! internal HTTP (`POST /internal/v1/credentials/delegated-authority`).

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    routing::post,
    Json, Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    CorrelationId, DelegatedPlatformContext, InternalPermission, OutboundServiceRequest, ServiceId,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use common::{flows::idem, *};
use messaging::identity::{cache::CachingAuthority, PlatformAccessAuthority, TrustedContext};
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------------------
// Scripted Platform Access (real HTTP, verifies Messaging's service token)
// ---------------------------------------------------------------------------------------

/// What Platform Access observed of one delegated-authority call.
#[derive(Debug, Clone)]
struct Seen {
    caller: String,
    correlation_id: CorrelationId,
    delegated: Option<DelegatedPlatformContext>,
    body: String,
}

struct ScriptedPa {
    verifier: ServiceTokenVerifier,
    /// Live credentials: credential id -> (trusted context, authority mode).
    credentials: StdMutex<HashMap<Uuid, (TrustedContext, &'static str)>>,
    /// Bearer credentials (`/credentials/authenticate`): presented value -> credential id.
    tokens: StdMutex<HashMap<String, Uuid>>,
    down: StdMutex<bool>,
    seen: StdMutex<Vec<Seen>>,
}

fn chain(ctx: &TrustedContext, mode: &str) -> Value {
    json!({
        "authorityMode": mode,
        "tenant": { "id": ctx.scope.tenant_id, "classification": if ctx.first_party { "FIRST_PARTY" } else { "EXTERNAL" } },
        "application": { "id": ctx.scope.application_id },
        "environment": { "id": ctx.scope.environment_id, "kind": ctx.scope.environment_kind.as_str() },
        "serviceAccount": { "id": ctx.scope.service_account_id },
        "credential": { "id": ctx.credential_id },
        "scopes": ctx.scopes,
        "principalRestriction": ctx.principal_restriction,
    })
}

fn refusal(status: StatusCode, code: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": { "code": code } })))
}

/// Mirrors Platform Access's handler: `platform-credential.authenticate`, the credential
/// id from the signed delegated context, the live chain required to match its scope.
async fn delegated_authority(
    State(pa): State<Arc<ScriptedPa>>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, Json<Value>) {
    let Ok(caller) = pa.verifier.verify_headers(&headers) else {
        return refusal(StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED");
    };
    if !caller.has_permission(InternalPermission::PlatformCredentialAuthenticate) {
        return refusal(StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED");
    }
    pa.seen.lock().unwrap().push(Seen {
        caller: caller.authenticated_service().to_string(),
        correlation_id: caller.correlation_id().clone(),
        delegated: caller.delegated_platform_context().cloned(),
        body,
    });
    if *pa.down.lock().unwrap() {
        return refusal(StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE");
    }
    let Some(delegated) = caller.delegated_platform_context() else {
        return refusal(StatusCode::BAD_REQUEST, "DELEGATED_CONTEXT_REQUIRED");
    };
    let Some(credential_id) = delegated.initiating_credential_id() else {
        return refusal(StatusCode::BAD_REQUEST, "DELEGATED_CONTEXT_REQUIRED");
    };
    let live = pa.credentials.lock().unwrap().get(&credential_id).cloned();
    let answer = match live {
        Some((ctx, mode))
            if ctx.scope.tenant_id == delegated.tenant_id()
                && ctx.scope.application_id == delegated.application_id()
                && ctx.scope.environment_id == delegated.application_environment_id()
                && ctx.scope.service_account_id == delegated.initiating_service_account_id() =>
        {
            json!({ "outcome": "AUTHENTICATED", "authority": chain(&ctx, mode) })
        }
        _ => json!({ "outcome": "REJECTED", "reason": "unknown_credential" }),
    };
    (StatusCode::OK, Json(answer))
}

/// `/credentials/authenticate`, for the bearer path of the same harness.
async fn authenticate(
    State(pa): State<Arc<ScriptedPa>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let Ok(caller) = pa.verifier.verify_headers(&headers) else {
        return refusal(StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED");
    };
    if !caller.has_permission(InternalPermission::PlatformCredentialAuthenticate) {
        return refusal(StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED");
    }
    if *pa.down.lock().unwrap() {
        return refusal(StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE");
    }
    let id = body["credential"]
        .as_str()
        .and_then(|t| pa.tokens.lock().unwrap().get(t).copied());
    let live = id.and_then(|id| pa.credentials.lock().unwrap().get(&id).cloned());
    let answer = match live {
        Some((ctx, mode)) => json!({ "outcome": "AUTHENTICATED", "authority": chain(&ctx, mode) }),
        None => json!({ "outcome": "REJECTED", "reason": "unknown_credential" }),
    };
    (StatusCode::OK, Json(answer))
}

struct World {
    h: Harness,
    pa: Arc<ScriptedPa>,
    peers: Peers,
}

impl World {
    /// A harness whose customer authority is the REAL [`PlatformAccessAuthority`] talking
    /// to the scripted Platform Access over signed internal HTTP.
    async fn new() -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        let clock: SharedClock = Arc::new(SystemClock);
        let messaging = Arc::new(ServiceTokenSigner::new(
            ServiceId::messaging(),
            ENV,
            key("messaging-to-pa-test-1"),
            TokenTtl::default(),
            clock.clone(),
        ));
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::PLATFORM_ACCESS,
            "runtime_environment": ENV,
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::MESSAGING,
                "audience": cf_service_auth::services::PLATFORM_ACCESS,
                "runtime_environment": ENV,
                "keys": [messaging.public_jwk()],
                "permissions": ["platform-credential.authenticate"],
            }],
        })
        .to_string();
        let policy = TrustPolicy::from_json(&bundle, &ServiceId::platform_access(), ENV).unwrap();
        let pa = Arc::new(ScriptedPa {
            verifier: ServiceTokenVerifier::new(policy, clock),
            credentials: StdMutex::default(),
            tokens: StdMutex::default(),
            down: StdMutex::new(false),
            seen: StdMutex::default(),
        });
        let app = Router::new()
            .route("/internal/v1/credentials/authenticate", post(authenticate))
            .route(
                "/internal/v1/credentials/delegated-authority",
                post(delegated_authority),
            )
            .with_state(pa.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut targets = InternalTargets::new(ENV);
        targets.insert(ServiceId::platform_access(), &url).unwrap();
        let client = Arc::new(
            InternalServiceClient::new(messaging, targets, std::time::Duration::from_secs(2))
                .unwrap(),
        );
        let h = Harness::on(db, |state| {
            state.authority = Arc::new(CachingAuthority::new(
                Arc::new(PlatformAccessAuthority::new(client)),
                // Caching on: forwarded requests must still never be served from it.
                std::time::Duration::from_secs(60),
            ));
            state.authority.set_healthy(true);
        });
        Some(Self {
            h,
            pa,
            peers: peers(),
        })
    }

    /// A live credential of a new customer environment (with an ACTIVE principal).
    fn customer(&self, scopes: &[&str], mode: &'static str) -> (Customer, Uuid) {
        let c = self.h.customer(scopes);
        self.live(&c, mode);
        let p = self.h.principal(&c);
        (c, p)
    }

    fn live(&self, c: &Customer, mode: &'static str) {
        self.pa
            .credentials
            .lock()
            .unwrap()
            .insert(c.context.credential_id, (c.context.clone(), mode));
        self.pa
            .tokens
            .lock()
            .unwrap()
            .insert(c.token.clone(), c.context.credential_id);
    }

    fn calls(&self) -> usize {
        self.pa.seen.lock().unwrap().len()
    }

    /// A request as the API Gateway forwards it: signed by `signer` with `delegated`, plus
    /// `extra` headers; returns the response and the gateway's correlation id.
    async fn forward(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
        delegated: Option<DelegatedPlatformContext>,
        extra: &[(&str, String)],
    ) -> (Response, CorrelationId) {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::messaging());
        if let Some(d) = delegated {
            outbound = outbound.with_delegated_context(d);
        }
        let correlation = outbound.correlation_id().clone();
        let signed = signer.sign(&outbound).unwrap();
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
        for (k, v) in extra {
            b = b.header(*k, v);
        }
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        (self.h.raw(b.body(body).unwrap()).await, correlation)
    }
}

/// The delegated context the API Gateway signs for an authenticated credential.
fn gateway_context(ctx: &TrustedContext) -> DelegatedPlatformContext {
    ctx.scope
        .delegated()
        .with_initiating_credential(ctx.credential_id)
}

fn acting(p: Uuid) -> (&'static str, String) {
    ("cf-acting-principal", p.to_string())
}

fn group() -> Value {
    json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn forwarded_requests_are_accepted_and_scoped_to_the_live_chain() {
    let Some(w) = World::new().await else {
        return;
    };
    let (c, alice) = w.customer(flows::ALL_SCOPES, "FULL");
    w.h.allow_managed(&c, "GROUP").await;
    let gw = &w.peers.gateway;

    // A real public mutation route, forwarded without Authorization.
    let (r, correlation) = w
        .forward(
            gw,
            Method::POST,
            "/v1/conversations",
            Some(group()),
            Some(gateway_context(&c.context)),
            &[acting(alice), ("idempotency-key", idem())],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let cid = r.json["conversationId"].as_str().unwrap().to_string();
    // Platform Access was asked about exactly that credential, by Messaging, continuing
    // the gateway's operation, with no body content (the id travels signed).
    let seen = w.pa.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.caller, cf_service_auth::services::MESSAGING);
    assert_eq!(seen.correlation_id, correlation);
    assert_eq!(seen.delegated, Some(gateway_context(&c.context)));
    assert_eq!(seen.body, "{}");

    // The conversation lives in the chain's environment: the bearer credential of that
    // environment sees it, another environment does not.
    let r =
        w.h.get(&c, Some(alice), &format!("/v1/conversations/{cid}"))
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (other, other_p) = w.customer(flows::ALL_SCOPES, "FULL");
    let r =
        w.h.get(&other, Some(other_p), &format!("/v1/conversations/{cid}"))
            .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // ... and a forwarded read of the other environment does not either.
    let (r, _) = w
        .forward(
            gw,
            Method::GET,
            &format!("/v1/conversations/{cid}"),
            None,
            Some(gateway_context(&other.context)),
            &[acting(other_p)],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // A forwarded read of its own environment succeeds (never from the cache: every
    // forwarded request asks Platform Access).
    for _ in 0..2 {
        let before = w.calls();
        let (r, _) = w
            .forward(
                gw,
                Method::GET,
                &format!("/v1/conversations/{cid}"),
                None,
                Some(gateway_context(&c.context)),
                &[acting(alice)],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(w.calls(), before + 1);
    }

    // Scopes come from the LIVE chain, not from the gateway: a credential of the same
    // environment without `conversations.create` is refused.
    let reader = w.h.credential(&c, &["messages.read"]);
    w.live(&reader, "FULL");
    let (r, _) = w
        .forward(
            gw,
            Method::POST,
            "/v1/conversations",
            Some(group()),
            Some(gateway_context(&reader.context)),
            &[acting(alice), ("idempotency-key", idem())],
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE")
    );
    // The principal restriction of the chain applies, exactly as on the bearer path.
    let restricted = w.h.restricted(&c, &[Uuid::new_v4()]);
    w.live(&restricted, "FULL");
    let (r, _) = w
        .forward(
            gw,
            Method::POST,
            "/v1/conversations",
            Some(group()),
            Some(gateway_context(&restricted.context)),
            &[acting(alice), ("idempotency-key", idem())],
        )
        .await;
    assert_eq!(r.reason(), "CREDENTIAL_RESTRICTION", "{}", r.text);
    // A suspended scope (SAFETY_ONLY chain) is refused like on the bearer path.
    let suspended = w.h.credential(&c, flows::ALL_SCOPES);
    w.live(&suspended, "SAFETY_ONLY");
    let (r, _) = w
        .forward(
            gw,
            Method::GET,
            &format!("/v1/conversations/{cid}"),
            None,
            Some(gateway_context(&suspended.context)),
            &[acting(alice)],
        )
        .await;
    assert_eq!(r.code(), "AUTHORITY_SUSPENDED", "{}", r.text);

    // Revocation applies at once: the very next forwarded request is refused.
    w.pa.credentials
        .lock()
        .unwrap()
        .remove(&c.context.credential_id);
    let (r, _) = w
        .forward(
            gw,
            Method::GET,
            &format!("/v1/conversations/{cid}"),
            None,
            Some(gateway_context(&c.context)),
            &[acting(alice)],
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED")
    );
}

#[tokio::test]
async fn forwarded_requests_fail_closed() {
    let Some(w) = World::new().await else {
        return;
    };
    let (c, alice) = w.customer(flows::ALL_SCOPES, "FULL");
    let path = format!("/v1/messaging/principals/{alice}/devices");
    let unauthorized = |r: &Response, what: &str| {
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{what}: {}", r.text);
        assert_eq!(r.code(), "AUTHENTICATION_FAILED", "{what}");
        conforms("messaging/common/v1.schema.json#/$defs/Error", &r.json);
        assert!(r.headers.get("www-authenticate").is_some(), "{what}");
        assert!(r.headers.get("x-request-id").is_some(), "{what}");
    };
    let ok = w
        .forward(
            &w.peers.gateway,
            Method::GET,
            &path,
            None,
            Some(gateway_context(&c.context)),
            &[acting(alice)],
        )
        .await
        .0;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);

    let before = w.calls();
    // No delegated context at all, and one without the initiating credential id.
    for delegated in [None, Some(c.context.scope.delegated())] {
        let (r, _) = w
            .forward(
                &w.peers.gateway,
                Method::GET,
                &path,
                None,
                delegated,
                &[acting(alice)],
            )
            .await;
        unauthorized(&r, "missing credential id");
    }
    // A trusted issuer without `messaging-request.forward`, the gateway's name with a key
    // the bundle does not list, an untrusted issuer.
    for (signer, what) in [
        (&w.peers.meeting, "trusted issuer without the grant"),
        (&w.peers.rogue_gateway, "untrusted gateway key"),
        (&w.h.untrusted, "untrusted issuer"),
    ] {
        let (r, _) = w
            .forward(
                signer,
                Method::GET,
                &path,
                None,
                Some(gateway_context(&c.context)),
                &[acting(alice)],
            )
            .await;
        unauthorized(&r, what);
    }
    // A gateway token AND a customer credential (even a valid one).
    let (r, _) = w
        .forward(
            &w.peers.gateway,
            Method::GET,
            &path,
            None,
            Some(gateway_context(&c.context)),
            &[
                acting(alice),
                ("authorization", format!("Bearer {}", c.token)),
            ],
        )
        .await;
    unauthorized(&r, "token and Authorization");
    // None of these reached Platform Access.
    assert_eq!(w.calls(), before);

    // Platform Access REJECTS: an unknown (or revoked) credential, and a credential the
    // signed context places in another scope.
    let mut unknown = c.context.clone();
    unknown.credential_id = Uuid::new_v4();
    let mut misplaced = c.context.clone();
    misplaced.scope.tenant_id = Uuid::new_v4();
    for (ctx, what) in [
        (unknown, "unknown credential"),
        (misplaced, "foreign scope"),
    ] {
        let (r, _) = w
            .forward(
                &w.peers.gateway,
                Method::GET,
                &path,
                None,
                Some(gateway_context(&ctx)),
                &[acting(alice)],
            )
            .await;
        unauthorized(&r, what);
    }
    assert_eq!(w.calls(), before + 2);

    // Platform Access unavailable: 503, never a default authority.
    *w.pa.down.lock().unwrap() = true;
    let (r, _) = w
        .forward(
            &w.peers.gateway,
            Method::GET,
            &path,
            None,
            Some(gateway_context(&c.context)),
            &[acting(alice)],
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE"),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn an_unreachable_platform_access_refuses_forwarded_requests() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    // The default deployment wiring: Platform Access target configured but not listening.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut targets = InternalTargets::new(ENV);
    targets
        .insert(
            ServiceId::platform_access(),
            &format!("http://127.0.0.1:{port}"),
        )
        .unwrap();
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::messaging(),
        ENV,
        key("messaging-unreachable-test-1"),
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let client = Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(2)).unwrap(),
    );
    let h = Harness::on(Arc::new(db), |state| {
        state.authority = Arc::new(CachingAuthority::new(
            Arc::new(PlatformAccessAuthority::new(client)),
            std::time::Duration::ZERO,
        ));
    });
    let c = h.customer(flows::ALL_SCOPES);
    let alice = h.principal(&c);
    let r = h
        .call_internal(
            &peers().gateway,
            Method::GET,
            &format!("/v1/messaging/principals/{alice}/devices"),
            None,
            Some(gateway_context(&c.context)),
            &[("cf-acting-principal", &alice.to_string())],
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
}

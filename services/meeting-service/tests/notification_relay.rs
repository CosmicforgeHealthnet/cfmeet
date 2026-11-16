//! Document 4 §3.1 — the Notification Service user relay (U1–U9 under
//! `/api/v2/notifications`).
//!
//! Like `/api/v2/communications/*`, Meeting verifies the Account session (401 first) and
//! forwards to I2 `POST /internal/v1/notifications/dispatch` with the server-resolved
//! account, the relative path, the raw query, the Idempotency-Key and the JSON body,
//! signed as `cfmeet.meeting-service` (a fake Notification Service verifies the real token
//! against its own trust bundle granting `notifications-user.forward`). The answer's status
//! and `schemaVersion: 1` body are returned with `Retry-After`, `ETag` and
//! `Cache-Control: no-store`. Without `CF_NOTIFICATIONS_BASE_URL` (the default) every
//! route answers 503 and nothing is forwarded.

mod common;

use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use backend::services::notification_relay::NotificationsRelay;
use cf_service_auth::{
    InternalPermission, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::{contract::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

/// One scripted answer of the fake Notification Service.
#[derive(Clone)]
struct Scripted {
    status: u16,
    body: String,
    headers: Vec<(&'static str, &'static str)>,
}

#[derive(Default)]
struct Fake {
    /// Every authenticated dispatch envelope.
    envelopes: Vec<Value>,
    unauthenticated: usize,
    script: Vec<Scripted>,
}

#[derive(Clone)]
struct FakeState {
    verifier: Arc<ServiceTokenVerifier>,
    inner: Arc<Mutex<Fake>>,
}

/// The fake I2: verifies the Meeting token (`notifications-user.forward`), records the
/// envelope and echoes it (or answers the next scripted response).
async fn dispatch(State(s): State<FakeState>, headers: HeaderMap, body: String) -> Response {
    let ctx = match s
        .verifier
        .verify_request(
            &Method::POST,
            "/internal/v1/notifications/dispatch",
            &headers,
        )
        .await
    {
        Ok(ctx) => ctx,
        Err(_) => {
            s.inner.lock().unwrap().unauthenticated += 1;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "schemaVersion": 1, "error": { "code": "INTERNAL_AUTHENTICATION_FAILED", "message": "x", "requestId": "r" } })),
            )
                .into_response();
        }
    };
    assert_eq!(ctx.authenticated_service(), &ServiceId::meeting_service());
    if !ctx.has_permission(InternalPermission::NotificationsUserForward) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "schemaVersion": 1, "error": { "code": "FORBIDDEN", "message": "x", "requestId": "r" } })),
        )
            .into_response();
    }
    let envelope: Value = serde_json::from_str(&body).unwrap();
    let mut inner = s.inner.lock().unwrap();
    inner.envelopes.push(envelope.clone());
    if !inner.script.is_empty() {
        let next = inner.script.remove(0);
        let mut r = Response::new(axum::body::Body::from(next.body));
        *r.status_mut() = StatusCode::from_u16(next.status).unwrap();
        for (k, v) in next.headers {
            r.headers_mut().insert(k, v.parse().unwrap());
        }
        return r;
    }
    let mut r = Json(json!({ "schemaVersion": 1, "echo": envelope })).into_response();
    if envelope["path"] == "/unread-count" {
        r.headers_mut().insert("etag", "W/\"3-7\"".parse().unwrap());
    }
    r
}

fn key(kid: &str) -> (ServiceSigningKey, ServiceSigningKey) {
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let public =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &key.to_pkcs8_pem()).unwrap();
    (key, public)
}

/// A fake Notification Service trusting `meeting` with `permission`; returns its URL.
async fn fake_notifications(
    meeting: &ServiceSigningKey,
    permission: &str,
    inner: Arc<Mutex<Fake>>,
) -> String {
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.notifications", "runtime_environment": "test",
        "trusted_issuers": [{ "issuer": "cfmeet.meeting-service", "audience": "cfmeet.notifications",
            "runtime_environment": "test", "keys": [meeting.public_jwk()], "permissions": [permission] }]});
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::notifications(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    let state = FakeState {
        verifier: Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock))),
        inner,
    };
    let app = Router::new()
        .route("/internal/v1/notifications/dispatch", post(dispatch))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    url
}

/// Point the app's relay at `url`, signing with `key`, and rebuild the router.
fn configure(app: &mut TestApp, key: ServiceSigningKey, url: &str) {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
        key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    app.state.notifications = Some(Arc::new(NotificationsRelay::new(signer, url).unwrap()));
    app.router = backend::routes::create_routes(app.state.clone())
        .route_layer(axum::middleware::from_fn(route_coverage::record));
}

struct Relay {
    app: TestApp,
    fake: Arc<Mutex<Fake>>,
}

impl Relay {
    async fn start() -> Option<Self> {
        if !require_test_redis() {
            return None;
        }
        let mut app = TestApp::new().await?;
        let (signing, public) = key("meeting-service-test-1");
        let fake = Arc::new(Mutex::new(Fake::default()));
        let url = fake_notifications(&public, "notifications-user.forward", fake.clone()).await;
        configure(&mut app, signing, &url);
        Some(Self { app, fake })
    }

    fn last(&self) -> Value {
        self.fake.lock().unwrap().envelopes.last().cloned().unwrap()
    }

    fn forwarded(&self) -> usize {
        self.fake.lock().unwrap().envelopes.len()
    }

    fn script(&self, status: u16, body: Value, headers: Vec<(&'static str, &'static str)>) {
        self.fake.lock().unwrap().script.push(Scripted {
            status,
            body: body.to_string(),
            headers,
        });
    }
}

async fn send(
    app: &TestApp,
    method: Method,
    uri: &str,
    token: Option<&str>,
    extra: &[(&str, &str)],
    body: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let bearer = token.map(|t| format!("Bearer {t}"));
    let mut headers: Vec<(&str, &str)> = extra.to_vec();
    if let Some(b) = bearer.as_deref() {
        headers.push(("authorization", b));
    }
    raw(app, method, uri, &headers, body).await
}

/// The nine operations with a representative request (method, URI, body).
fn operations(device: Uuid, notification: Uuid) -> Vec<(Method, String, Option<&'static str>)> {
    vec![
        (Method::GET, "/api/v2/notifications".into(), None),
        (
            Method::GET,
            "/api/v2/notifications/unread-count".into(),
            None,
        ),
        (
            Method::PATCH,
            format!("/api/v2/notifications/{notification}"),
            Some(r#"{"read":true,"expectedVersion":1}"#),
        ),
        (
            Method::POST,
            "/api/v2/notifications/read".into(),
            Some(r#"{"upTo":"2026-11-16T10:00:00Z"}"#),
        ),
        (
            Method::POST,
            "/api/v2/notifications/dismiss".into(),
            Some(r#"{"ids":[]}"#),
        ),
        (
            Method::GET,
            "/api/v2/notifications/preferences".into(),
            None,
        ),
        (
            Method::PUT,
            "/api/v2/notifications/preferences".into(),
            Some(r#"{"expectedVersion":1,"categories":{}}"#),
        ),
        (
            Method::PUT,
            format!("/api/v2/notifications/devices/{device}/push"),
            Some(r#"{"platform":"FCM","handle":"h"}"#),
        ),
        (
            Method::DELETE,
            format!("/api/v2/notifications/devices/{device}/push"),
            None,
        ),
    ]
}

#[tokio::test]
async fn relay_requires_an_account_session_and_is_dark_by_default() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    assert!(app.state.notifications.is_none(), "not routed by default");
    let doc = openapi();
    let alice = app.user("alice").await;
    for (method, uri, body) in operations(Uuid::new_v4(), Uuid::new_v4()) {
        // No session: 401 before anything else.
        let (s, h, v) = send(&app, method.clone(), &uri, None, &[], body).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}: {v}");
        assert_eq!(code(&v), "AUTHENTICATION_REQUIRED");
        assert_error_contract(&doc, s, &h, &v);
        // A garbage token is no session either.
        let (s, _, _) = send(&app, method.clone(), &uri, Some("garbage"), &[], body).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}");
        // Signed in, relay not configured: 503 and nothing forwarded.
        if require_test_redis() {
            let (s, h, v) = send(
                &app,
                method.clone(),
                &uri,
                Some(&alice.token),
                &[("idempotency-key", "key-0001")],
                body,
            )
            .await;
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{method} {uri}: {v}");
            assert_eq!(code(&v), "SERVICE_UNAVAILABLE");
            assert_error_contract(&doc, s, &h, &v);
        }
    }
}

#[tokio::test]
async fn relay_forwards_the_server_resolved_account_and_the_request() {
    let Some(r) = Relay::start().await else {
        return;
    };
    let app = &r.app;
    let alice = app.user("alice").await;
    let account = format!("cfmeet-user:{}", alice.id);
    let (device, notification) = (Uuid::new_v4(), Uuid::new_v4());

    // U1: the raw query is forwarded verbatim; the account is never the client's.
    let (s, h, v) = send(
        app,
        Method::GET,
        "/api/v2/notifications?state=unread&limit=5&category=CONTACT_REQUESTS%2CMISSED_CALLS&account=cfmeet-user:00000000-0000-4000-8000-000000000000",
        Some(&alice.token),
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(h.get("cache-control").unwrap(), "no-store");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(
        r.last(),
        json!({ "method": "GET", "path": "", "account": account,
                "query": "state=unread&limit=5&category=CONTACT_REQUESTS%2CMISSED_CALLS&account=cfmeet-user:00000000-0000-4000-8000-000000000000" })
    );

    // U2: ETag relayed; a matching If-None-Match answers 304 without a body.
    let (s, h, v) = send(
        app,
        Method::GET,
        "/api/v2/notifications/unread-count",
        Some(&alice.token),
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(h.get("etag").unwrap(), "W/\"3-7\"");
    assert_eq!(
        r.last(),
        json!({ "method": "GET", "path": "/unread-count", "account": account })
    );
    let (s, h, v) = send(
        app,
        Method::GET,
        "/api/v2/notifications/unread-count",
        Some(&alice.token),
        &[("if-none-match", "W/\"3-7\"")],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    assert_eq!(v, Value::Null);
    assert_eq!(h.get("etag").unwrap(), "W/\"3-7\"");
    assert_eq!(h.get("cache-control").unwrap(), "no-store");

    // U3–U9: method, relative path, Idempotency-Key and body.
    let expected_paths = [
        format!("/{notification}"),
        "/read".to_string(),
        "/dismiss".to_string(),
        "/preferences".to_string(),
        "/preferences".to_string(),
        format!("/devices/{device}/push"),
        format!("/devices/{device}/push"),
    ];
    for ((method, uri, body), path) in operations(device, notification)
        .into_iter()
        .skip(2)
        .zip(expected_paths)
    {
        let (s, h, v) = send(
            app,
            method.clone(),
            &uri,
            Some(&alice.token),
            &[("idempotency-key", "client-key-0042")],
            body,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{method} {uri}: {v}");
        assert_eq!(h.get("cache-control").unwrap(), "no-store");
        let mut expected = json!({ "method": method.as_str(), "path": path, "account": account,
                                   "idempotencyKey": "client-key-0042" });
        if let Some(b) = body {
            expected["body"] = serde_json::from_str(b).unwrap();
        }
        assert_eq!(r.last(), expected, "{method} {uri}");
        assert_eq!(v["echo"], expected);
    }
    // U9's platform filter travels in the query.
    let (s, _, _) = send(
        app,
        Method::DELETE,
        &format!("/api/v2/notifications/devices/{device}/push?platform=FCM"),
        Some(&alice.token),
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r.last()["query"], "platform=FCM");
    assert!(r.last().get("idempotencyKey").is_none());
    assert_eq!(r.fake.lock().unwrap().unauthenticated, 0);
}

#[tokio::test]
async fn relay_returns_the_notification_service_answer_and_refuses_off_contract_ones() {
    let Some(r) = Relay::start().await else {
        return;
    };
    let app = &r.app;
    let alice = app.user("alice").await;
    let doc = openapi();
    let token = Some(alice.token.as_str());

    // A user-level error and a rate limit are relayed with their headers.
    r.script(
        400,
        json!({ "schemaVersion": 1, "error": { "code": "VALIDATION_ERROR", "message": "Idempotency-Key is required", "requestId": "n1" } }),
        vec![],
    );
    let (s, _, v) = send(
        app,
        Method::POST,
        "/api/v2/notifications/read",
        token,
        &[],
        Some("{}"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["message"], "Idempotency-Key is required");
    r.script(
        429,
        json!({ "schemaVersion": 1, "error": { "code": "RATE_LIMITED", "message": "x", "requestId": "n2" } }),
        vec![("retry-after", "17")],
    );
    let (s, h, v) = send(
        app,
        Method::GET,
        "/api/v2/notifications/preferences",
        token,
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(h.get("retry-after").unwrap(), "17");
    assert_eq!(h.get("cache-control").unwrap(), "no-store");
    // A 404 for someone else's notification is relayed as is.
    r.script(
        404,
        json!({ "schemaVersion": 1, "error": { "code": "NOT_FOUND", "message": "x", "requestId": "n3" } }),
        vec![],
    );
    let (s, _, _) = send(
        app,
        Method::PATCH,
        &format!("/api/v2/notifications/{}", Uuid::new_v4()),
        token,
        &[("idempotency-key", "client-key-0043")],
        Some(r#"{"read":true,"expectedVersion":3}"#),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // An empty 204 is relayed.
    r.script(204, Value::Null, vec![]);
    r.fake.lock().unwrap().script.last_mut().unwrap().body = String::new();
    let (s, _, v) = send(
        app,
        Method::DELETE,
        &format!("/api/v2/notifications/devices/{}/push", Uuid::new_v4()),
        token,
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(v, Value::Null);

    // Off-contract answers are never relayed: a body without schemaVersion 1, and the
    // Notification Service refusing Meeting's own service authentication.
    r.script(200, json!({ "items": [] }), vec![]);
    let (s, h, v) = send(app, Method::GET, "/api/v2/notifications", token, &[], None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_error_contract(&doc, s, &h, &v);
    r.script(
        401,
        json!({ "schemaVersion": 1, "error": { "code": "INTERNAL_AUTHENTICATION_FAILED", "message": "x", "requestId": "n4" } }),
        vec![],
    );
    let (s, _, v) = send(app, Method::GET, "/api/v2/notifications", token, &[], None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(code(&v), "SERVICE_UNAVAILABLE");

    // Requests Meeting refuses before forwarding.
    let before = r.forwarded();
    for (method, uri, headers, body, why) in [
        (
            Method::PATCH,
            "/api/v2/notifications/not-a-uuid".to_string(),
            vec![],
            Some("{}"),
            "malformed notification id",
        ),
        (
            Method::PUT,
            "/api/v2/notifications/devices/nope/push".to_string(),
            vec![],
            Some("{}"),
            "malformed device id",
        ),
        (
            Method::PUT,
            "/api/v2/notifications/preferences".to_string(),
            vec![],
            Some("{not json"),
            "invalid JSON",
        ),
        (
            Method::POST,
            "/api/v2/notifications/dismiss".to_string(),
            vec![("idempotency-key", "has space")],
            Some("{}"),
            "invalid Idempotency-Key",
        ),
        (
            Method::GET,
            format!("/api/v2/notifications?cursor={}", "x".repeat(1100)),
            vec![],
            None,
            "query too long",
        ),
    ] {
        let (s, h, v) = send(app, method, &uri, token, &headers, body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{why}: {v}");
        assert_eq!(code(&v), "VALIDATION_ERROR", "{why}");
        assert_error_contract(&doc, s, &h, &v);
    }
    assert_eq!(r.forwarded(), before, "nothing forwarded");
}

#[tokio::test]
async fn relay_fails_closed_on_trust_and_transport() {
    if !require_test_redis() {
        return;
    }
    let Some(mut app) = TestApp::new().await else {
        return;
    };
    let alice = app.user("alice").await;
    // The Notification Service does not grant Meeting the forward permission.
    let (signing, public) = key("meeting-service-test-2");
    let fake = Arc::new(Mutex::new(Fake::default()));
    let url = fake_notifications(&public, "notifications-event.publish", fake.clone()).await;
    configure(&mut app, signing, &url);
    let (s, _, v) = raw(
        &app,
        Method::GET,
        "/api/v2/notifications/unread-count",
        &[("authorization", &format!("Bearer {}", alice.token))],
        None,
    )
    .await;
    // Authenticated but refused (403): a service-trust outcome, never relayed to the user.
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(code(&v), "SERVICE_UNAVAILABLE");
    assert_eq!(fake.lock().unwrap().unauthenticated, 0);
    assert!(fake.lock().unwrap().envelopes.is_empty());

    // An unknown signing key: the receiver refuses it and the user sees 503.
    let (other, _) = key("meeting-service-test-3");
    configure(&mut app, other, &url);
    let (s, _, v) = raw(
        &app,
        Method::GET,
        "/api/v2/notifications/unread-count",
        &[("authorization", &format!("Bearer {}", alice.token))],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(fake.lock().unwrap().unauthenticated, 1);

    // Unreachable Notification Service.
    let (k, _) = key("meeting-service-test-4");
    configure(&mut app, k, "http://127.0.0.1:1");
    let (s, _, v) = raw(
        &app,
        Method::GET,
        "/api/v2/notifications",
        &[("authorization", &format!("Bearer {}", alice.token))],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(code(&v), "SERVICE_UNAVAILABLE");
}

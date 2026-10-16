//! Part D §26/§30: the public Privacy Relay ingress on Calling.
//!
//! Calling's real `HttpPrivacyRelay` adapter talks to a mock Privacy Relay HTTP server
//! that verifies every request with a Privacy Relay trust policy (Calling's key,
//! receiver-authoritative permissions) and records what it received.

mod common;

use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json, Router,
};
use calling::platform::scopes::names;
use cf_service_auth::{ServiceId, ServiceTokenVerifier};
use common::{app::AppWorld, TestResponse};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone)]
struct Seen {
    method: Method,
    path: String,
    query: Option<String>,
    body: Value,
    tenant: Uuid,
    application: Uuid,
    environment: Uuid,
    service: String,
    raw_headers: String,
}

#[derive(Clone)]
struct Mock {
    verifier: Arc<Mutex<Option<Arc<ServiceTokenVerifier>>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    reply: Arc<Mutex<(u16, Value, bool)>>,
    refused: Arc<Mutex<u32>>,
}

async fn handle(
    axum::extract::State(m): axum::extract::State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let verifier = m
        .verifier
        .lock()
        .unwrap()
        .clone()
        .expect("verifier installed");
    let verified = match verifier.verify_headers(&headers) {
        Ok(v) => v,
        Err(_) => {
            *m.refused.lock().unwrap() += 1;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": { "code": "INTERNAL_AUTHENTICATION_FAILED", "message": "no" } })),
            )
                .into_response();
        }
    };
    let delegated = verified
        .delegated_platform_context()
        .cloned()
        .expect("delegated scope");
    m.seen.lock().unwrap().push(Seen {
        method,
        path: uri.path().to_string(),
        query: uri.query().map(str::to_string),
        body: if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
        tenant: delegated.tenant_id(),
        application: delegated.application_id(),
        environment: delegated.application_environment_id(),
        service: verified.authenticated_service().as_str().to_string(),
        raw_headers: format!("{headers:?}"),
    });
    let (status, body, replayed) = m.reply.lock().unwrap().clone();
    let mut r = (StatusCode::from_u16(status).unwrap(), Json(body)).into_response();
    if replayed {
        r.headers_mut()
            .insert("idempotent-replayed", "true".parse().unwrap());
    }
    r
}

async fn mock() -> (Mock, String) {
    let m = Mock {
        verifier: Arc::new(Mutex::new(None)),
        seen: Arc::new(Mutex::new(Vec::new())),
        reply: Arc::new(Mutex::new((200, json!({ "ok": true }), false))),
        refused: Arc::new(Mutex::new(0)),
    };
    let app = Router::new().fallback(handle).with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (m, url)
}

const PRIVACY_SCOPES: [&str; 5] = [
    names::PRINCIPALS_READ,
    names::PRIVACY_CONTEXTS_READ,
    names::PRIVACY_CONTEXTS_WRITE,
    names::PRIVACY_SESSIONS_READ,
    names::PRIVACY_SESSIONS_WRITE,
];

/// A customer credential holding the four privacy scopes.
async fn privacy_token(w: &AppWorld) -> String {
    let t = w
        .h()
        .issue(w.w.consumer.sa(w.w.kind), &PRIVACY_SCOPES)
        .await;
    t.secret.expose_once().to_string()
}

async fn world() -> Option<(AppWorld, Mock, String)> {
    let (m, url) = mock().await;
    let w = AppWorld::with_privacy_relay_at(&url).await?;
    *m.verifier.lock().unwrap() = Some(w.h().peers.privacy_relay_verifier.clone());
    let t = privacy_token(&w).await;
    Some((w, m, t))
}

async fn call(
    w: &AppWorld,
    method: Method,
    path: &str,
    token: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> TestResponse {
    let headers: Vec<(&str, &str)> = key
        .map(|k| vec![("idempotency-key", k)])
        .unwrap_or_default();
    w.h()
        .request_with(method, path, Some(token), body, &headers)
        .await
}

/// A contract-valid CreateContextRequest naming both world principals.
fn context_body(w: &AppWorld) -> Value {
    json!({
        "externalContextReference": "order:42",
        "participants": [
            { "principalId": w.w.initiator.id() },
            { "principalId": w.w.recipient.id() },
        ],
        "allowedChannels": ["VOICE"],
        "directionPolicy": "EXPLICIT",
        "directions": [{ "from": { "principalId": w.w.initiator.id() }, "to": { "principalId": w.w.recipient.id() } }],
        "expiresAt": "2031-03-01T12:00:00Z",
    })
}

#[tokio::test]
async fn commands_forward_a_delegated_scope_and_never_the_customer_credential() {
    let Some((w, m, t)) = world().await else {
        return;
    };
    let created = json!({ "context": { "contextId": Uuid::new_v4(), "version": 1 } });
    *m.reply.lock().unwrap() = (201, created.clone(), false);
    let r = call(
        &w,
        Method::POST,
        "/v1/privacy-relay/contexts",
        &t,
        Some(context_body(&w)),
        Some("ctx-1"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json, created);
    assert_eq!(r.headers["cache-control"], "no-store");
    assert!(r.headers.get("idempotent-replayed").is_none());

    let seen = m.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!(
        (s.method.clone(), s.path.as_str()),
        (Method::POST, "/internal/v1/contexts")
    );
    assert_eq!(s.service, ServiceId::calling().as_str());
    assert_eq!(
        s.body,
        json!({ "idempotencyKey": "ctx-1", "request": context_body(&w) })
    );
    let scope = w.w.scope();
    assert_eq!(
        (s.tenant, s.application, s.environment),
        (
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid()
        )
    );
    // The customer credential (or any part of it) is never forwarded.
    assert!(!s.raw_headers.contains(&t));
    assert!(!s.raw_headers.contains(common::secret_part(&t)));
    assert!(!s
        .raw_headers
        .to_ascii_lowercase()
        .contains("\"authorization\""));
    assert!(!s.raw_headers.contains("cfmk_"));

    // A replay answer carries the replay header through.
    *m.reply.lock().unwrap() = (201, created.clone(), true);
    let r = call(
        &w,
        Method::POST,
        "/v1/privacy-relay/contexts",
        &t,
        Some(context_body(&w)),
        Some("ctx-1"),
    )
    .await;
    assert_eq!(r.headers["idempotent-replayed"], "true");

    // Every route maps to its internal operation.
    let id = Uuid::new_v4();
    let cases: Vec<(Method, String, Option<Value>, &str, Method)> = vec![
        (
            Method::GET,
            format!("/v1/privacy-relay/contexts/{id}"),
            None,
            "/internal/v1/contexts/ID",
            Method::GET,
        ),
        (
            Method::GET,
            format!("/v1/privacy-relay/contexts/{id}/evidence"),
            None,
            "/internal/v1/contexts/ID/evidence",
            Method::GET,
        ),
        (
            Method::PATCH,
            format!("/v1/privacy-relay/contexts/{id}"),
            Some(json!({ "expectedVersion": 1 })),
            "/internal/v1/contexts/ID/update",
            Method::POST,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/contexts/{id}/suspend"),
            Some(json!({ "expectedVersion": 1 })),
            "/internal/v1/contexts/ID/suspend",
            Method::POST,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/contexts/{id}/resume"),
            Some(json!({ "expectedVersion": 2 })),
            "/internal/v1/contexts/ID/resume",
            Method::POST,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/contexts/{id}/complete"),
            Some(json!({ "expectedVersion": 3 })),
            "/internal/v1/contexts/ID/complete",
            Method::POST,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/contexts/{id}/revoke"),
            Some(json!({ "expectedVersion": 3 })),
            "/internal/v1/contexts/ID/revoke",
            Method::POST,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/contexts/{id}/sessions"),
            Some(json!({})),
            "/internal/v1/contexts/ID/sessions",
            Method::POST,
        ),
        (
            Method::GET,
            format!("/v1/privacy-relay/sessions/{id}"),
            None,
            "/internal/v1/sessions/ID",
            Method::GET,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/sessions/{id}/suspend"),
            Some(json!({ "expectedVersion": 1 })),
            "/internal/v1/sessions/ID/suspend",
            Method::POST,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/sessions/{id}/resume"),
            Some(json!({ "expectedVersion": 2 })),
            "/internal/v1/sessions/ID/resume",
            Method::POST,
        ),
        (
            Method::POST,
            format!("/v1/privacy-relay/sessions/{id}/revoke"),
            Some(json!({ "expectedVersion": 2 })),
            "/internal/v1/sessions/ID/revoke",
            Method::POST,
        ),
    ];
    *m.reply.lock().unwrap() = (200, json!({ "ok": true }), false);
    for (method, path, body, internal, internal_method) in cases {
        m.seen.lock().unwrap().clear();
        let r = call(
            &w,
            method.clone(),
            &path,
            &t,
            body.clone(),
            body.as_ref().map(|_| "k-1"),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{method} {path}: {}", r.text);
        let s = m.seen.lock().unwrap()[0].clone();
        assert_eq!(s.path, internal.replace("ID", &id.to_string()), "{path}");
        assert_eq!(s.method, internal_method);
        if let Some(b) = body {
            assert_eq!(s.body, json!({ "idempotencyKey": "k-1", "request": b }));
        }
    }
}

#[tokio::test]
async fn the_edge_refuses_before_privacy_relay_is_asked() {
    let Some((w, m, t)) = world().await else {
        return;
    };
    let id = Uuid::new_v4();
    // Scopes: reads need *.read, commands *.write; neither is implied by calls scopes.
    let calls_only = w
        .h()
        .issue(
            w.w.consumer.sa(w.w.kind),
            &[names::CALLS_READ, names::CALLS_WRITE],
        )
        .await;
    let read_only = w
        .h()
        .issue(
            w.w.consumer.sa(w.w.kind),
            &[names::PRIVACY_CONTEXTS_READ, names::PRIVACY_SESSIONS_READ],
        )
        .await;
    let calls_only = calls_only.secret.expose_once().to_string();
    let read_only = read_only.secret.expose_once().to_string();
    for (token, method, path, body) in [
        (
            &calls_only,
            Method::GET,
            format!("/v1/privacy-relay/contexts/{id}"),
            None,
        ),
        (
            &calls_only,
            Method::GET,
            "/v1/privacy-relay/sessions".to_string(),
            None,
        ),
        (
            &read_only,
            Method::POST,
            "/v1/privacy-relay/contexts".to_string(),
            Some(context_body(&w)),
        ),
        (
            &read_only,
            Method::POST,
            format!("/v1/privacy-relay/contexts/{id}/sessions"),
            Some(json!({})),
        ),
        (
            &read_only,
            Method::POST,
            format!("/v1/privacy-relay/sessions/{id}/revoke"),
            Some(json!({ "expectedVersion": 1 })),
        ),
    ] {
        let r = call(&w, method, &path, token, body, Some("k")).await;
        assert_eq!(
            (r.status, r.json["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("INSUFFICIENT_SCOPE")),
            "{path}"
        );
    }
    // No credential at all.
    let r = w
        .h()
        .request(
            Method::GET,
            &format!("/v1/privacy-relay/contexts/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Idempotency-Key is required (and well-formed) on every mutation.
    for key in [None, Some("has space"), Some("")] {
        let r = call(
            &w,
            Method::POST,
            "/v1/privacy-relay/contexts",
            &t,
            Some(context_body(&w)),
            key,
        )
        .await;
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{key:?}");
    }
    // Bodies are JSON objects.
    let r = call(
        &w,
        Method::POST,
        &format!("/v1/privacy-relay/contexts/{id}/revoke"),
        &t,
        Some(json!([1])),
        Some("k"),
    )
    .await;
    assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    // Path ids are UUIDs: the resource's NOT_FOUND code without a round trip.
    let r = call(
        &w,
        Method::GET,
        "/v1/privacy-relay/contexts/not-a-uuid",
        &t,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("CONTEXT_NOT_FOUND"))
    );
    let r = call(
        &w,
        Method::GET,
        "/v1/privacy-relay/sessions/not-a-uuid",
        &t,
        None,
        None,
    )
    .await;
    assert_eq!(r.json["error"]["code"], "SESSION_NOT_FOUND");
    // Named principals must be ACTIVE principals of this application environment.
    let mut foreign = context_body(&w);
    foreign["participants"][1]["principalId"] = json!(Uuid::new_v4());
    let r = call(
        &w,
        Method::POST,
        "/v1/privacy-relay/contexts",
        &t,
        Some(foreign),
        Some("k2"),
    )
    .await;
    assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    let mut foreign_direction = context_body(&w);
    foreign_direction["directions"][0]["to"]["principalId"] = json!(Uuid::new_v4());
    let r = call(
        &w,
        Method::POST,
        "/v1/privacy-relay/contexts",
        &t,
        Some(foreign_direction),
        Some("k3"),
    )
    .await;
    assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    // List parameters are a closed set.
    for q in [
        "?phoneNumber=1",
        "?limit=abc",
        "?contextId=00000000-0000-0000-0000-000000000000",
    ] {
        let r = call(
            &w,
            Method::GET,
            &format!("/v1/privacy-relay/contexts{q}"),
            &t,
            None,
            None,
        )
        .await;
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{q}");
    }
    let r = call(
        &w,
        Method::GET,
        "/v1/privacy-relay/sessions?externalContextReference=x",
        &t,
        None,
        None,
    )
    .await;
    assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");

    assert!(
        m.seen.lock().unwrap().is_empty(),
        "nothing reached Privacy Relay"
    );

    // Accepted list parameters are forwarded as a query body.
    let r = call(
        &w,
        Method::GET,
        "/v1/privacy-relay/contexts?limit=5&status=ACTIVE&externalContextReference=order:42",
        &t,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let ctx = Uuid::new_v4();
    call(
        &w,
        Method::GET,
        &format!("/v1/privacy-relay/sessions?contextId={ctx}&cursor=abc"),
        &t,
        None,
        None,
    )
    .await;
    let seen = m.seen.lock().unwrap().clone();
    assert_eq!(
        (seen[0].path.as_str(), &seen[0].body),
        (
            "/internal/v1/contexts/query",
            &json!({ "limit": 5, "status": "ACTIVE", "externalContextReference": "order:42" })
        )
    );
    assert_eq!(
        (seen[1].path.as_str(), &seen[1].body),
        (
            "/internal/v1/sessions/query",
            &json!({ "contextId": ctx, "cursor": "abc" })
        )
    );
    assert!(seen.iter().all(|s| s.query.is_none()));
}

#[tokio::test]
async fn privacy_relay_errors_are_relayed_safely_and_outages_fail_closed() {
    let Some((w, m, t)) = world().await else {
        return;
    };
    let id = Uuid::new_v4();
    let path = format!("/v1/privacy-relay/contexts/{id}/revoke");
    // Public codes are relayed with Calling's request id.
    *m.reply.lock().unwrap() = (
        409,
        json!({ "error": { "code": "CONTEXT_VERSION_CONFLICT", "message": "The context changed.", "requestId": "req_other" } }),
        false,
    );
    let r = call(
        &w,
        Method::POST,
        &path,
        &t,
        Some(json!({ "expectedVersion": 1 })),
        Some("k"),
    )
    .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("CONTEXT_VERSION_CONFLICT"))
    );
    assert_eq!(r.json["error"]["message"], "The context changed.");
    assert_ne!(r.json["error"]["requestId"], "req_other");
    assert_eq!(
        r.json["error"]["requestId"].as_str(),
        r.headers.get("x-request-id").and_then(|v| v.to_str().ok())
    );
    // An unsafe message is replaced.
    *m.reply.lock().unwrap() = (
        422,
        json!({ "error": { "code": "CONTEXT_NOT_ACTIVE", "message": "line\nbreak" } }),
        false,
    );
    let r = call(
        &w,
        Method::POST,
        &path,
        &t,
        Some(json!({ "expectedVersion": 1 })),
        Some("k"),
    )
    .await;
    assert_eq!(
        r.json["error"]["message"],
        "The request could not be completed."
    );
    // Internal or unknown codes mean misconfiguration: 503, never the internal code.
    for code in [
        "INTERNAL_PERMISSION_DENIED",
        "INTERNAL_AUTHENTICATION_FAILED",
        "SOMETHING_NEW",
    ] {
        *m.reply.lock().unwrap() = (
            403,
            json!({ "error": { "code": code, "message": "x" } }),
            false,
        );
        let r = call(
            &w,
            Method::POST,
            &path,
            &t,
            Some(json!({ "expectedVersion": 1 })),
            Some("k"),
        )
        .await;
        assert_eq!(
            (r.status, r.json["error"]["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("SERVICE_UNAVAILABLE")),
            "{code}"
        );
    }
    // A Privacy Relay that does not trust this Calling: 503.
    let untrusting = {
        let (_, peers) = common::service_auth_fixture();
        peers.privacy_relay_verifier
    };
    *m.verifier.lock().unwrap() = Some(untrusting);
    let r = call(
        &w,
        Method::GET,
        &format!("/v1/privacy-relay/contexts/{id}"),
        &t,
        None,
        None,
    )
    .await;
    assert_eq!(r.json["error"]["code"], "SERVICE_UNAVAILABLE");
    assert_eq!(*m.refused.lock().unwrap(), 1);
}

#[tokio::test]
async fn an_unreachable_or_unconfigured_privacy_relay_is_service_unavailable() {
    // Nothing listens on this port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let Some(w) = AppWorld::with_privacy_relay_at(&url).await else {
        return;
    };
    let t = privacy_token(&w).await;
    let r = call(
        &w,
        Method::GET,
        &format!("/v1/privacy-relay/contexts/{}", Uuid::new_v4()),
        &t,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("SERVICE_UNAVAILABLE"))
    );
    // A deployment without Privacy Relay configured.
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let t = privacy_token(&w).await;
    let r = call(
        &w,
        Method::POST,
        "/v1/privacy-relay/contexts",
        &t,
        Some(context_body(&w)),
        Some("k"),
    )
    .await;
    assert_eq!(r.json["error"]["code"], "SERVICE_UNAVAILABLE");
}

//! ADR 0068 — the admin bridge's operator reads served by the Meeting Service
//! (`/internal/v1/operator/*`, contract `meeting-service/admin-operator/v1.openapi.json`).
//!
//! Only `cfmeet.admin-bridge` holding `account.administer` / `meeting.administer` and
//! naming an HQ staff member (`X-CF-Operator`) is answered. User search matches the email
//! address but never returns it; the detail masks it. Operator attribution from any other
//! caller is refused on every internal router (checked here on the notification owner API).

mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use backend::services::{admin_operator as op, notification_owner as owner};
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::*;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const BASE: &str = "/internal/v1/operator";

fn signer(id: ServiceId, kid: &str) -> (ServiceTokenSigner, ServiceSigningKey) {
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let public =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &key.to_pkcs8_pem()).unwrap();
    (
        ServiceTokenSigner::new(
            id,
            RuntimeEnvironment::Test,
            key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ),
        public,
    )
}

/// Receiver trust bundle of the Meeting Service for these tests.
fn verifier(issuers: &[(&str, &ServiceSigningKey, &[&str])]) -> Arc<ServiceTokenVerifier> {
    let trusted: Vec<Value> = issuers
        .iter()
        .map(|(id, k, perms)| {
            json!({ "issuer": id, "audience": "cfmeet.meeting-service", "runtime_environment": "test",
                    "keys": [k.public_jwk()], "permissions": perms })
        })
        .collect();
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.meeting-service",
                         "runtime_environment": "test", "trusted_issuers": trusted });
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)))
}

struct Harness {
    app: TestApp,
    router: Router,
    bridge: ServiceTokenSigner,
    /// A bridge identity granted only `meeting.administer`.
    meetings_only: Router,
    peer: ServiceTokenSigner,
}

impl Harness {
    async fn start() -> Option<Self> {
        let app = TestApp::new().await?;
        let (bridge, b_pub) = signer(ServiceId::admin_bridge(), "admin-bridge-test-1");
        let (peer, p_pub) = signer(ServiceId::organizations(), "organizations-test-1");
        let both: &[&str] = &["account.administer", "meeting.administer"];
        let router = op::router(
            app.state.clone(),
            verifier(&[
                ("cfmeet.admin-bridge", &b_pub, both),
                (
                    "cfmeet.organizations",
                    &p_pub,
                    &["meeting-organization-events.apply"],
                ),
            ]),
        );
        let meetings_only = op::router(
            app.state.clone(),
            verifier(&[("cfmeet.admin-bridge", &b_pub, &["meeting.administer"])]),
        );
        Some(Self {
            app,
            router,
            bridge,
            meetings_only,
            peer,
        })
    }
}

fn signed(signer: &ServiceTokenSigner, method: &Method, path: &str) -> Vec<(String, String)> {
    let bare = path.split('?').next().unwrap();
    signer
        .sign_request(
            &OutboundServiceRequest::new_operation(ServiceId::meeting_service()),
            method,
            bare,
        )
        .unwrap()
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
        .collect()
}

async fn send(
    router: &Router,
    method: Method,
    path: &str,
    headers: Vec<(String, String)>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        b = b.header(k, v);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = router.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A bridge read naming `operator`.
async fn get(
    h: &Harness,
    router: &Router,
    path: &str,
    operator: Option<&str>,
) -> (StatusCode, Value) {
    let mut headers = signed(&h.bridge, &Method::GET, path);
    if let Some(o) = operator {
        headers.push(("x-cf-operator".into(), o.into()));
    }
    send(router, Method::GET, path, headers, None).await
}

async fn bridge(h: &Harness, path: &str) -> (StatusCode, Value) {
    get(h, &h.router, path, Some("hq:staff-0001")).await
}

#[tokio::test]
async fn the_bridge_reads_accounts_and_never_gets_an_address() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let u = h
        .app
        .user_with_email("ada", "ada.lovelace@example.com")
        .await;

    // search by the exact address: found, the address never returned
    let (status, v) = bridge(
        &h,
        &format!("{BASE}/accounts/users?q=ADA.LOVELACE@example.com"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["userId"], json!(u.id));
    assert!(
        !v.to_string().contains("lovelace@"),
        "search never returns the address"
    );

    let (status, d) = bridge(&h, &format!("{BASE}/accounts/users/{}", u.id)).await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["emailMasked"], "a***@e***.com");
    assert!(!d.to_string().contains("lovelace@"));

    for suffix in ["sessions", "devices", "mfa", "security-events"] {
        let (status, v) = bridge(&h, &format!("{BASE}/accounts/users/{}/{suffix}", u.id)).await;
        assert_eq!(status, StatusCode::OK, "{suffix}: {v}");
    }
    let (_, mfa) = bridge(&h, &format!("{BASE}/accounts/users/{}/mfa", u.id)).await;
    assert_eq!(mfa["totp"]["state"], "NOT_ENROLLED");
    assert_eq!(mfa["recoveryCodesRemaining"], 0);

    let ghost = Uuid::new_v4();
    for path in [
        format!("{BASE}/accounts/users/{ghost}"),
        format!("{BASE}/accounts/users/{ghost}/sessions"),
        format!("{BASE}/accounts/users/{ghost}/devices"),
        format!("{BASE}/accounts/users/{ghost}/mfa"),
        format!("{BASE}/accounts/users/{ghost}/security-events"),
    ] {
        assert_eq!(bridge(&h, &path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn the_bridge_reads_meetings() {
    let Some(h) = Harness::start().await else {
        return;
    };
    for path in [
        format!("{BASE}/meetings/spaces?q=weekly"),
        format!("{BASE}/meetings/conferences?q=weekly"),
        format!("{BASE}/meetings/live"),
        format!("{BASE}/meetings/audit-events"),
    ] {
        let (status, v) = bridge(&h, &path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {v}");
        assert!(
            v["items"].is_array() && v.get("nextCursor").is_some(),
            "{path}"
        );
    }
    let ghost = Uuid::new_v4();
    assert_eq!(
        bridge(&h, &format!("{BASE}/meetings/conferences/{ghost}"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        bridge(
            &h,
            &format!("{BASE}/meetings/conferences/{ghost}/participants")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn operator_reads_need_the_permission_a_named_operator_and_a_token() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let users = format!("{BASE}/accounts/users?q=ada");
    // another service, no permission
    let mut headers = signed(&h.peer, &Method::GET, &users);
    headers.push(("x-cf-operator".into(), "hq:staff-0001".into()));
    let (status, v) = send(&h.router, Method::GET, &users, headers, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    // the bridge without account.administer
    let (status, v) = get(&h, &h.meetings_only, &users, Some("hq:staff-0001")).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("INTERNAL_PERMISSION_DENIED"))
    );
    // no operator, malformed operator
    let (status, v) = get(&h, &h.router, &users, None).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("OPERATOR_REQUIRED"))
    );
    let (status, _) = get(&h, &h.router, &users, Some("staff-0001")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // no token at all
    let (status, _) = send(
        &h.router,
        Method::GET,
        &users,
        vec![("x-cf-operator".into(), "hq:staff-0001".into())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn queries_are_validated() {
    let Some(h) = Harness::start().await else {
        return;
    };
    for path in [
        format!("{BASE}/accounts/users"),
        format!("{BASE}/accounts/users?q=ada&limit=0"),
        format!("{BASE}/accounts/users?q=ada&cursor=nope"),
        format!(
            "{BASE}/meetings/audit-events?since=2026-01-01T00:00:00Z&until=2026-03-01T00:00:00Z"
        ),
        format!("{BASE}/meetings/audit-events?cursor=12345"),
        format!("{BASE}/meetings/live?unknown=1"),
    ] {
        let (status, _) = bridge(&h, &path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
}

/// Operator attribution from any caller other than the bridge is refused on every internal
/// router: here the Notification Service owner API (before its body is even read).
#[tokio::test]
async fn foreign_operator_attribution_is_refused_on_other_internal_routes() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let (notifications, n_pub) = signer(ServiceId::notifications(), "notifications-test-1");
    let router = owner::router(
        h.app.state.clone(),
        verifier(&[("cfmeet.notifications", &n_pub, &["account-contacts.read"])]),
    );
    let path = owner::CONTACTS_PATH;
    let mut headers = signed(&notifications, &Method::POST, path);
    headers.push(("x-cf-operator".into(), "hq:staff-0001".into()));
    let (status, _) = send(
        &router,
        Method::POST,
        path,
        headers,
        Some(json!({"accountRefs": []})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

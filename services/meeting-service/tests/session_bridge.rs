//! Part VI §50 — the Developer Console session bridge
//! (`POST /internal/v1/user-sessions/verify`).
//!
//! Only the Developer Platform (`user-session.verify`) may verify a session; the Meeting
//! Service stays the session authority: revoked tokens, deactivated accounts, guest
//! tokens and garbage are REJECTED, and an unreachable revocation store fails CLOSED.

mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use backend::services::session_bridge;
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::*;
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use tower::ServiceExt;

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

fn verifier(dp: &ServiceSigningKey, calling: &ServiceSigningKey) -> Arc<ServiceTokenVerifier> {
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({
        "issuer": id, "audience": "cfmeet.meeting-service", "runtime_environment": "test",
        "keys": [k.public_jwk()], "permissions": perms })
    };
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.meeting-service", "runtime_environment": "test",
    "trusted_issuers": [
        issuer("cfmeet.developer-platform", dp, &["user-session.verify"]),
    ]});
    let _ = calling;
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)))
}

async fn verify(
    router: &axum::Router,
    signer: Option<&ServiceTokenSigner>,
    token: &str,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(session_bridge::VERIFY_PATH)
        .header("content-type", "application/json");
    if let Some(s) = signer {
        let signed = s
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::meeting_service(),
            ))
            .unwrap();
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
    }
    let r = router
        .clone()
        .oneshot(
            b.body(Body::from(json!({ "sessionToken": token }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_developer_platform_verifies_sessions_through_the_session_authority() {
    if !require_test_redis() {
        return;
    }
    let Some(app) = TestApp::new().await else {
        return;
    };
    let (dp, dp_pub) = signer(ServiceId::developer_platform(), "developer-platform-test-1");
    let (calling, calling_pub) = signer(ServiceId::calling(), "calling-test-1");
    let router = session_bridge::router(app.state.clone(), verifier(&dp_pub, &calling_pub));

    let alice = app.user("alice").await;
    let (s, v) = verify(&router, Some(&dp), &alice.token).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "VERIFIED");
    assert_eq!(v["userId"], alice.id.to_string());

    // Garbage, guest and revoked tokens are rejected.
    assert_eq!(
        verify(&router, Some(&dp), "not-a-jwt").await.1["outcome"],
        "REJECTED"
    );
    let guest = app
        .state
        .jwt_service
        .generate_access_token(uuid::Uuid::new_v4(), "guest")
        .unwrap();
    assert_eq!(
        verify(&router, Some(&dp), &guest).await.1["outcome"],
        "REJECTED"
    );
    let bob = app.user("bob").await;
    app.state
        .token_blacklist
        .blacklist_token(&bob.token, 900)
        .await
        .unwrap();
    assert_eq!(
        verify(&router, Some(&dp), &bob.token).await.1["outcome"],
        "REJECTED"
    );
    // A deactivated account is rejected.
    let carol = app.user("carol").await;
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "UPDATE users SET status = 'inactive' WHERE id = $1",
            [carol.id.into()],
        ))
        .await
        .unwrap();
    assert_eq!(
        verify(&router, Some(&dp), &carol.token).await.1["outcome"],
        "REJECTED"
    );

    // Only the Developer Platform is trusted for it (registry: grantable to it alone);
    // another service and a request without a service token are refused.
    assert_eq!(
        verify(&router, Some(&calling), &alice.token).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        verify(&router, None, &alice.token).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn an_unreachable_revocation_store_fails_closed() {
    let Some(app) = TestApp::with(TestOptions {
        redis: RedisMode::Unreachable,
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let (dp, dp_pub) = signer(ServiceId::developer_platform(), "developer-platform-test-1");
    let (_c, calling_pub) = signer(ServiceId::calling(), "calling-test-1");
    let router = session_bridge::router(app.state.clone(), verifier(&dp_pub, &calling_pub));
    let alice = app.user("alice").await;
    assert_eq!(
        verify(&router, Some(&dp), &alice.token).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

//! E. Authentication: the full authority chain is verified on every request, and every
//! failure looks the same to the client.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{
        ApplicationStatus, Clock, EnvironmentKind, EnvironmentStatus, ServiceAccountStatus,
        TenantClassification, TenantStatus,
    },
    platform::{
        authentication::{AuthFailureReason, AuthenticationError},
        context::{AuthorityMode, SuspendedLink},
        credentials::IssueCredential,
        scopes::names,
    },
};
use chrono::Duration;
use common::{Harness, TestResponse};
use serde_json::{json, Value};

async fn assert_rejected(h: &Harness, token: &str) -> TestResponse {
    let response = h.whoami(token).await;
    assert_eq!(
        response.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        response.text
    );
    assert_eq!(
        response.normalized_error(),
        json!({ "error": {
            "code": "AUTHENTICATION_FAILED",
            "message": "Authentication failed.",
            "requestId": null
        }})
    );
    assert_eq!(
        response.headers.get("www-authenticate").unwrap(),
        "Bearer realm=\"cf-meet-calling\""
    );
    // The response carries the server request id, in the body and the header.
    let request_id = response.json["error"]["requestId"].as_str().unwrap();
    assert!(request_id.starts_with("req_"));
    assert_eq!(response.headers.get("x-request-id").unwrap(), request_id);
    assert!(!response.text.contains(token));
    response
}

async fn reason(h: &Harness, token: &str) -> AuthFailureReason {
    match h.authenticate(token).await {
        Err(AuthenticationError::Rejected { reason, .. }) => reason,
        other => panic!("expected rejection, got {other:?}"),
    }
}

/// Part H §72: a SUSPENDED tenant / application / environment authenticates into a
/// safety-only authority (so a live call can still be ended) — every ordinary route is
/// refused with the normalized authentication failure.
async fn assert_safety_only(h: &Harness, token: &str, link: SuspendedLink) {
    let ctx = h.authenticate(token).await.expect("safety-only context");
    assert!(ctx.is_safety_only());
    assert_eq!(ctx.authority_mode(), AuthorityMode::SafetyOnly(link));
}

#[tokio::test]
async fn valid_credential_authenticates_with_persisted_context() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let response = h.whoami(issued.secret.expose_once()).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    let env = c.env(EnvironmentKind::Production);
    assert_eq!(
        response.json["tenant_id"],
        json!(c.tenant.id),
        "tenant from persistence"
    );
    assert_eq!(response.json["application_id"], json!(c.application.id));
    assert_eq!(response.json["environment_id"], json!(env.id));
    assert_eq!(response.json["environment_kind"], "PRODUCTION");
    assert_eq!(
        response.json["service_account_id"],
        json!(c.sa(EnvironmentKind::Production))
    );
    assert_eq!(response.json["credential_id"], json!(issued.credential.id));
    assert_eq!(response.json["scopes"], json!([names::PRINCIPALS_READ]));
    assert_eq!(response.json["tenant_classification"], "EXTERNAL");
    assert_eq!(response.json["live_telecom_allowed"], false);
    // The context carries the same server-generated request id the response reports.
    assert_eq!(
        response.headers.get("x-request-id").unwrap(),
        response.json["request_id"].as_str().unwrap()
    );
}

#[tokio::test]
async fn invalid_secret_unknown_id_and_malformed_values_fail() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once().to_string();

    // Wrong secret (one hex digit changed).
    let last = token.chars().last().unwrap();
    let flipped = format!(
        "{}{}",
        &token[..token.len() - 1],
        if last == '0' { '1' } else { '0' }
    );
    assert_rejected(&h, &flipped).await;
    assert_eq!(
        reason(&h, &flipped).await,
        AuthFailureReason::SecretMismatch
    );

    // Unknown public id with a well-formed secret.
    let unknown = format!(
        "cfmk_live_{}_{}",
        "ab".repeat(12),
        &token[token.len() - 64..]
    );
    assert_rejected(&h, &unknown).await;
    assert_eq!(
        reason(&h, &unknown).await,
        AuthFailureReason::UnknownCredential
    );

    // Malformed values.
    for bad in [
        "x",
        "cfmk_live_",
        "cfmk_live_zz_zz",
        &token[..token.len() - 1],
    ] {
        assert_rejected(&h, bad).await;
        assert_eq!(
            reason(&h, bad).await,
            AuthFailureReason::MalformedCredential
        );
    }

    // Missing header / wrong scheme / token in another header.
    let missing = h
        .request(Method::POST, "/test/context", None, Some(json!({})))
        .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    for headers in [
        vec![("authorization", format!("Basic {token}"))],
        vec![("authorization", token.clone())],
        vec![("api-key", token.clone())],
        vec![("x-api-key", token.clone())],
    ] {
        let headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let response = h
            .request_with(
                Method::POST,
                "/test/context",
                None,
                Some(json!({})),
                &headers,
            )
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
    // A credential in the query string is not accepted either.
    let query = h
        .request(
            Method::POST,
            &format!("/test/context?access_token={token}"),
            None,
            Some(json!({})),
        )
        .await;
    assert_eq!(query.status, StatusCode::UNAUTHORIZED);

    // The valid credential still works.
    assert_eq!(h.whoami(&token).await.status, StatusCode::OK);
}

#[tokio::test]
async fn expired_and_not_yet_valid_credentials_fail() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let now = h.clock.now();
    let short = h
        .platform
        .issue_credential(
            c.sa(EnvironmentKind::Production),
            IssueCredential {
                expires_at: Some(now + Duration::minutes(10)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let future = h
        .platform
        .issue_credential(
            c.sa(EnvironmentKind::Production),
            IssueCredential {
                not_before: Some(now + Duration::minutes(5)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let short_token = short.secret.expose_once();
    let future_token = future.secret.expose_once();

    assert_eq!(h.whoami(short_token).await.status, StatusCode::OK);
    assert_rejected(&h, future_token).await;
    assert_eq!(
        reason(&h, future_token).await,
        AuthFailureReason::CredentialNotYetValid
    );

    h.clock.advance(Duration::minutes(5));
    assert_eq!(h.whoami(future_token).await.status, StatusCode::OK);

    h.clock.advance(Duration::minutes(5));
    assert_rejected(&h, short_token).await;
    assert_eq!(
        reason(&h, short_token).await,
        AuthFailureReason::CredentialExpired
    );
}

#[tokio::test]
async fn revoked_credential_fails_immediately_and_revocation_is_idempotent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once();
    assert_eq!(h.whoami(token).await.status, StatusCode::OK);

    let revoked = h
        .platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    assert_eq!(revoked.status.as_str(), "REVOKED");
    let revoked_at = revoked.revoked_at.unwrap();
    // The very next request is refused: no cache.
    assert_rejected(&h, token).await;
    assert_eq!(
        reason(&h, token).await,
        AuthFailureReason::CredentialRevoked
    );

    // Idempotent: the second revocation changes nothing.
    h.clock.advance(Duration::minutes(1));
    let again = h
        .platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    assert_eq!(again.revoked_at, Some(revoked_at));
    assert_rejected(&h, token).await;

    // And the database refuses to un-revoke.
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_api_credentials SET status = 'ACTIVE', revoked_at = NULL WHERE id = '{}'",
            issued.credential.id
        ),
    )
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn every_inactive_link_of_the_authority_chain_fails_authentication() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once();
    let sa = c.sa(EnvironmentKind::Production);
    let env = c.env(EnvironmentKind::Production).id;
    let p = &h.platform;

    let mut bodies: Vec<Value> = Vec::new();

    p.set_service_account_status(sa, ServiceAccountStatus::Suspended)
        .await
        .unwrap();
    bodies.push(assert_rejected(&h, token).await.normalized_error());
    assert_eq!(
        reason(&h, token).await,
        AuthFailureReason::ServiceAccountInactive
    );
    p.set_service_account_status(sa, ServiceAccountStatus::Active)
        .await
        .unwrap();
    assert_eq!(h.whoami(token).await.status, StatusCode::OK);

    p.set_environment_status(env, EnvironmentStatus::Suspended)
        .await
        .unwrap();
    bodies.push(assert_rejected(&h, token).await.normalized_error());
    assert_safety_only(&h, token, SuspendedLink::Environment).await;
    p.set_environment_status(env, EnvironmentStatus::Active)
        .await
        .unwrap();
    assert_eq!(h.whoami(token).await.status, StatusCode::OK);

    p.set_application_status(c.application.id, ApplicationStatus::Suspended)
        .await
        .unwrap();
    bodies.push(assert_rejected(&h, token).await.normalized_error());
    assert_safety_only(&h, token, SuspendedLink::Application).await;
    p.set_application_status(c.application.id, ApplicationStatus::Active)
        .await
        .unwrap();
    assert_eq!(h.whoami(token).await.status, StatusCode::OK);

    p.set_tenant_status(c.tenant.id, TenantStatus::Suspended)
        .await
        .unwrap();
    bodies.push(assert_rejected(&h, token).await.normalized_error());
    assert_safety_only(&h, token, SuspendedLink::Tenant).await;
    p.set_tenant_status(c.tenant.id, TenantStatus::Active)
        .await
        .unwrap();
    assert_eq!(h.whoami(token).await.status, StatusCode::OK);

    // Revoked service account: terminal.
    p.set_service_account_status(sa, ServiceAccountStatus::Revoked)
        .await
        .unwrap();
    bodies.push(assert_rejected(&h, token).await.normalized_error());
    assert!(p
        .set_service_account_status(sa, ServiceAccountStatus::Active)
        .await
        .is_err());
    assert_rejected(&h, token).await;

    // Every failure is indistinguishable to the client.
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
}

#[tokio::test]
async fn suspended_tenant_blocks_every_subordinate_credential() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let second_app = h.application_in(&c.tenant, "second").await;
    let mut tokens = Vec::new();
    for consumer in [&c, &second_app] {
        for kind in EnvironmentKind::ALL {
            tokens.push(h.issue(consumer.sa(*kind), &[]).await);
        }
    }
    for t in &tokens {
        assert_eq!(
            h.whoami(t.secret.expose_once()).await.status,
            StatusCode::OK
        );
    }
    h.platform
        .set_tenant_status(c.tenant.id, TenantStatus::Suspended)
        .await
        .unwrap();
    for t in &tokens {
        assert_rejected(&h, t.secret.expose_once()).await;
    }
}

#[tokio::test]
async fn verifier_is_bound_to_the_deployment_pepper() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let other = h.sibling("another-deployment-pepper-99f0c3b8e1d24a6f8c7b5e3d2a1f0e9d");
    assert_eq!(
        match other.authenticate(issued.secret.expose_once()).await {
            Err(AuthenticationError::Rejected { reason, .. }) => reason,
            other => panic!("{other:?}"),
        },
        AuthFailureReason::SecretMismatch
    );
    assert_eq!(
        h.whoami(issued.secret.expose_once()).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn issuance_requires_an_active_authority_chain() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let sa = c.sa(EnvironmentKind::Production);
    h.platform
        .set_tenant_status(c.tenant.id, TenantStatus::Suspended)
        .await
        .unwrap();
    let err = h
        .platform
        .issue_credential(sa, IssueCredential::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, calling::platform::PlatformError::InvalidState(_)),
        "{err}"
    );
}

#[tokio::test]
async fn health_is_public_and_unauthenticated_routes_do_not_exist() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let health = h.request(Method::GET, "/health", None, None).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.json["status"], "ok");
    let missing = h.request(Method::GET, "/whoami", None, None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json["error"]["code"], "NOT_FOUND");
}

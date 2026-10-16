//! F. Environment isolation: the environment of a request is the persisted environment of
//! its credential. Payloads, headers and edited prefixes cannot change it.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, TenantClassification},
    platform::{
        authentication::{AuthFailureReason, AuthenticationError},
        credentials::CredentialFilter,
        PlatformError,
    },
};
use common::Harness;
use serde_json::json;

#[tokio::test]
async fn each_environment_credential_authenticates_only_as_its_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    for kind in EnvironmentKind::ALL {
        let issued = h.issue(c.sa(*kind), &[]).await;
        assert!(issued
            .secret
            .expose_once()
            .starts_with(&format!("cfmk_{}_", kind.credential_tag())));
        let ctx = h.ctx(issued.secret.expose_once()).await;
        assert_eq!(ctx.environment_kind(), *kind);
        assert_eq!(ctx.environment_id(), c.env(*kind).id);
    }
}

#[tokio::test]
async fn production_credential_cannot_become_sandbox_through_the_request() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let other = h.consumer("other", TenantClassification::FirstParty).await;
    let prod = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let sandbox = c.env(EnvironmentKind::Sandbox);

    let response = h
        .request_with(
            Method::POST,
            "/test/context",
            Some(prod.secret.expose_once()),
            Some(json!({
                "environment": "sandbox",
                "environment_kind": "SANDBOX",
                "environment_id": sandbox.id,
                "tenant_id": other.tenant.id,
                "application_id": other.application.id,
                "service_account_id": other.sa(EnvironmentKind::Sandbox),
                "credential_id": uuid::Uuid::new_v4(),
                "scopes": ["calls.write", "events.private_delivery"],
                "live_telecom_allowed": true,
            })),
            &[
                ("x-environment", "sandbox"),
                ("x-tenant-id", &other.tenant.id.to_string()),
                ("x-scopes", "calls.write"),
            ],
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json["environment_kind"], "PRODUCTION");
    assert_eq!(
        response.json["environment_id"],
        json!(c.env(EnvironmentKind::Production).id)
    );
    assert_eq!(response.json["tenant_id"], json!(c.tenant.id));
    assert_eq!(response.json["application_id"], json!(c.application.id));
    assert_eq!(response.json["credential_id"], json!(prod.credential.id));
    assert_eq!(response.json["scopes"], json!([]));
    assert_eq!(response.json["tenant_classification"], "EXTERNAL");
}

#[tokio::test]
async fn sandbox_credential_cannot_become_production_through_the_request() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let sandbox = h.issue(c.sa(EnvironmentKind::Sandbox), &[]).await;
    let response = h
        .request_with(
            Method::POST,
            "/test/context?environment=production",
            Some(sandbox.secret.expose_once()),
            Some(json!({
                "environment": "production",
                "environment_id": c.env(EnvironmentKind::Production).id,
                "live_telecom_allowed": true,
            })),
            &[("x-environment", "production")],
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json["environment_kind"], "SANDBOX");
    assert_eq!(
        response.json["environment_id"],
        json!(c.env(EnvironmentKind::Sandbox).id)
    );
    assert_eq!(response.json["live_telecom_allowed"], false);
}

#[tokio::test]
async fn editing_the_environment_prefix_never_switches_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let prod = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let sandbox = h.issue(c.sa(EnvironmentKind::Sandbox), &[]).await;
    let test = h.issue(c.sa(EnvironmentKind::Test), &[]).await;

    let cases = [
        prod.secret
            .expose_once()
            .replacen("cfmk_live_", "cfmk_sbx_", 1),
        prod.secret
            .expose_once()
            .replacen("cfmk_live_", "cfmk_test_", 1),
        sandbox
            .secret
            .expose_once()
            .replacen("cfmk_sbx_", "cfmk_live_", 1),
        test.secret
            .expose_once()
            .replacen("cfmk_test_", "cfmk_live_", 1),
    ];
    for edited in &cases {
        assert_eq!(h.whoami(edited).await.status, StatusCode::UNAUTHORIZED);
        // The tag is bound into the verifier: an edited tag never verifies.
        match h.authenticate(edited).await {
            Err(AuthenticationError::Rejected { reason, .. }) => {
                assert_eq!(reason, AuthFailureReason::SecretMismatch)
            }
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn environments_of_one_application_have_isolated_credentials() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let mut seen = Vec::new();
    let mut sandbox_credential = None;
    for kind in EnvironmentKind::ALL {
        let issued = h.issue(c.sa(*kind), &[]).await;
        if *kind == EnvironmentKind::Sandbox {
            sandbox_credential = Some(issued.credential.id);
        }
        let listed = h
            .platform
            .list_credentials(CredentialFilter::Environment(c.env(*kind).id))
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, issued.credential.id);
        assert_eq!(listed[0].environment_kind, *kind);
        seen.push(issued.credential.id);
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3);

    // A service account can never be moved to another environment.
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_service_accounts SET environment_id = '{}' WHERE id = '{}'",
            c.env(EnvironmentKind::Production).id,
            c.sa(EnvironmentKind::Sandbox)
        ),
    )
    .await;
    assert!(err.is_err());
    // Nor can a sandbox credential be re-bound to the production environment.
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_api_credentials SET environment_id = '{}' WHERE id = '{}'",
            c.env(EnvironmentKind::Production).id,
            sandbox_credential.unwrap()
        ),
    )
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn non_production_environments_can_never_allow_live_telecom() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    for kind in [EnvironmentKind::Sandbox, EnvironmentKind::Test] {
        let env = c.env(kind);
        assert!(!env.live_telecom_allowed);
        let err = h
            .platform
            .set_live_telecom_allowed(env.id, true)
            .await
            .unwrap_err();
        assert!(matches!(err, PlatformError::PolicyViolation(_)));
        // The database refuses it too.
        let raw = common::exec(
            h.conn(),
            &format!(
                "UPDATE platform_environments SET live_telecom_allowed = TRUE WHERE id = '{}'",
                env.id
            ),
        )
        .await;
        assert!(raw.is_err());
    }

    // Production defaults to disabled and is enabled only explicitly.
    let prod = c.env(EnvironmentKind::Production);
    assert!(!prod.live_telecom_allowed);
    let token = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    assert!(!h
        .ctx(token.secret.expose_once())
        .await
        .live_telecom_allowed());
    h.platform
        .set_live_telecom_allowed(prod.id, true)
        .await
        .unwrap();
    assert!(h
        .ctx(token.secret.expose_once())
        .await
        .live_telecom_allowed());
    let sandbox = h.issue(c.sa(EnvironmentKind::Sandbox), &[]).await;
    assert!(!h
        .ctx(sandbox.secret.expose_once())
        .await
        .live_telecom_allowed());

    // Environment kind is immutable: sandbox cannot be turned into production.
    let raw = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_environments SET kind = 'PRODUCTION' WHERE id = '{}'",
            c.env(EnvironmentKind::Sandbox).id
        ),
    )
    .await;
    assert!(raw.is_err());
}

#[tokio::test]
async fn one_environment_of_each_kind_per_application() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let err = h
        .platform
        .create_environment(c.application.id, EnvironmentKind::Production)
        .await
        .unwrap_err();
    assert!(matches!(err, PlatformError::Conflict(_)), "{err}");
}

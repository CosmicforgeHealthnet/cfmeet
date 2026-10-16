//! G. Scopes are server-authoritative: registry-validated, a subset of the application's
//! allowed scopes, and never extendable by a request.

mod common;

use common::{IssueCredential, RotateCredential};

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, TenantClassification},
    platform::{scopes::names, PlatformError},
};
use common::Harness;
use serde_json::json;

#[tokio::test]
async fn scope_check_passes_with_scope_and_fails_without() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let with = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let without = h.issue(c.sa(EnvironmentKind::Production), &[]).await;

    let ok = h
        .request(
            Method::GET,
            "/test/principals-read",
            Some(with.secret.expose_once()),
            None,
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK);

    let denied = h
        .request(
            Method::GET,
            "/test/principals-read",
            Some(without.secret.expose_once()),
            None,
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.json["error"]["code"], "INSUFFICIENT_SCOPE");
    assert_eq!(
        denied.headers.get("www-authenticate").unwrap(),
        "Bearer realm=\"cf-meet-calling\", error=\"insufficient_scope\", scope=\"principals.read\""
    );

    // Domain-level check used by future APIs.
    let ctx = h.ctx(with.secret.expose_once()).await;
    assert!(ctx.require_scope(names::PRINCIPALS_READ).is_ok());
    assert!(ctx.require_scope(names::CALLS_WRITE).is_err());
    assert!(ctx.require_scope("not.a.registered.scope").is_err());
}

#[tokio::test]
async fn credentials_cannot_receive_scopes_outside_the_application_allowed_set() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let tenant = h
        .platform
        .create_tenant(crate::common::NewTenant {
            slug: "acme".into(),
            display_name: "Acme".into(),
            classification: TenantClassification::External,
            billing_currency: None,
        })
        .await
        .unwrap();
    let app = h
        .platform
        .create_application(
            tenant.id,
            crate::common::NewApplication {
                slug: "app".into(),
                display_name: "App".into(),
            },
        )
        .await
        .unwrap();
    h.platform
        .grant_application_scope(app.id, names::PRINCIPALS_READ)
        .await
        .unwrap();
    let env = h
        .platform
        .create_environment(app.id, EnvironmentKind::Production)
        .await
        .unwrap();
    let sa = h
        .platform
        .create_service_account(
            env.id,
            crate::common::NewServiceAccount {
                name: "backend".into(),
                description: None,
            },
        )
        .await
        .unwrap();

    let not_allowed = h
        .platform
        .issue_credential(
            sa.id,
            IssueCredential {
                scopes: vec![names::PRINCIPALS_READ.into(), names::CALLS_WRITE.into()],
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(not_allowed, PlatformError::PolicyViolation(_)),
        "{not_allowed}"
    );

    let unknown = h
        .platform
        .issue_credential(
            sa.id,
            IssueCredential {
                scopes: vec!["admin.everything".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(unknown, PlatformError::Validation(_)), "{unknown}");

    // Granting an unknown scope to the application is also refused.
    assert!(h
        .platform
        .grant_application_scope(app.id, "made.up")
        .await
        .is_err());

    // The database enforces the subset rule independently of the service.
    let issued = h
        .platform
        .issue_credential(
            sa.id,
            IssueCredential {
                scopes: vec![names::PRINCIPALS_READ.into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let raw = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_api_credential_scopes (credential_id, tenant_id, application_id, scope) \
             VALUES ('{}', '{}', '{}', 'calls.write')",
            issued.credential.id, tenant.id, app.id
        ),
    )
    .await;
    assert!(
        raw.is_err(),
        "subset rule must be enforced by a foreign key"
    );
}

#[tokio::test]
async fn client_supplied_scopes_do_nothing() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let token = issued.secret.expose_once();

    let response = h
        .request_with(
            Method::POST,
            "/test/context?scope=calls.write",
            Some(token),
            Some(json!({ "scopes": [names::CALLS_WRITE, names::EVENTS_PRIVATE_DELIVERY], "scope": "calls.write" })),
            &[("x-scopes", "calls.write"), ("x-oauth-scopes", "calls.write")],
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json["scopes"], json!([names::PRINCIPALS_READ]));

    let denied = h
        .request_with(
            Method::GET,
            "/test/calls-write?scope=calls.write",
            Some(token),
            None,
            &[("x-scopes", "calls.write")],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn withdrawing_an_application_scope_removes_it_from_credentials_immediately() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(
            c.sa(EnvironmentKind::Production),
            &[names::PRINCIPALS_READ, names::CALLS_WRITE],
        )
        .await;
    let token = issued.secret.expose_once();
    let ok = h
        .request(Method::GET, "/test/calls-write", Some(token), None)
        .await;
    assert_eq!(ok.status, StatusCode::OK);

    assert!(h
        .platform
        .revoke_application_scope(c.application.id, names::CALLS_WRITE)
        .await
        .unwrap());
    let denied = h
        .request(Method::GET, "/test/calls-write", Some(token), None)
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let view = h.platform.credential(issued.credential.id).await.unwrap();
    assert_eq!(view.scopes, vec![names::PRINCIPALS_READ.to_string()]);
    // Idempotent.
    assert!(!h
        .platform
        .revoke_application_scope(c.application.id, names::CALLS_WRITE)
        .await
        .unwrap());
}

#[tokio::test]
async fn rotation_revalidates_scopes_against_the_application() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let err = h
        .platform
        .rotate_credential(
            issued.credential.id,
            RotateCredential {
                scopes: Some(vec![names::EVENTS_PRIVATE_DELIVERY.into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PlatformError::PolicyViolation(_)), "{err}");
    // The failed rotation left the credential untouched.
    let view = h.platform.credential(issued.credential.id).await.unwrap();
    assert!(view.superseded_by_credential_id.is_none());
    assert_eq!(
        h.whoami(issued.secret.expose_once()).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn calling_scopes_are_registered_by_platform_access() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let registered = h.platform.pa.registered_scopes().await;
    for scope in names::ALL {
        assert!(
            registered.iter().any(|r| r == scope),
            "{scope} is enforced by Calling but not registered by Platform Access"
        );
    }
}

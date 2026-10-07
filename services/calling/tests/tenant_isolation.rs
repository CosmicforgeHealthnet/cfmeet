//! B. Tenant isolation: ownership chains are consistent and immutable at the database
//! level, and principal resolution never crosses tenant / application / environment.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, PrincipalKind, TenantClassification},
    platform::principals::{NewPrincipal, PrincipalReference, PrincipalResolutionError},
};
use common::Harness;
use serde_json::json;

#[tokio::test]
async fn children_cannot_point_at_parents_of_another_tenant() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;

    // Domain API: the parent is the only input; ownership is derived from it.
    assert_eq!(a.application.tenant_id, a.tenant.id);
    let env = a.env(EnvironmentKind::Production);
    assert_eq!(
        (env.tenant_id, env.application_id),
        (a.tenant.id, a.application.id)
    );

    // Environment claiming tenant B for application A.
    let err = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_environments (tenant_id, application_id, kind) VALUES ('{}', '{}', 'SANDBOX')",
            b.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err(), "environment crossed tenants");

    // Application scopes claiming another tenant.
    let err = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_application_scopes (tenant_id, application_id, scope) VALUES ('{}', '{}', 'calls.read')",
            b.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err());

    // An application can never be re-parented.
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_applications SET tenant_id = '{}' WHERE id = '{}'",
            b.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err(), "application re-parented");
}

#[tokio::test]
async fn service_accounts_cannot_cross_application_or_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let a2 = h.application_in(&a.tenant, "second").await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;
    let env_a = a.env(EnvironmentKind::Production).id;

    for (tenant, application, environment, case) in [
        (
            a.tenant.id,
            a2.application.id,
            env_a,
            "other application, same tenant",
        ),
        (b.tenant.id, a.application.id, env_a, "other tenant"),
        (
            a.tenant.id,
            a.application.id,
            b.env(EnvironmentKind::Production).id,
            "foreign environment",
        ),
    ] {
        let err = common::exec(
            h.conn(),
            &format!(
                "INSERT INTO platform_service_accounts (tenant_id, application_id, environment_id, name) \
                 VALUES ('{tenant}', '{application}', '{environment}', 'intruder')"
            ),
        )
        .await;
        assert!(err.is_err(), "service account accepted: {case}");
    }

    // Orphans are impossible.
    let err = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_service_accounts (tenant_id, application_id, environment_id, name) \
             VALUES ('{}', '{}', gen_random_uuid(), 'orphan')",
            a.tenant.id, a.application.id
        ),
    )
    .await;
    assert!(err.is_err());

    // Credentials cannot be attached to a service account through a mismatched chain.
    let issued = h.issue(a.sa(EnvironmentKind::Production), &[]).await;
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_api_credentials SET service_account_id = '{}' WHERE id = '{}'",
            a.sa(EnvironmentKind::Sandbox),
            issued.credential.id
        ),
    )
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn principal_resolution_never_crosses_tenant_application_or_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let a2 = h.application_in(&a.tenant, "second").await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;

    let (principal, _) = h
        .platform
        .register_principal(
            a.env(EnvironmentKind::Production).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "user-1".into(),
            },
        )
        .await
        .unwrap();
    // Tenant B has its own, unrelated "user-1".
    let (b_principal, _) = h
        .platform
        .register_principal(
            b.env(EnvironmentKind::Production).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "user-1".into(),
            },
        )
        .await
        .unwrap();
    assert_ne!(principal.id, b_principal.id);

    let read = &["principals.read"];
    let own = h.issue(a.sa(EnvironmentKind::Production), read).await;
    let other_tenant = h.issue(b.sa(EnvironmentKind::Production), read).await;
    let other_app = h.issue(a2.sa(EnvironmentKind::Production), read).await;
    let other_env = h.issue(a.sa(EnvironmentKind::Sandbox), read).await;

    let by_id = PrincipalReference::Id(principal.id);
    let by_ref = PrincipalReference::External {
        kind: PrincipalKind::ExternalApplicationPrincipal,
        external_reference: "user-1".into(),
    };

    // Own scope resolves.
    let ctx = h.ctx(own.secret.expose_once()).await;
    assert_eq!(
        h.state.principals.resolve(&ctx, &by_id).await.unwrap().id(),
        principal.id
    );
    assert_eq!(
        h.state
            .principals
            .resolve(&ctx, &by_ref)
            .await
            .unwrap()
            .id(),
        principal.id
    );

    // Foreign scopes: by id → not found; by reference → the foreign scope's own principal
    // (or not found), never tenant A's.
    for (token, own_ref) in [
        (&other_tenant, Some(b_principal.id)),
        (&other_app, None),
        (&other_env, None),
    ] {
        let ctx = h.ctx(token.secret.expose_once()).await;
        assert!(matches!(
            h.state.principals.resolve(&ctx, &by_id).await,
            Err(PrincipalResolutionError::NotFound)
        ));
        match (h.state.principals.resolve(&ctx, &by_ref).await, own_ref) {
            (Ok(p), Some(expected)) => {
                assert_eq!(p.id(), expected);
                assert_eq!(p.tenant_id(), ctx.tenant_id());
            }
            (Err(PrincipalResolutionError::NotFound), None) => {}
            (other, _) => panic!("unexpected resolution: {other:?}"),
        }
    }

    // Over HTTP a foreign principal is indistinguishable from a nonexistent one.
    let foreign = h
        .request(
            Method::POST,
            "/test/resolve",
            Some(other_tenant.secret.expose_once()),
            Some(json!({ "id": principal.id })),
        )
        .await;
    let nonexistent = h
        .request(
            Method::POST,
            "/test/resolve",
            Some(other_tenant.secret.expose_once()),
            Some(json!({ "id": uuid::Uuid::new_v4() })),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(nonexistent.status, StatusCode::NOT_FOUND);
    assert_eq!(foreign.normalized_error(), nonexistent.normalized_error());
}

#[tokio::test]
async fn principal_ownership_is_immutable_and_scoped_unique() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("tenant-a", TenantClassification::External).await;
    let b = h.consumer("tenant-b", TenantClassification::External).await;
    let (p, _) = h
        .platform
        .register_principal(
            a.env(EnvironmentKind::Production).id,
            NewPrincipal::External {
                kind: PrincipalKind::NativeCfUser,
                external_reference: "subject-9".into(),
            },
        )
        .await
        .unwrap();
    let err = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_principals SET tenant_id = '{}', application_id = '{}', environment_id = '{}' WHERE id = '{}'",
            b.tenant.id,
            b.application.id,
            b.env(EnvironmentKind::Production).id,
            p.id
        ),
    )
    .await;
    assert!(err.is_err());
}

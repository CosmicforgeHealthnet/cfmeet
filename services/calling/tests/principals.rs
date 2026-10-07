//! J. All principal categories are representable.
//! K. API client identity and communication principal identity are separate.
//! L. Telephone endpoints carry only opaque references.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{
        EnvironmentKind, PrincipalKind, PrincipalStatus, ServiceAccountStatus, TenantClassification,
    },
    platform::{
        principals::{NewPrincipal, PrincipalReference, PrincipalResolutionError},
        scopes::names,
        PlatformError,
    },
};
use common::{scalar_i64, Harness};
use serde_json::json;

const CONTACT_HANDLE: &str = "6f1c2b9e-3a4d-4c5e-8f70-112233445566";

#[tokio::test]
async fn every_principal_category_is_representable_and_resolvable() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let env = c.env(EnvironmentKind::Production).id;
    let token = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;

    let mut registered = Vec::new();
    for (kind, reference) in [
        (PrincipalKind::NativeCfUser, "cfuser:5b0f6a"),
        (
            PrincipalKind::ExternalApplicationPrincipal,
            "customer-user-77",
        ),
        (PrincipalKind::EnterpriseMember, "member:ent-1:u-3"),
        (PrincipalKind::TelephoneEndpoint, CONTACT_HANDLE),
        (
            PrincipalKind::TemporaryExternalParticipant,
            "guest-session-abc",
        ),
        (PrincipalKind::BotOrService, "bot:transcriber"),
    ] {
        let (p, created) = h
            .platform
            .register_principal(
                env,
                NewPrincipal::External {
                    kind,
                    external_reference: reference.into(),
                },
            )
            .await
            .unwrap();
        assert!(created);
        assert_eq!(p.kind, kind);
        registered.push(p);
    }
    let (sa_principal, created) = h
        .platform
        .register_principal(
            env,
            NewPrincipal::ServiceAccount(c.sa(EnvironmentKind::Production)),
        )
        .await
        .unwrap();
    assert!(created);
    registered.push(sa_principal);

    let listed = h.platform.list_principals(env).await.unwrap();
    let mut kinds: Vec<_> = listed.iter().map(|p| p.kind).collect();
    kinds.sort();
    let mut all = PrincipalKind::ALL.to_vec();
    all.sort();
    assert_eq!(kinds, all, "all seven categories are represented");

    for p in &registered {
        let response = h
            .request(
                Method::POST,
                "/test/resolve",
                Some(token.secret.expose_once()),
                Some(json!({ "kind": p.kind, "reference": p.external_reference })),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text);
        assert_eq!(response.json["id"], json!(p.id));
        assert_eq!(response.json["kind"], json!(p.kind));
    }

    // Registration is idempotent.
    let (again, created) = h
        .platform
        .register_principal(
            env,
            NewPrincipal::External {
                kind: PrincipalKind::BotOrService,
                external_reference: "bot:transcriber".into(),
            },
        )
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(again.id, registered[5].id);
}

#[tokio::test]
async fn authentication_does_not_make_the_service_account_a_participant() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let env = c.env(EnvironmentKind::Production).id;
    let sa = c.sa(EnvironmentKind::Production);
    let credential = h.issue(sa, &[names::PRINCIPALS_READ]).await;

    let before = scalar_i64(h.conn(), "SELECT count(*) FROM platform_principals").await;
    let ctx = h.ctx(credential.secret.expose_once()).await;
    let after = scalar_i64(h.conn(), "SELECT count(*) FROM platform_principals").await;
    assert_eq!(before, after, "authentication must not create principals");

    // The authenticated service account is not implicitly a principal.
    let as_sa = PrincipalReference::External {
        kind: PrincipalKind::ServiceAccount,
        external_reference: sa.to_string(),
    };
    assert!(matches!(
        h.state.principals.resolve(&ctx, &as_sa).await,
        Err(PrincipalResolutionError::NotFound)
    ));

    // A participant resolved for this request is whoever the reference names.
    let (user, _) = h
        .platform
        .register_principal(
            env,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "end-user-1".into(),
            },
        )
        .await
        .unwrap();
    let resolved = h
        .state
        .principals
        .resolve(&ctx, &PrincipalReference::Id(user.id))
        .await
        .unwrap();
    assert_eq!(resolved.kind(), PrincipalKind::ExternalApplicationPrincipal);
    assert_ne!(resolved.id().as_uuid(), ctx.service_account_id().as_uuid());
    assert_ne!(resolved.id().as_uuid(), ctx.credential_id().as_uuid());
    assert!(resolved.linked_service_account_id().is_none());
    // Same scope, different identity types.
    assert_eq!(resolved.tenant_id(), ctx.tenant_id());
    assert_eq!(resolved.environment_id(), ctx.environment_id());

    // Only explicit registration links a service account as a principal.
    let (sa_principal, _) = h
        .platform
        .register_principal(env, NewPrincipal::ServiceAccount(sa))
        .await
        .unwrap();
    let resolved = h.state.principals.resolve(&ctx, &as_sa).await.unwrap();
    assert_eq!(resolved.id(), sa_principal.id);
    assert_eq!(resolved.linked_service_account_id(), Some(sa));
    assert_eq!(resolved.external_reference(), sa.to_string());

    // A SERVICE_ACCOUNT principal can never be created from free text.
    let err = h
        .platform
        .register_principal(
            env,
            NewPrincipal::External {
                kind: PrincipalKind::ServiceAccount,
                external_reference: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PlatformError::PolicyViolation(_)));
    let raw = common::exec(
        h.conn(),
        &format!(
            "INSERT INTO platform_principals (tenant_id, application_id, environment_id, kind, external_reference) \
             VALUES ('{}', '{}', '{}', 'SERVICE_ACCOUNT', 'free-text')",
            c.tenant.id, c.application.id, env
        ),
    )
    .await;
    assert!(raw.is_err());
}

#[tokio::test]
async fn service_account_principals_stay_inside_their_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    // The sandbox service account cannot become a principal of production.
    let err = h
        .platform
        .register_principal(
            c.env(EnvironmentKind::Production).id,
            NewPrincipal::ServiceAccount(c.sa(EnvironmentKind::Sandbox)),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PlatformError::NotFound(_)), "{err}");

    // A linked principal becomes inactive with its service account.
    let env = c.env(EnvironmentKind::Production).id;
    let sa = c.sa(EnvironmentKind::Production);
    let (p, _) = h
        .platform
        .register_principal(env, NewPrincipal::ServiceAccount(sa))
        .await
        .unwrap();
    let reader = h.issue(sa, &[names::PRINCIPALS_READ]).await;
    let ctx = h.ctx(reader.secret.expose_once()).await;
    h.platform
        .set_service_account_status(sa, ServiceAccountStatus::Suspended)
        .await
        .unwrap();
    assert!(matches!(
        h.state
            .principals
            .resolve(&ctx, &PrincipalReference::Id(p.id))
            .await,
        Err(PrincipalResolutionError::Inactive)
    ));
}

#[tokio::test]
async fn inactive_principals_do_not_resolve() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let env = c.env(EnvironmentKind::Test).id;
    let (p, _) = h
        .platform
        .register_principal(
            env,
            NewPrincipal::External {
                kind: PrincipalKind::TemporaryExternalParticipant,
                external_reference: "guest-1".into(),
            },
        )
        .await
        .unwrap();
    let token = h
        .issue(c.sa(EnvironmentKind::Test), &[names::PRINCIPALS_READ])
        .await;
    let ctx = h.ctx(token.secret.expose_once()).await;
    let reference = PrincipalReference::Id(p.id);
    assert!(h.state.principals.resolve(&ctx, &reference).await.is_ok());
    for status in [PrincipalStatus::Suspended, PrincipalStatus::Deactivated] {
        h.platform.set_principal_status(p.id, status).await.unwrap();
        assert!(matches!(
            h.state.principals.resolve(&ctx, &reference).await,
            Err(PrincipalResolutionError::Inactive)
        ));
    }
    h.platform
        .set_principal_status(p.id, PrincipalStatus::Active)
        .await
        .unwrap();
    assert!(h.state.principals.resolve(&ctx, &reference).await.is_ok());
}

#[tokio::test]
async fn telephone_endpoints_store_only_opaque_references() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let env = c.env(EnvironmentKind::Production).id;

    for phone in [
        "+2348012345678",
        "08012345678",
        "2348012345678",
        "tel:+2348012345678",
        "sip:+2348012345678@carrier.example",
        "+1 (555) 010-9999",
    ] {
        let err = h
            .platform
            .register_principal(
                env,
                NewPrincipal::External {
                    kind: PrincipalKind::TelephoneEndpoint,
                    external_reference: phone.into(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                PlatformError::PolicyViolation(_) | PlatformError::Validation(_)
            ),
            "{phone}: {err}"
        );
        // The database refuses it independently of the service.
        let raw = common::exec(
            h.conn(),
            &format!(
                "INSERT INTO platform_principals (tenant_id, application_id, environment_id, kind, external_reference) \
                 VALUES ('{}', '{}', '{}', 'TELEPHONE_ENDPOINT', '{}')",
                c.tenant.id, c.application.id, env, phone.replace('\'', "''")
            ),
        )
        .await;
        assert!(raw.is_err(), "database accepted {phone}");
    }
    // E.164 numbers are refused for every other category as well.
    let err = h
        .platform
        .register_principal(
            env,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "+2348012345678".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PlatformError::PolicyViolation(_)));

    // An opaque contact handle is accepted and is the only thing stored.
    let (p, _) = h
        .platform
        .register_principal(
            env,
            NewPrincipal::External {
                kind: PrincipalKind::TelephoneEndpoint,
                external_reference: CONTACT_HANDLE.into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(p.external_reference, CONTACT_HANDLE);
    let dump = common::dump_database(h.conn()).await;
    assert!(!dump.contains("2348012345678"));
    assert!(!dump.contains("08012345678"));
    // The only `*_number` columns are the call domain's integer ordinals
    // (call_session_intervals.interval_number, call_attempts.attempt_number).
    let phone_columns = scalar_i64(
        h.conn(),
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' \
         AND (column_name ILIKE '%phone%' OR column_name ILIKE '%e164%' OR column_name ILIKE '%msisdn%' \
              OR column_name ILIKE '%number%' OR column_name ILIKE '%contact%') \
         AND NOT (data_type = 'integer' AND (table_name, column_name) IN \
              (('call_session_intervals', 'interval_number'), ('call_attempts', 'attempt_number')))",
    )
    .await;
    assert_eq!(phone_columns, 0);
}

#[tokio::test]
async fn malformed_references_resolve_to_nothing() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let token = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let ctx = h.ctx(token.secret.expose_once()).await;
    for (kind, reference) in [
        (
            PrincipalKind::TelephoneEndpoint,
            "+2348012345678".to_string(),
        ),
        (PrincipalKind::ExternalApplicationPrincipal, String::new()),
        (PrincipalKind::ExternalApplicationPrincipal, "x".repeat(300)),
    ] {
        assert!(matches!(
            h.state
                .principals
                .resolve(
                    &ctx,
                    &PrincipalReference::External {
                        kind,
                        external_reference: reference
                    }
                )
                .await,
            Err(PrincipalResolutionError::InvalidReference)
        ));
    }
    // Resolution needs the read scope at the API layer.
    let no_scope = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let response = h
        .request(
            Method::POST,
            "/test/resolve",
            Some(no_scope.secret.expose_once()),
            Some(json!({ "kind": "BOT_OR_SERVICE", "reference": "x" })),
        )
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}

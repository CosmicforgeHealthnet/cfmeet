//! C. First-party and external consumers use the same model, credential system,
//! authentication path and principal resolution. Only explicit configuration differs.

mod common;

use common::{NewApplication, NewPrincipal};

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, PrincipalKind, TenantClassification},
    platform::{principals::PrincipalReference, scopes::names, PlatformError},
};
use common::Harness;
use serde_json::{json, Value};

#[tokio::test]
async fn first_party_and_external_consumers_share_one_model_and_path() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // CosmicForge-owned products are ordinary applications of a FIRST_PARTY tenant; an
    // external commercial customer is an ordinary application of an EXTERNAL tenant.
    let first_party = h
        .consumer("cosmicforge", TenantClassification::FirstParty)
        .await;
    let healthnet = h
        .platform
        .create_application(
            first_party.tenant.id,
            NewApplication {
                slug: "healthnet".into(),
                display_name: "HealthNet".into(),
            },
        )
        .await
        .unwrap();
    let karryam = h
        .platform
        .create_application(
            first_party.tenant.id,
            NewApplication {
                slug: "karryam".into(),
                display_name: "Karryam".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(healthnet.tenant_id, first_party.tenant.id);
    assert_eq!(karryam.tenant_id, first_party.tenant.id);
    let external = h
        .consumer("acme-logistics", TenantClassification::External)
        .await;

    let mut contexts: Vec<Value> = Vec::new();
    for consumer in [&first_party, &external] {
        let env = consumer.env(EnvironmentKind::Production);
        let issued = h
            .issue(
                consumer.sa(EnvironmentKind::Production),
                &[names::PRINCIPALS_READ],
            )
            .await;
        let token = issued.secret.expose_once();
        // Same credential format.
        assert!(token.starts_with("cfmk_live_"));

        // Same authentication path (the same router and middleware).
        let response = h.whoami(token).await;
        assert_eq!(response.status, StatusCode::OK);
        contexts.push(response.json.clone());

        // Same principal model and resolution.
        let (principal, _) = h
            .platform
            .register_principal(
                env.id,
                NewPrincipal::External {
                    kind: PrincipalKind::ExternalApplicationPrincipal,
                    external_reference: "customer-defined-subject-42".into(),
                },
            )
            .await
            .unwrap();
        let resolved = h
            .request(
                Method::POST,
                "/test/resolve",
                Some(token),
                Some(json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "reference": "customer-defined-subject-42" })),
            )
            .await;
        assert_eq!(resolved.status, StatusCode::OK);
        assert_eq!(resolved.json["id"], json!(principal.id));
        assert_eq!(resolved.json["tenant_id"], json!(consumer.tenant.id));
    }

    // Identical context shape; only the persisted classification and ids differ.
    let keys = |v: &Value| {
        let mut k: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&contexts[0]), keys(&contexts[1]));
    assert_eq!(contexts[0]["tenant_classification"], "FIRST_PARTY");
    assert_eq!(contexts[1]["tenant_classification"], "EXTERNAL");
    assert_eq!(contexts[0]["scopes"], contexts[1]["scopes"]);
    assert_eq!(
        contexts[0]["environment_kind"],
        contexts[1]["environment_kind"]
    );
}

#[tokio::test]
async fn first_party_privileges_exist_only_as_explicit_trusted_scopes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let first_party = h
        .consumer("cosmicforge", TenantClassification::FirstParty)
        .await;
    let external = h.consumer("acme", TenantClassification::External).await;

    // FIRST_PARTY does not imply anything: without an explicit grant there is no trusted
    // scope on the credential.
    let plain = h
        .issue(first_party.sa(EnvironmentKind::Production), &[])
        .await;
    let ctx = h.ctx(plain.secret.expose_once()).await;
    assert!(!ctx.has_scope(names::EVENTS_PRIVATE_DELIVERY));
    assert!(ctx.scopes().is_empty());

    // The trusted scope is grantable to the FIRST_PARTY application, explicitly …
    h.platform
        .grant_application_scope(first_party.application.id, names::EVENTS_PRIVATE_DELIVERY)
        .await
        .unwrap();
    let trusted = h
        .issue(
            first_party.sa(EnvironmentKind::Production),
            &[names::EVENTS_PRIVATE_DELIVERY],
        )
        .await;
    assert!(h
        .ctx(trusted.secret.expose_once())
        .await
        .has_scope(names::EVENTS_PRIVATE_DELIVERY));

    // … and never to an EXTERNAL one.
    let err = h
        .platform
        .grant_application_scope(external.application.id, names::EVENTS_PRIVATE_DELIVERY)
        .await
        .unwrap_err();
    assert!(matches!(err, PlatformError::PolicyViolation(_)), "{err}");

    // Trust classification is immutable: an external tenant cannot be promoted in place.
    let raw = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_tenants SET classification = 'FIRST_PARTY' WHERE id = '{}'",
            external.tenant.id
        ),
    )
    .await;
    assert!(raw.is_err());
}

#[tokio::test]
async fn every_consumer_application_is_the_same_entity() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let first_party = h
        .consumer("cosmicforge", TenantClassification::FirstParty)
        .await;
    let external = h.consumer("acme", TenantClassification::External).await;
    let apps_fp = h
        .platform
        .list_applications(first_party.tenant.id)
        .await
        .unwrap();
    let apps_ext = h
        .platform
        .list_applications(external.tenant.id)
        .await
        .unwrap();
    // Same table, same view type, same fields; nothing product-specific.
    let a = serde_json::to_value(&apps_fp[0]).unwrap();
    let b = serde_json::to_value(&apps_ext[0]).unwrap();
    let keys = |v: &Value| {
        let mut k: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&a), keys(&b));
    // The same principal reference string means different principals in different
    // consumers: business meaning stays with the consumer.
    let (p1, _) = h
        .platform
        .register_principal(
            first_party.env(EnvironmentKind::Sandbox).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "42".into(),
            },
        )
        .await
        .unwrap();
    let (p2, _) = h
        .platform
        .register_principal(
            external.env(EnvironmentKind::Sandbox).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "42".into(),
            },
        )
        .await
        .unwrap();
    assert_ne!(p1.id, p2.id);
    let token = h
        .issue(
            external.sa(EnvironmentKind::Sandbox),
            &[names::PRINCIPALS_READ],
        )
        .await;
    let ctx = h.ctx(token.secret.expose_once()).await;
    let resolved = h
        .state
        .principals
        .resolve(&ctx, &PrincipalReference::Id(p1.id))
        .await;
    assert!(resolved.is_err());
}

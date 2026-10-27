//! Developer Platform foundation (Part VI §§1–5): probes, migrations and database
//! ownership, internal service authentication, the gateway-forwarded context path, the
//! feature flag and the served contract.

mod common;

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use cf_service_auth::{DelegatedPlatformContext, OutboundServiceRequest};
use common::*;
use developer_platform::config::FeatureFlags;
use developer_platform_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::Value;
use uuid::Uuid;

const CONTEXT: &str = "/internal/v1/gateway/context";

#[tokio::test]
async fn health_and_readiness_report_the_own_database_and_flags() {
    let Some(h) = Harness::with_flags(FeatureFlags::default()).await else {
        return;
    };
    let r = h.get("/health").await;
    assert_eq!(r.status, StatusCode::OK);
    conforms(
        "platform-developer/common/v1.schema.json#/$defs/HealthStatus",
        &r.json,
    );
    assert!(r.headers.get("x-request-id").is_some());
    let r = h.get("/ready").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(
        "platform-developer/common/v1.schema.json#/$defs/ReadinessStatus",
        &r.json,
    );
    assert_eq!(r.json["migrations"], "current");
    assert_eq!(r.json["gatewayApi"], "disabled", "flags are off by default");
}

#[tokio::test]
async fn readiness_fails_while_a_migration_is_pending() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let (runtime, _) = service_auth();
    let router = developer_platform::http::app(developer_platform::state::AppState::new(
        db.conn.clone(),
        runtime,
    ));
    use tower::ServiceExt;
    let r = router
        .oneshot(
            axum::http::Request::get("/ready")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply_in_their_own_history_table() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    Migrator::up(&db.conn, None).await.unwrap();
    assert!(Migrator::get_pending_migrations(&db.conn)
        .await
        .unwrap()
        .is_empty());
    Migrator::reset(&db.conn).await.unwrap();
    let left = db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' AND tablename LIKE 'devplat\\_%'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap();
    assert_eq!(left, 0, "down removes every devplat_ table");
    Migrator::up(&db.conn, None).await.unwrap();
    let applied = db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*) FROM developer_platform_seaql_migrations",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap();
    assert_eq!(applied as usize, Migrator::migrations().len());
}

#[tokio::test]
async fn the_schema_owns_only_devplat_tables_and_no_identity_hierarchy() {
    let Some(h) = Harness::new().await else {
        return;
    };
    assert_eq!(
        h.count(
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' \
             AND tablename <> 'developer_platform_seaql_migrations' AND tablename !~ '^devplat_'"
        )
        .await,
        0
    );
    // Platform Access is the identity authority: no copy of its attributes (slug,
    // classification, scopes), credentials or verifiers; no communications content; no
    // E2EE material. The only columns matching sensitive-looking names are the
    // Developer Platform's own metadata, listed explicitly with their reason.
    let rows = h
        .db
        .conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT table_name || '.' || column_name AS c FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name LIKE 'devplat\\_%' AND (column_name ~ \
             '(name|slug|display|classification|status|scope|credential|secret|verifier|hash|pepper|password|token|private|key_material|plaintext|content|body|e164|phone)') \
             ORDER BY 1",
        ))
        .await
        .unwrap();
    let found: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "c").unwrap())
        .collect();
    const ALLOWED: &[&str] = &[
        // Section 51 normalized API_OPERATION usage: response status class (2xx..5xx).
        "devplat_api_operation_usage.status_class",
        // Section 50 billing-account reference metadata (never Platform Access identity):
        // RETAIL / CONTRACT / INTERNAL_COST_CENTER and its own lifecycle.
        "devplat_billing_accounts.classification",
        "devplat_billing_accounts.status",
        // Plans: customer-facing display metadata and lifecycle.
        "devplat_commercial_plans.display_name",
        "devplat_commercial_plans.status",
        // Developer Platform display metadata and its own lifecycle (not Platform Access's).
        "devplat_environment_profiles.display_name",
        "devplat_environment_profiles.status",
        // Idempotency replay (actor + operation key; secrets redacted before storage).
        "devplat_idempotency_records.response_body",
        "devplat_idempotency_records.response_status",
        "devplat_idempotency_records.scope_key",
        // Invitations: the one-time token is stored as a SHA-256 digest only.
        "devplat_invitations.status",
        "devplat_invitations.token_digest",
        "devplat_organizations.display_name",
        "devplat_project_profiles.display_name",
        "devplat_project_profiles.status",
        "devplat_project_profiles.status_reason",
        // Section 53 webhooks: attempt / delivery status classes, the subscription's own
        // lifecycle, and its signing secret stored ONLY sealed (AES-256-GCM through
        // cf_webhooks, CHECK-enforced `v2.<kid>.` format) with the sealing key's id.
        "devplat_webhook_attempts.http_status",
        "devplat_webhook_deliveries.last_http_status",
        "devplat_webhook_deliveries.status",
        // Section 53 migration bookkeeping: Calling's subscription status and secret
        // version at import, and the secret OUTCOME code (TRANSFERRED / ROTATION_REQUIRED_*;
        // never secret material).
        "devplat_webhook_migration_subscriptions.calling_secret_version",
        "devplat_webhook_migration_subscriptions.calling_status",
        "devplat_webhook_migration_subscriptions.secret_outcome",
        "devplat_webhook_subscriptions.secret_key_id",
        "devplat_webhook_subscriptions.secret_sealed",
        // VALID / ROTATION_REQUIRED (a migrated subscription without any secret).
        "devplat_webhook_subscriptions.secret_state",
        "devplat_webhook_subscriptions.secret_version",
        "devplat_webhook_subscriptions.status",
        "devplat_workspace_members.status",
    ];
    assert_eq!(
        found, ALLOWED,
        "every sensitive-looking column is justified"
    );
    // No table of the identity hierarchy itself.
    assert_eq!(
        h.count(
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' AND tablename ~ \
             '(tenants|applications|environments$|service_accounts|credentials|scopes|principals|memberships)'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn canonical_environment_references_are_immutable() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let d = delegated();
    let r = h
        .signed_get(&h.peers.gateway, CONTEXT, Some(d.clone()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let err = h
        .db
        .conn
        .execute(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "UPDATE devplat_environment_refs SET tenant_id = gen_random_uuid() WHERE environment_id = '{}'",
                d.application_environment_id()
            ),
        ))
        .await;
    assert!(err.is_err(), "the chain of an environment never changes");
}

#[tokio::test]
async fn every_internal_path_requires_a_valid_service_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    for path in [
        CONTEXT,
        "/internal/v1/unknown",
        "/internal/v1/admin/tenants",
    ] {
        let r = h.get(path).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(r.json["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
        conforms(
            "platform-developer/common/v1.schema.json#/$defs/Error",
            &r.json,
        );
        assert_eq!(
            r.headers.get("cache-control").unwrap(),
            &HeaderValue::from_static("no-store")
        );
    }
    // A key nobody trusts, claiming the gateway's identity.
    let r = h
        .signed_get(&h.peers.rogue_gateway, CONTEXT, Some(delegated()))
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // A customer bearer credential is not a service token.
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer cfk_test_x"),
    );
    let r = h.call(Method::GET, CONTEXT, headers).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // A forged delegated context header next to a token is not part of the signature.
    let signed = h
        .peers
        .gateway
        .sign(&OutboundServiceRequest::new_operation(
            cf_service_auth::ServiceId::developer_platform(),
        ))
        .unwrap();
    let mut headers = HeaderMap::new();
    signed.apply_to(&mut headers);
    headers.insert(
        "x-cf-delegated-context",
        HeaderValue::from_static("{\"tenant_id\":\"00000000-0000-0000-0000-000000000000\"}"),
    );
    let r = h.call(Method::GET, CONTEXT, headers).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"]["code"], "DELEGATED_CONTEXT_REQUIRED");
    assert!(
        h.pa.calls.lock().unwrap().is_empty(),
        "nothing reached the authority"
    );
    // Tampering with a signed token breaks it.
    let mut headers = HeaderMap::new();
    signed.apply_to(&mut headers);
    let token = headers
        .get("x-cf-service-authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let tampered = format!("{}x", token);
    headers.insert(
        "x-cf-service-authorization",
        HeaderValue::from_str(&tampered).unwrap(),
    );
    let r = h.call(Method::GET, CONTEXT, headers).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_the_gateway_permission_reaches_the_forwarded_context() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h
        .signed_get(&h.peers.messaging, CONTEXT, Some(delegated()))
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
    // A trust bundle cannot even grant the permission to another issuer.
    let bundle = serde_json::json!({
        "bundle_version": 1,
        "receiver": cf_service_auth::services::DEVELOPER_PLATFORM,
        "runtime_environment": ENV,
        "trusted_issuers": [{
            "issuer": cf_service_auth::services::MESSAGING,
            "audience": cf_service_auth::services::DEVELOPER_PLATFORM,
            "runtime_environment": ENV,
            "keys": [key("messaging-test-2").public_jwk()],
            "permissions": ["developer-request.forward"],
        }],
    });
    assert!(cf_service_auth::TrustPolicy::from_json(
        &bundle.to_string(),
        &cf_service_auth::ServiceId::developer_platform(),
        ENV
    )
    .is_err());
    let r = h.signed_get(&h.peers.gateway, CONTEXT, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"]["code"], "DELEGATED_CONTEXT_REQUIRED");
}

#[tokio::test]
async fn the_forwarded_context_is_confirmed_by_platform_access_and_anchored_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let d = delegated();
    for _ in 0..2 {
        let r = h
            .signed_get(&h.peers.gateway, CONTEXT, Some(d.clone()))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        conforms(
            "platform-developer/common/v1.schema.json#/$defs/GatewayContext",
            &r.json,
        );
        assert_eq!(r.json["tenantId"], Value::String(d.tenant_id().to_string()));
        assert_eq!(r.json["authority"], "PLATFORM_ACCESS");
        assert_eq!(r.json["forwardedBy"], "cfmeet.api-gateway");
        assert!(r.headers.get("x-cf-request-id").is_some());
    }
    assert_eq!(
        h.pa.calls.lock().unwrap().len(),
        2,
        "re-validated every time"
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM devplat_environment_refs WHERE environment_id = '{}' AND tenant_id = '{}'",
            d.application_environment_id(),
            d.tenant_id()
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn a_context_platform_access_refuses_or_contradicts_is_rejected() {
    let Some(h) = Harness::new().await else {
        return;
    };
    for answer in [Answer::Refuse, Answer::Other] {
        *h.pa.answer.lock().unwrap() = Some(answer);
        let r = h
            .signed_get(&h.peers.gateway, CONTEXT, Some(delegated()))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(r.json["error"]["code"], "DELEGATED_CONTEXT_INVALID");
    }
    // A correctly signed hop naming a credential revoked since the gateway's check
    // (or naming none) is refused: the credential must still authenticate now.
    *h.pa.answer.lock().unwrap() = Some(Answer::Echo);
    let revoked = delegated();
    h.pa.revoked
        .lock()
        .unwrap()
        .push(revoked.initiating_credential_id().unwrap());
    for ctx in [
        revoked,
        DelegatedPlatformContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            cf_service_auth::ApplicationEnvironmentKind::Sandbox,
            Uuid::new_v4(),
        ),
    ] {
        let r = h.signed_get(&h.peers.gateway, CONTEXT, Some(ctx)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(r.json["error"]["code"], "DELEGATED_CONTEXT_INVALID");
    }
    assert_eq!(
        h.count("SELECT count(*) FROM devplat_environment_refs")
            .await,
        0
    );
    // The authority unreachable fails closed.
    *h.pa.answer.lock().unwrap() = None;
    let r = h
        .signed_get(&h.peers.gateway, CONTEXT, Some(delegated()))
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json["error"]["code"], "SERVICE_UNAVAILABLE");
}

#[tokio::test]
async fn the_gateway_api_is_hidden_while_its_flag_is_off() {
    let Some(h) = Harness::with_flags(FeatureFlags::default()).await else {
        return;
    };
    let r = h
        .signed_get(&h.peers.gateway, CONTEXT, Some(delegated()))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(h.pa.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_served_openapi_is_the_contract_and_every_listed_route_exists() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.get("/openapi.json").await;
    assert_eq!(r.status, StatusCode::OK);
    let contract: Value =
        serde_json::from_str(developer_platform::http::OPENAPI).expect("contract parses");
    assert_eq!(r.json, contract);
    for (path, ops) in contract["paths"].as_object().unwrap() {
        for method in ops.as_object().unwrap().keys() {
            // Parameterised and operator routes are covered by the control-plane suite;
            // gateway-forwarded customer routes (`/v1/**`) by the webhook suite.
            if method != "get"
                || path.contains('{')
                || path.contains("/operator/")
                || path.starts_with("/v1/")
            {
                continue;
            }
            let r = if path.starts_with("/internal/") {
                h.signed_get(&h.peers.gateway, path, Some(delegated()))
                    .await
            } else {
                h.get(path).await
            };
            assert!(r.status.is_success(), "{path}: {} {}", r.status, r.json);
        }
    }
    // Every /v1 path (unknown ones included) requires the gateway hop first; the answer
    // is the error envelope, not a framework page.
    let r = h.get("/v1/anything").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
    let without_credential = DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        cf_service_auth::ApplicationEnvironmentKind::Sandbox,
        Uuid::new_v4(),
    );
    let r = h
        .signed_get(&h.peers.gateway, "/v1/anything", Some(without_credential))
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "a delegated context without a credential id is not a caller"
    );
}

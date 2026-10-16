//! Controlled identity migration (Part IV §B2 / §B6.1, §B6.9): credentials minted by the
//! Part III authority keep authenticating after the import with the SAME identifiers and
//! lifecycle state; the import is integrity-checked, idempotent and all-or-nothing; the
//! recovery export round-trips.

mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, Utc};
use common::*;
use platform_access::{
    domain::{
        ApplicationStatus, CredentialStatus, EnvironmentKind, EnvironmentStatus, PrincipalKind,
        PrincipalStatus, ServiceAccountStatus, TenantClassification, TenantStatus,
    },
    identity::{handoff::*, operator::OperatorContext},
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

/// A Part III credential minted outside Platform Access: (bundle row, raw token).
fn part_iii_credential(
    scope: (Uuid, Uuid, Uuid, Uuid),
    kind_tag: &str,
    created: DateTime<Utc>,
    expires: DateTime<Utc>,
) -> (BundleCredential, String) {
    let mut public = [0u8; 12];
    let mut secret = [0u8; 32];
    for (i, b) in public.iter_mut().enumerate() {
        *b = rand_byte(i as u8);
    }
    for (i, b) in secret.iter_mut().enumerate() {
        *b = rand_byte(100 + i as u8);
    }
    let public_id = hex::encode(public);
    let secret_hex = hex::encode(secret);
    let verifier = part_iii_verifier(TEST_PEPPER, kind_tag, &public_id, &secret);
    let (tenant_id, application_id, environment_id, service_account_id) = scope;
    (
        BundleCredential {
            id: Uuid::new_v4(),
            public_id: public_id.clone(),
            tenant_id,
            application_id,
            environment_id,
            service_account_id,
            label: Some("legacy".into()),
            verifier: hex::encode(verifier),
            verifier_algorithm: "HMAC_SHA256".into(),
            verifier_key_version: 1,
            last_four: secret_hex[60..].to_string(),
            status: CredentialStatus::Active,
            not_before: created,
            expires_at: expires,
            revoked_at: None,
            rotated_from_credential_id: None,
            superseded_by_credential_id: None,
            superseded_at: None,
            supersession_valid_until: None,
            created_at: created,
            updated_at: created,
        },
        format!("cfmk_{kind_tag}_{public_id}_{secret_hex}"),
    )
}

fn rand_byte(seed: u8) -> u8 {
    // Distinct per call site without a RNG dependency.
    let r = Uuid::new_v4();
    r.as_bytes()[(seed % 16) as usize] ^ seed
}

struct Legacy {
    body: BundleBody,
    tenant: Uuid,
    application: Uuid,
    sandbox: Uuid,
    active_token: String,
    active_id: Uuid,
    revoked_token: String,
    expired_token: String,
    predecessor_token: String,
    successor_token: String,
    tombstoned: Uuid,
}

/// A Part III identity estate: one tenant / application / sandbox environment / service
/// account, credentials in every lifecycle state, scopes and principals.
fn legacy_estate(now: DateTime<Utc>) -> Legacy {
    let t = now - Duration::days(30);
    let tenant = Uuid::new_v4();
    let application = Uuid::new_v4();
    let sandbox = Uuid::new_v4();
    let sa = Uuid::new_v4();
    let scope = (tenant, application, sandbox, sa);
    let (active, active_token) = part_iii_credential(scope, "sbx", t, now + Duration::days(300));
    let (mut revoked, revoked_token) =
        part_iii_credential(scope, "sbx", t, now + Duration::days(300));
    revoked.status = CredentialStatus::Revoked;
    revoked.revoked_at = Some(now - Duration::days(1));
    let (expired, expired_token) = part_iii_credential(
        scope,
        "sbx",
        t - Duration::days(400),
        now - Duration::days(1),
    );
    let (mut predecessor, predecessor_token) =
        part_iii_credential(scope, "sbx", t, now + Duration::days(300));
    let (mut successor, successor_token) = part_iii_credential(
        scope,
        "sbx",
        t + Duration::hours(1),
        now + Duration::days(300),
    );
    successor.rotated_from_credential_id = Some(predecessor.id);
    predecessor.superseded_by_credential_id = Some(successor.id);
    predecessor.superseded_at = Some(now - Duration::minutes(10));
    predecessor.supersession_valid_until = Some(now + Duration::minutes(50));
    let tombstoned = Uuid::new_v4();
    let body = BundleBody {
        tenants: vec![BundleTenant {
            id: tenant,
            slug: "legacy".into(),
            display_name: "Legacy tenant".into(),
            classification: TenantClassification::External,
            status: TenantStatus::Active,
            billing_currency: "NGN".into(),
            created_at: t,
            updated_at: t,
        }],
        applications: vec![BundleApplication {
            id: application,
            tenant_id: tenant,
            slug: "app".into(),
            display_name: "App".into(),
            status: ApplicationStatus::Active,
            created_at: t,
            updated_at: t,
        }],
        environments: vec![BundleEnvironment {
            id: sandbox,
            tenant_id: tenant,
            application_id: application,
            kind: EnvironmentKind::Sandbox,
            status: EnvironmentStatus::Active,
            live_telecom_allowed: false,
            created_at: t,
            updated_at: t,
        }],
        service_accounts: vec![BundleServiceAccount {
            id: sa,
            tenant_id: tenant,
            application_id: application,
            environment_id: sandbox,
            name: "backend".into(),
            description: None,
            status: ServiceAccountStatus::Active,
            revoked_at: None,
            created_at: t,
            updated_at: t,
        }],
        application_scopes: vec![BundleApplicationScope {
            tenant_id: tenant,
            application_id: application,
            scope: "calls.read".into(),
            allowed_environment_kinds: None,
            grant_basis: None,
            granted_at: t,
        }],
        credential_scopes: vec![BundleCredentialScope {
            credential_id: active.id,
            tenant_id: tenant,
            application_id: application,
            scope: "calls.read".into(),
            granted_at: t,
        }],
        principals: vec![
            BundlePrincipal {
                id: Uuid::new_v4(),
                tenant_id: tenant,
                application_id: application,
                environment_id: sandbox,
                kind: PrincipalKind::NativeCfUser,
                external_reference: "user-1".into(),
                linked_service_account_id: None,
                status: PrincipalStatus::Active,
                tombstoned_at: None,
                created_at: t,
                updated_at: t,
            },
            BundlePrincipal {
                id: tombstoned,
                tenant_id: tenant,
                application_id: application,
                environment_id: sandbox,
                kind: PrincipalKind::NativeCfUser,
                external_reference: format!("tombstoned:{tombstoned}"),
                linked_service_account_id: None,
                status: PrincipalStatus::Deactivated,
                tombstoned_at: Some(t),
                created_at: t,
                updated_at: t,
            },
        ],
        credentials: vec![active.clone(), revoked, expired, predecessor, successor],
    };
    Legacy {
        body,
        tenant,
        application,
        sandbox,
        active_id: active.id,
        active_token,
        revoked_token,
        expired_token,
        predecessor_token,
        successor_token,
        tombstoned,
    }
}

async fn count(h: &Harness, table: &str) -> i64 {
    h.conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT count(*) FROM {table}"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap()
}

#[tokio::test]
async fn part_iii_credentials_keep_authenticating_with_the_same_identity_after_import() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let now = {
        use platform_access::domain::Clock;
        h.clock.now()
    };
    let legacy = legacy_estate(now);
    let bytes = write_bundle("cfmeet.calling", now, &legacy.body).unwrap();
    let bundle = parse_bundle(&bytes).unwrap();
    let op = OperatorContext::new(OPERATOR).unwrap();
    let receipt = h
        .state
        .platform
        .operator_import_bundle(&op, &bundle)
        .await
        .unwrap();
    assert!(!receipt.already_imported);
    assert_eq!(receipt.counts["credentials"], 5);
    assert_eq!(receipt.verifier_digest, legacy.body.verifier_digest());

    // Same ids, same scopes, same environment.
    let r = h.authenticate(&legacy.active_token).await;
    assert_eq!(r.body["outcome"], "AUTHENTICATED", "{}", r.body);
    let a = &r.body["authority"];
    assert_eq!(uuid(&a["credential"]["id"]), legacy.active_id);
    assert_eq!(uuid(&a["tenant"]["id"]), legacy.tenant);
    assert_eq!(uuid(&a["application"]["id"]), legacy.application);
    assert_eq!(uuid(&a["environment"]["id"]), legacy.sandbox);
    assert_eq!(a["tenant"]["billingCurrency"], "NGN");
    assert_eq!(a["scopes"], json!(["calls.read"]));
    // Lifecycle state carried over exactly.
    assert_eq!(
        h.authenticate(&legacy.revoked_token).await.body["reason"],
        "credential_revoked"
    );
    assert_eq!(
        h.authenticate(&legacy.expired_token).await.body["reason"],
        "credential_expired"
    );
    let p = h.authenticate(&legacy.predecessor_token).await;
    assert_eq!(p.body["authority"]["effectiveState"], "ROTATION_OVERLAP");
    assert_eq!(
        h.authenticate(&legacy.successor_token).await.body["outcome"],
        "AUTHENTICATED"
    );
    h.clock.advance(Duration::hours(1));
    assert_eq!(
        h.authenticate(&legacy.predecessor_token).await.body["reason"],
        "credential_superseded"
    );
    // The tombstoned principal stays tombstoned.
    let status: String = h
        .conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT status FROM platform_principals WHERE id = '{}' AND tombstoned_at IS NOT NULL", legacy.tombstoned),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(status, "DEACTIVATED");
    // The receipt is readable by the source through the internal contract.
    let r = h
        .as_calling(
            Method::GET,
            &format!("/internal/v1/identity/imports/{}", receipt.bundle_digest),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["verifierDigest"], receipt.verifier_digest);
    // Imported rows are published to the change feed (consumers project them).
    assert!(count(&h, "platform_identity_changes").await >= 12);
}

#[tokio::test]
async fn reimport_is_idempotent_and_tampering_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let now = Utc::now();
    let legacy = legacy_estate(now);
    let bytes = write_bundle("cfmeet.calling", now, &legacy.body).unwrap();
    let op = OperatorContext::new(OPERATOR).unwrap();
    let first = h
        .state
        .platform
        .operator_import_bundle(&op, &parse_bundle(&bytes).unwrap())
        .await
        .unwrap();
    let second = h
        .state
        .platform
        .operator_import_bundle(&op, &parse_bundle(&bytes).unwrap())
        .await
        .unwrap();
    assert!(second.already_imported);
    assert_eq!(first.id, second.id);
    assert_eq!(count(&h, "platform_identity_imports").await, 1);
    // A modified body no longer matches its digest.
    let tampered = String::from_utf8(bytes)
        .unwrap()
        .replace("\"legacy\"", "\"hijack\"");
    assert!(parse_bundle(tampered.as_bytes()).is_err());
}

#[tokio::test]
async fn a_conflicting_import_changes_nothing() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // An existing tenant with the slug the bundle uses.
    h.consumer("legacy", "EXTERNAL", &[]).await;
    let tenants = count(&h, "platform_tenants").await;
    let changes = count(&h, "platform_identity_changes").await;
    let audit = count(&h, "platform_access_audit").await;
    let now = Utc::now();
    let legacy = legacy_estate(now);
    let bytes = write_bundle("cfmeet.calling", now, &legacy.body).unwrap();
    let op = OperatorContext::new(OPERATOR).unwrap();
    assert!(h
        .state
        .platform
        .operator_import_bundle(&op, &parse_bundle(&bytes).unwrap())
        .await
        .is_err());
    // All or nothing: no row, no change, no audit, no receipt.
    assert_eq!(count(&h, "platform_tenants").await, tenants);
    assert_eq!(count(&h, "platform_api_credentials").await, 0);
    assert_eq!(count(&h, "platform_identity_changes").await, changes);
    assert_eq!(count(&h, "platform_access_audit").await, audit);
    assert_eq!(count(&h, "platform_identity_imports").await, 0);
    assert!(h.authenticate(&legacy.active_token).await.body["outcome"] == "REJECTED");
}

#[tokio::test]
async fn the_recovery_export_round_trips_into_a_fresh_authority() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    let (view, secret) = h.issue(c.sa("PRODUCTION"), &["calls.read"]).await;
    let body = h.state.platform.export_bundle_body().await.unwrap();
    assert_eq!(body.credentials.len(), 1);
    let bytes = write_bundle("cfmeet.platform-access", Utc::now(), &body).unwrap();
    // A fresh authority restored from the export authenticates the same credential.
    let Some(fresh) = Harness::new().await else {
        return;
    };
    let op = OperatorContext::new(OPERATOR).unwrap();
    fresh
        .state
        .platform
        .operator_import_bundle(&op, &parse_bundle(&bytes).unwrap())
        .await
        .unwrap();
    let r = fresh.authenticate(&secret).await;
    assert_eq!(r.body["outcome"], "AUTHENTICATED", "{}", r.body);
    assert_eq!(r.body["authority"]["credential"]["id"], view["id"]);
    // The export never contains the raw secret.
    assert!(!String::from_utf8(bytes)
        .unwrap()
        .contains(&secret[secret.len() - 64..]));
}

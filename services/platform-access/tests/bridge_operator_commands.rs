//! ADR 0068 A4: the admin bridge's identity commands (`POST /internal/v1/operator/*`) and
//! the settlement read.
//!
//! * `POST /internal/v1/operator/tenants/{tenantId}/status`
//! * `POST /internal/v1/operator/applications/{applicationId}/status`
//! * `POST /internal/v1/operator/environments/{environmentId}/status`
//! * `POST /internal/v1/operator/service-accounts/{serviceAccountId}/status`
//! * `POST /internal/v1/operator/applications/{applicationId}/scope-grants`
//! * `POST /internal/v1/operator/applications/{applicationId}/privileged-scope-grants`
//! * `POST /internal/v1/operator/credentials/{credentialId}/revoke`
//! * `POST /internal/v1/operator/credentials/{credentialId}/suspend`
//! * `POST /internal/v1/operator/credentials/{credentialId}/resume`
//! * `POST /internal/v1/operator/environments/{environmentId}/principals`
//! * `POST /internal/v1/operator/principals/{principalId}/status`
//! * `POST /internal/v1/operator/principals/{principalId}/remove`
//! * `POST /internal/v1/operator/principals/{principalId}/sign-out`
//! * `POST /internal/v1/operator/app-grants/{grantId}/revoke`
//! * `POST /internal/v1/operator/environments/{environmentId}/origins/remove`
//! * `GET /internal/v1/operator/operations/{idempotencyKey}`
//!
//! Real router, real service tokens: only the bridge with `platform-identity.administer`,
//! a strict `hq:` operator, an `Idempotency-Key` and the two-person approval changes
//! anything; a stale `expectedVersion` is `VERSION_CONFLICT`; a repeated key answers
//! again without applying; every applied command has its settlement record; no answer
//! carries a secret or an external reference.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use platform_access::config::platform_build::PlatformBuildConfig;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const HQ: &str = "hq:staff-0001";
const APPROVER: &str = "hq:staff-0002";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn flags_on() -> PlatformBuildConfig {
    PlatformBuildConfig {
        end_users_enabled: true,
        environment_origins_enabled: true,
        oauth_enabled: true,
        ..PlatformBuildConfig::default()
    }
}

async fn harness() -> Option<Harness> {
    Harness::with_build(flags_on()).await
}

fn headers(key: Option<Uuid>, approved: bool) -> Vec<(&'static str, String)> {
    let mut out = vec![("x-cf-operator", HQ.to_string())];
    if let Some(k) = key {
        out.push(("idempotency-key", k.to_string()));
    }
    if approved {
        out.push(("x-cf-approver", APPROVER.to_string()));
        out.push(("x-cf-approval-ref", "apr_0001".to_string()));
        out.push(("x-cf-intent-digest", DIGEST.to_string()));
    }
    out
}

/// A fully attributed bridge command with `key`.
async fn send(h: &Harness, path: &str, body: Value, key: Uuid) -> Reply {
    h.call_with_headers(
        &h.peers.bridge,
        Method::POST,
        path,
        Some(body),
        None,
        None,
        &headers(Some(key), true),
    )
    .await
}

#[track_caller]
fn ok(r: &Reply) -> &Value {
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let keys: Vec<&String> = r.body.as_object().unwrap().keys().collect();
    assert_eq!(
        keys.len(),
        3,
        "exactly status, version, resource: {}",
        r.body
    );
    &r.body
}

async fn record(h: &Harness, key: Uuid) -> Reply {
    h.call_with_headers(
        &h.peers.bridge,
        Method::GET,
        &format!("/internal/v1/operator/operations/{key}"),
        None,
        None,
        None,
        &headers(None, false),
    )
    .await
}

async fn count(h: &Harness, sql: String) -> i64 {
    h.conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

async fn text(h: &Harness, sql: String) -> String {
    h.conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

/// The record answers what the command answered.
async fn settles_as(h: &Harness, key: Uuid, answer: &Value) {
    let r = record(h, key).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["idempotency_key"], key.to_string());
    assert_eq!(r.body["outcome"], "APPLIED");
    assert_eq!(r.body["source_status"], answer["status"]);
    assert_eq!(r.body["source_version"], answer["version"]);
}

#[tokio::test]
async fn a_status_change_is_refused_before_anything_is_done() {
    let Some(h) = harness().await else {
        return;
    };
    let c = h.consumer("bridge-refusals", "EXTERNAL", &[]).await;
    let t = c.tenant_id;
    let path = format!("/internal/v1/operator/tenants/{t}/status");
    let body = json!({ "expectedVersion": 1, "status": "SUSPENDED" });
    let key = Uuid::new_v4();

    // The operator console (permission, legacy operator) and a caller without it.
    let console = h
        .call_with_headers(
            &h.peers.operator,
            Method::POST,
            &path,
            Some(body.clone()),
            None,
            Some(OPERATOR),
            &[("idempotency-key", key.to_string())],
        )
        .await;
    assert_eq!(console.status, StatusCode::BAD_REQUEST, "{}", console.body);
    assert_eq!(console.code(), "OPERATOR_REQUIRED");
    let calling = h
        .call_with_headers(
            &h.peers.calling,
            Method::POST,
            &path,
            Some(body.clone()),
            None,
            None,
            &[("idempotency-key", key.to_string())],
        )
        .await;
    assert_eq!(calling.status, StatusCode::FORBIDDEN, "{}", calling.body);

    // The bridge without a key, without the approval, with a stale version, with an
    // unknown field.
    let no_key = h
        .call_with_headers(
            &h.peers.bridge,
            Method::POST,
            &path,
            Some(body.clone()),
            None,
            None,
            &headers(None, true),
        )
        .await;
    assert_eq!(no_key.status, StatusCode::BAD_REQUEST, "{}", no_key.body);
    assert_eq!(no_key.code(), "VALIDATION_ERROR");
    let unapproved = h
        .call_with_headers(
            &h.peers.bridge,
            Method::POST,
            &path,
            Some(body.clone()),
            None,
            None,
            &headers(Some(key), false),
        )
        .await;
    assert_eq!(unapproved.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unapproved.code(), "POLICY_VIOLATION");
    let stale = send(
        &h,
        &path,
        json!({ "expectedVersion": 7, "status": "SUSPENDED" }),
        key,
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    assert_eq!(stale.code(), "VERSION_CONFLICT");
    let zero = send(
        &h,
        &path,
        json!({ "expectedVersion": 0, "status": "SUSPENDED" }),
        key,
    )
    .await;
    assert_eq!(zero.status, StatusCode::BAD_REQUEST);
    let unknown = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "status": "SUSPENDED", "note": "x" }),
        key,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let missing = send(
        &h,
        &format!("/internal/v1/operator/tenants/{}/status", Uuid::new_v4()),
        body.clone(),
        key,
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);

    // Nothing was done or recorded.
    assert_eq!(
        text(
            &h,
            format!("SELECT status FROM platform_tenants WHERE id = '{t}'")
        )
        .await,
        "ACTIVE"
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM platform_access_admin_operations".into()
        )
        .await,
        0
    );
    assert_eq!(record(&h, key).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn status_changes_apply_once_record_and_replay() {
    let Some(h) = harness().await else {
        return;
    };
    let c = h.consumer("bridge-status", "EXTERNAL", &[]).await;
    let t = c.tenant_id;

    let key = Uuid::new_v4();
    let path = format!("/internal/v1/operator/tenants/{t}/status");
    let body = json!({ "expectedVersion": 1, "status": "SUSPENDED" });
    let r = send(&h, &path, body.clone(), key).await;
    let a = ok(&r).clone();
    assert_eq!(a["status"], "SUSPENDED");
    assert_eq!(a["version"], "2");
    assert_eq!(a["resource"]["id"], t.to_string());
    assert!(a["resource"].get("displayName").is_none());
    assert!(a["resource"].get("slug").is_none());
    settles_as(&h, key, &a).await;

    // The same key again: the same answer, nothing applied twice.
    let again = send(&h, &path, body.clone(), key).await;
    assert_eq!(ok(&again), &a);
    assert_eq!(
        count(
            &h,
            format!("SELECT revision FROM platform_tenants WHERE id = '{t}'")
        )
        .await,
        2
    );
    // The same key for another subject: refused, nothing done.
    let other = send(
        &h,
        &format!(
            "/internal/v1/operator/applications/{}/status",
            c.application_id
        ),
        json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        key,
    )
    .await;
    assert_eq!(other.status, StatusCode::CONFLICT, "{}", other.body);
    assert_eq!(other.code(), "CONFLICT");

    // The audit row joins the record: bridge actor, hq_<key> correlation.
    assert_eq!(
        count(
            &h,
            format!(
                "SELECT count(*) FROM platform_access_audit WHERE actor = 'cfmeet.admin-bridge' \
                 AND correlation_id = 'hq_{}' AND action = 'TENANT_STATUS_SET'",
                key.simple()
            )
        )
        .await,
        1
    );
    assert_eq!(
        text(
            &h,
            format!(
                "SELECT operator_ref || ' ' || approver_ref || ' ' || command FROM \
                 platform_access_admin_operations WHERE idempotency_key = '{key}'"
            )
        )
        .await,
        format!("{HQ} {APPROVER} identity.status.change")
    );

    // Application, environment, service account (partners.service-account.suspend).
    let app = send(
        &h,
        &format!(
            "/internal/v1/operator/applications/{}/status",
            c.application_id
        ),
        json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(ok(&app)["status"], "SUSPENDED");
    let env_key = Uuid::new_v4();
    let env = send(
        &h,
        &format!(
            "/internal/v1/operator/environments/{}/status",
            c.env("SANDBOX")
        ),
        json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        env_key,
    )
    .await;
    let env = ok(&env).clone();
    assert_eq!(env["status"], "SUSPENDED");
    assert_eq!(env["resource"]["kind"], "SANDBOX");
    settles_as(&h, env_key, &env).await;
    let sa_key = Uuid::new_v4();
    let sa = send(
        &h,
        &format!(
            "/internal/v1/operator/service-accounts/{}/status",
            c.sa("TEST")
        ),
        json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        sa_key,
    )
    .await;
    let sa = ok(&sa).clone();
    assert_eq!(sa["status"], "SUSPENDED");
    assert_eq!(sa["version"], "2");
    assert!(sa["resource"].get("name").is_none());
    settles_as(&h, sa_key, &sa).await;

    // The settlement read is the bridge's only: the console and others get 404.
    for signer in [&h.peers.operator, &h.peers.calling] {
        let r = h
            .call(
                signer,
                Method::GET,
                &format!("/internal/v1/operator/operations/{key}"),
                None,
                None,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    }
    assert_eq!(
        record(&h, Uuid::new_v4()).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn api_keys_are_revoked_suspended_and_resumed_never_revealed() {
    let Some(h) = harness().await else {
        return;
    };
    let c = h.consumer("bridge-keys", "EXTERNAL", &["calls.read"]).await;
    let (credential, secret) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let id = uuid(&credential["id"]);
    let base = format!("/internal/v1/operator/credentials/{id}");

    // The bridge read carries the version the commands expect.
    let read = h
        .call_with_headers(
            &h.peers.bridge,
            Method::GET,
            &format!("/internal/v1/operator/tenants/{}/credentials", c.tenant_id),
            None,
            None,
            None,
            &headers(None, false),
        )
        .await;
    assert_eq!(read.body["items"][0]["version"], 1, "{}", read.body);

    let k1 = Uuid::new_v4();
    let suspended = send(
        &h,
        &format!("{base}/suspend"),
        json!({ "expectedVersion": 1, "reasonCode": "HQ_REVIEW" }),
        k1,
    )
    .await;
    let s1 = ok(&suspended).clone();
    assert_eq!(s1["status"], "SUSPENDED");
    assert_eq!(s1["version"], "2");
    assert_eq!(s1["resource"]["revision"], 1);
    assert_eq!(s1["resource"]["suspensionReason"], "HQ_REVIEW");
    settles_as(&h, k1, &s1).await;
    rejected_secret(&h, &secret).await;

    // Suspension moved the version: the old one is stale.
    let stale = send(
        &h,
        &format!("{base}/resume"),
        json!({ "expectedVersion": 1 }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(stale.code(), "VERSION_CONFLICT");
    let resumed = send(
        &h,
        &format!("{base}/resume"),
        json!({ "expectedVersion": 2 }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(ok(&resumed)["status"], "ACTIVE");
    assert_eq!(resumed.body["version"], "3");
    assert_eq!(
        h.authenticate(&secret).await.body["outcome"],
        "AUTHENTICATED"
    );

    let k3 = Uuid::new_v4();
    let revoked = send(
        &h,
        &format!("{base}/revoke"),
        json!({ "expectedVersion": 3 }),
        k3,
    )
    .await;
    let r3 = ok(&revoked).clone();
    assert_eq!(r3["status"], "REVOKED");
    assert_eq!(r3["version"], "4");
    settles_as(&h, k3, &r3).await;
    for r in [&suspended, &resumed, &revoked] {
        let text = r.body.to_string();
        assert!(!text.contains(&secret), "never the secret");
        for k in [
            "verifier",
            "verifierAlgorithm",
            "verifierKeyVersion",
            "label",
            "secret",
        ] {
            assert!(!text.contains(&format!("\"{k}\"")), "{k} in {text}");
        }
    }
    // A malformed reason code is refused (nothing done).
    let bad = send(
        &h,
        &format!("{base}/suspend"),
        json!({ "expectedVersion": 4, "reasonCode": "lower" }),
        Uuid::new_v4(),
    )
    .await;
    assert!(bad.status.is_client_error(), "{}", bad.body);
}

async fn rejected_secret(h: &Harness, secret: &str) {
    let r = h.authenticate(secret).await;
    assert_eq!(r.body["outcome"], "REJECTED", "{}", r.body);
}

#[tokio::test]
async fn scope_grants_and_privileged_scope_grants_change_through_their_own_route() {
    let Some(h) = harness().await else {
        return;
    };
    let c = h.consumer("bridge-scopes", "FIRST_PARTY", &[]).await;
    let app = c.application_id;
    let normal = format!("/internal/v1/operator/applications/{app}/scope-grants");
    let privileged = format!("/internal/v1/operator/applications/{app}/privileged-scope-grants");

    let k = Uuid::new_v4();
    let granted = send(
        &h,
        &normal,
        json!({ "action": "GRANT", "scope": "calls.read", "environmentKinds": ["SANDBOX"] }),
        k,
    )
    .await;
    let g = ok(&granted).clone();
    assert_eq!(g["status"], "GRANTED");
    assert_eq!(g["version"], Value::Null);
    assert_eq!(g["resource"]["granted"], true);
    assert_eq!(g["resource"]["allowedEnvironmentKinds"], json!(["SANDBOX"]));
    settles_as(&h, k, &g).await;
    // The same key for another scope of the same application: refused.
    let other = send(
        &h,
        &normal,
        json!({ "action": "GRANT", "scope": "calls.write" }),
        k,
    )
    .await;
    assert_eq!(other.status, StatusCode::CONFLICT, "{}", other.body);

    let revoked = send(
        &h,
        &normal,
        json!({ "action": "REVOKE", "scope": "calls.read" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(ok(&revoked)["status"], "REVOKED");
    assert_eq!(revoked.body["resource"]["granted"], false);

    // numbers.admin only through the privileged route, and nothing else through it.
    let wrong = send(
        &h,
        &normal,
        json!({ "action": "GRANT", "scope": "numbers.admin" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(
        wrong.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        wrong.body
    );
    let wrong = send(
        &h,
        &privileged,
        json!({ "action": "GRANT", "scope": "calls.read" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(
        wrong.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        wrong.body
    );
    let unknown = send(
        &h,
        &normal,
        json!({ "action": "GRANT", "scope": "no.such-scope" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{}", unknown.body);
    let unapproved = h
        .call_with_headers(
            &h.peers.bridge,
            Method::POST,
            &privileged,
            Some(json!({ "action": "GRANT", "scope": "numbers.admin" })),
            None,
            None,
            &headers(Some(Uuid::new_v4()), false),
        )
        .await;
    assert_eq!(unapproved.status, StatusCode::UNPROCESSABLE_ENTITY);

    let pk = Uuid::new_v4();
    let p = send(
        &h,
        &privileged,
        json!({ "action": "GRANT", "scope": "numbers.admin" }),
        pk,
    )
    .await;
    let p = ok(&p).clone();
    assert_eq!(p["status"], "GRANTED");
    assert_eq!(p["resource"]["grantBasis"], "PRIVILEGED_OPERATOR");
    settles_as(&h, pk, &p).await;
    let p = send(
        &h,
        &privileged,
        json!({ "action": "REVOKE", "scope": "numbers.admin" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(ok(&p)["status"], "REVOKED");
}

#[tokio::test]
async fn identity_records_are_registered_set_and_removed_without_their_reference() {
    let Some(h) = harness().await else {
        return;
    };
    let c = h.consumer("bridge-records", "EXTERNAL", &[]).await;
    let env = c.env("SANDBOX");
    let reference = "partner-user-ada@example.test";

    let k = Uuid::new_v4();
    let path = format!("/internal/v1/operator/environments/{env}/principals");
    let body = json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "externalReference": reference });
    let registered = send(&h, &path, body.clone(), k).await;
    let r = ok(&registered).clone();
    assert_eq!(r["status"], "REGISTERED");
    assert_eq!(r["version"], "1");
    assert!(!registered.body.to_string().contains(reference));
    settles_as(&h, k, &r).await;
    let again = send(&h, &path, body, k).await;
    assert_eq!(ok(&again), &r);
    assert_eq!(h.scalar_principals(reference).await, 1);
    let id = uuid(&r["resource"]["id"]);
    assert_eq!(
        count(
            &h,
            format!(
                "SELECT count(*) FROM platform_access_admin_operations WHERE resulting_id = '{id}' \
                 AND subject_detail IS NULL"
            )
        )
        .await,
        1
    );

    let set = send(
        &h,
        &format!("/internal/v1/operator/principals/{id}/status"),
        json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(ok(&set)["status"], "SUSPENDED");
    assert_eq!(set.body["version"], "2");

    // Sign-out of a partner user (unversioned).
    let so_key = Uuid::new_v4();
    let so = send(
        &h,
        &format!("/internal/v1/operator/principals/{id}/sign-out"),
        json!({}),
        so_key,
    )
    .await;
    let so = ok(&so).clone();
    assert_eq!(so["status"], "SIGNED_OUT");
    assert_eq!(so["version"], Value::Null);
    assert!(so["resource"]["tokensNotBefore"].is_string());
    settles_as(&h, so_key, &so).await;
    let versioned = send(
        &h,
        &format!("/internal/v1/operator/principals/{id}/sign-out"),
        json!({ "expectedVersion": 2 }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(
        versioned.status,
        StatusCode::BAD_REQUEST,
        "unversioned body"
    );

    let rk = Uuid::new_v4();
    let removed = send(
        &h,
        &format!("/internal/v1/operator/principals/{id}/remove"),
        json!({ "expectedVersion": 2 }),
        rk,
    )
    .await;
    let rm = ok(&removed).clone();
    assert_eq!(rm["status"], "TOMBSTONED");
    assert_eq!(rm["version"], "3");
    settles_as(&h, rk, &rm).await;
    assert_eq!(h.scalar_principals(reference).await, 0, "reference erased");
    // A tombstoned principal is no partner user to sign out.
    let gone = send(
        &h,
        &format!("/internal/v1/operator/principals/{id}/sign-out"),
        json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.body);
}

#[tokio::test]
async fn an_allowed_website_is_removed_at_its_version() {
    let Some(h) = harness().await else {
        return;
    };
    let c = h.consumer("bridge-sites", "EXTERNAL", &[]).await;
    let env = c.env("SANDBOX");
    let set = h
        .call_with_headers(
            &h.peers.developer_platform,
            Method::PUT,
            &format!("/internal/v1/delegation/environments/{env}/allowed-origins"),
            Some(json!({ "origins": ["https://a.example.com", "https://b.example.com"] })),
            None,
            None,
            &[("x-cf-delegated-actor", format!("u-{}", "a".repeat(32)))],
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    let path = format!("/internal/v1/operator/environments/{env}/origins/remove");

    let k = Uuid::new_v4();
    let removed = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "origin": "https://b.example.com" }),
        k,
    )
    .await;
    let r = ok(&removed).clone();
    assert_eq!(r["status"], "REMOVED");
    assert_eq!(r["version"], "2");
    assert_eq!(r["resource"]["originCount"], 1);
    settles_as(&h, k, &r).await;
    let stale = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "origin": "https://a.example.com" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(stale.code(), "VERSION_CONFLICT");
    let absent = send(
        &h,
        &path,
        json!({ "expectedVersion": 2, "origin": "https://b.example.com" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(absent.status, StatusCode::NOT_FOUND, "{}", absent.body);
    let none = send(
        &h,
        &format!(
            "/internal/v1/operator/environments/{}/origins/remove",
            c.env("TEST")
        ),
        json!({ "expectedVersion": 1, "origin": "https://a.example.com" }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND, "{}", none.body);
}

#[tokio::test]
async fn an_app_grant_is_revoked_by_staff() {
    let Some(h) = harness().await else {
        return;
    };
    let p = h.consumer("cfmeet-personal", "FIRST_PARTY", &[]).await;
    let dev = h.consumer("bridge-apps", "EXTERNAL", &[]).await;
    let app = dev.application_id;
    let profile = h
        .call(
            &h.peers.developer_platform,
            Method::PUT,
            &format!("/internal/v1/oauth/clients/{app}"),
            Some(json!({
                "name": "Calendar Sync",
                "redirectUris": ["https://app.example.com/callback"],
                "userScopes": ["meetings:read", "meetings:write", "chat:read", "profile:read"],
                "botScopes": ["chat:read:bot", "chat:write:bot", "events:meetings"],
                "distribution": "PUBLIC",
                "status": "PUBLISHED",
            })),
            None,
            None,
        )
        .await;
    assert_eq!(profile.status, StatusCode::CREATED, "{}", profile.body);
    let auth = h
        .call(
            &h.peers.meeting,
            Method::POST,
            "/internal/v1/oauth/authorizations",
            Some(json!({
                "clientId": app,
                "redirectUri": "https://app.example.com/callback",
                "scope": "meetings:read",
                "codeChallenge": "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "codeChallengeMethod": "S256",
                "target": "USER",
                "accountId": Uuid::new_v4(),
                "decision": "APPROVE",
            })),
            Some(p.delegated("PRODUCTION")),
            None,
        )
        .await;
    assert_eq!(auth.status, StatusCode::CREATED, "{}", auth.body);
    let grant = uuid(&auth.body["grantId"]);

    let path = format!("/internal/v1/operator/app-grants/{grant}/revoke");
    let stale = send(&h, &path, json!({ "expectedVersion": 9 }), Uuid::new_v4()).await;
    assert_eq!(stale.code(), "VERSION_CONFLICT");
    let k = Uuid::new_v4();
    let revoked = send(&h, &path, json!({ "expectedVersion": 1 }), k).await;
    let r = ok(&revoked).clone();
    assert_eq!(r["status"], "REVOKED");
    assert_eq!(r["version"], "2");
    assert_eq!(r["resource"]["revokeReason"], "OPERATOR_REVOKED");
    settles_as(&h, k, &r).await;
    assert_eq!(
        count(
            &h,
            format!(
                "SELECT count(*) FROM platform_access_audit WHERE action = 'APP_GRANT_REVOKED' \
                 AND target_kind = 'APP_GRANT' AND target_id = '{grant}'"
            )
        )
        .await,
        1
    );
    let unknown = send(
        &h,
        &format!("/internal/v1/operator/app-grants/{}/revoke", Uuid::new_v4()),
        json!({ "expectedVersion": 1 }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn flagged_commands_are_unknown_with_their_flag_off() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("bridge-flags", "EXTERNAL", &[]).await;
    let id = Uuid::new_v4();
    for (path, body) in [
        (
            format!("/internal/v1/operator/principals/{id}/sign-out"),
            json!({}),
        ),
        (
            format!("/internal/v1/operator/app-grants/{id}/revoke"),
            json!({ "expectedVersion": 1 }),
        ),
        (
            format!(
                "/internal/v1/operator/environments/{}/origins/remove",
                c.env("SANDBOX")
            ),
            json!({ "expectedVersion": 1, "origin": "https://a.example.com" }),
        ),
    ] {
        let r = send(&h, &path, body, Uuid::new_v4()).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.body);
    }
}

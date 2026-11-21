//! ADR 0068 A4 (brief §1): API keys issued and rotated by the admin bridge are
//! OWNER-DELIVERED.
//!
//! * `POST /internal/v1/operator/service-accounts/{serviceAccountId}/credentials`
//!   (`identity.key.issue`, R4)
//! * `POST /internal/v1/operator/credentials/{credentialId}/rotate` (`identity.key.rotate`,
//!   R4)
//! * `GET /internal/v1/delegation/environments/{environmentId}/credential-deliveries`
//! * `POST /internal/v1/delegation/environments/{environmentId}/credential-deliveries/{deliveryRef}/reveal`
//!
//! The bridge answer, the ledger, the audit trail and the HQ reads carry the delivery
//! reference only; the secret exists only sealed until the partner's ONE reveal through
//! the Developer Platform console (a human member), after which (and after expiry or a
//! revocation) the sealed copy is gone. Nothing is ever resent.

mod common;

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::*;
use platform_access::domain::Clock;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const HQ: &str = "hq:staff-0001";
const APPROVER: &str = "hq:staff-0002";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const MEMBER: &str = "u-0123456789abcdef0123456789abcdef";
const MACHINE: &str = "s-0123456789abcdef0123456789abcdef";

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

async fn console(h: &Harness, method: Method, path: &str, actor: &str) -> Reply {
    h.call_with_headers(
        &h.peers.developer_platform,
        method,
        path,
        None,
        None,
        None,
        &[("x-cf-delegated-actor", actor.to_string())],
    )
    .await
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

async fn scalar(h: &Harness, sql: String) -> i64 {
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

/// Everything HQ or the ledger could ever hold about delivered keys, as one text.
async fn hq_visible(h: &Harness) -> String {
    text(
        h,
        "SELECT coalesce(string_agg(row_to_json(o)::text, ' '), '') FROM ( \
           SELECT * FROM platform_access_admin_operations) o"
            .into(),
    )
    .await
        + &text(
            h,
            "SELECT coalesce(string_agg(row_to_json(a)::text, ' '), '') FROM ( \
               SELECT * FROM platform_access_audit) a"
                .into(),
        )
        .await
}

#[track_caller]
fn answered(r: &Reply) -> &Value {
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body.as_object().unwrap().len(), 3, "{}", r.body);
    let resource = &r.body["resource"];
    assert_eq!(resource["delivery"], "OWNER_DELIVERED");
    assert_eq!(resource["deliveryChannel"], "CONSOLE_REVEAL_ONCE");
    assert!(Uuid::parse_str(resource["deliveryRef"].as_str().unwrap()).is_ok());
    assert!(resource["expiresAt"].is_string());
    assert_eq!(resource["last4"].as_str().unwrap().len(), 4);
    // Never the secret, a token, a sealed copy or a URL.
    let body = r.body.to_string();
    for banned in ["cfmk_", "secret", "sealed", "nonce", "http"] {
        assert!(!body.contains(banned), "{banned} in {body}");
    }
    &r.body
}

async fn issue_path(sa: Uuid) -> String {
    format!("/internal/v1/operator/service-accounts/{sa}/credentials")
}

/// The partner's one reveal through their console.
async fn reveal(h: &Harness, env: Uuid, delivery: &Value, actor: &str) -> Reply {
    console(
        h,
        Method::POST,
        &format!(
            "/internal/v1/delegation/environments/{env}/credential-deliveries/{}/reveal",
            delivery.as_str().unwrap()
        ),
        actor,
    )
    .await
}

#[tokio::test]
async fn a_key_issue_is_refused_before_anything_is_done() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("deliver-refusals", "EXTERNAL", &["calls.read"])
        .await;
    let sa = c.sa("SANDBOX");
    let path = issue_path(sa).await;
    let body = json!({ "expectedVersion": 1, "tenantId": c.tenant_id, "scopes": ["calls.read"] });
    let key = Uuid::new_v4();

    // The operator console and a caller without the permission.
    let op = h
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
    assert_eq!(op.code(), "OPERATOR_REQUIRED", "{}", op.body);
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
    assert_eq!(calling.status, StatusCode::FORBIDDEN);
    // No key, no approval, a stale version, an unknown field, another tenant.
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
        json!({ "expectedVersion": 4, "tenantId": c.tenant_id, "scopes": ["calls.read"] }),
        key,
    )
    .await;
    assert_eq!(stale.code(), "VERSION_CONFLICT", "{}", stale.body);
    let unknown = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "tenantId": c.tenant_id, "scopes": [], "secret": "x" }),
        key,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let other = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "tenantId": Uuid::new_v4(), "scopes": [] }),
        key,
    )
    .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.body);
    // No partner console: a FIRST_PARTY tenant, an operator-created PRODUCTION environment.
    let fp = h.consumer("deliver-first-party", "FIRST_PARTY", &[]).await;
    let first_party = send(
        &h,
        &issue_path(fp.sa("SANDBOX")).await,
        json!({ "expectedVersion": 1, "tenantId": fp.tenant_id, "scopes": [] }),
        key,
    )
    .await;
    assert_eq!(
        first_party.code(),
        "POLICY_VIOLATION",
        "{}",
        first_party.body
    );
    let production = send(
        &h,
        &issue_path(c.sa("PRODUCTION")).await,
        json!({ "expectedVersion": 1, "tenantId": c.tenant_id, "scopes": [] }),
        key,
    )
    .await;
    assert_eq!(production.code(), "POLICY_VIOLATION", "{}", production.body);

    for table in [
        "platform_api_credentials",
        "platform_credential_deliveries",
        "platform_access_admin_operations",
    ] {
        assert_eq!(scalar(&h, format!("SELECT count(*) FROM {table}")).await, 0);
    }
    assert_eq!(record(&h, key).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_issued_key_reaches_only_the_partner_console_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("deliver-issue", "EXTERNAL", &["calls.read"])
        .await;
    let (env, sa) = (c.env("SANDBOX"), c.sa("SANDBOX"));
    let key = Uuid::new_v4();
    let body = json!({ "expectedVersion": 1, "tenantId": c.tenant_id, "scopes": ["calls.read"] });
    let r = send(&h, &issue_path(sa).await, body.clone(), key).await;
    let a = answered(&r).clone();
    assert_eq!(a["status"], "ISSUED");
    assert_eq!(a["version"], "1");
    let res = &a["resource"];
    assert_eq!(res["state"], "ACTIVE");
    assert_eq!(res["deliveryState"], "PENDING");
    assert_eq!(res["serviceAccountId"], sa.to_string());
    assert_eq!(res["tenantId"], c.tenant_id.to_string());
    assert!(res["rotatedFromCredentialId"].is_null());
    let credential = uuid(&res["id"]);
    let delivery = res["deliveryRef"].clone();

    // Settlement and replay: the same answer, nothing issued twice; the key reused for
    // another command is a conflict.
    let rec = record(&h, key).await;
    assert_eq!(rec.body["source_status"], "ISSUED");
    assert_eq!(rec.body["source_version"], "1");
    let again = send(&h, &issue_path(sa).await, body.clone(), key).await;
    assert_eq!(answered(&again)["resource"]["id"], res["id"]);
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM platform_api_credentials".into()).await,
        1
    );
    let reused = send(
        &h,
        &format!("/internal/v1/operator/credentials/{credential}/revoke"),
        json!({ "expectedVersion": 1 }),
        key,
    )
    .await;
    assert_eq!(reused.code(), "CONFLICT");

    // The ledger names the staff member and the approver; neither it nor the audit trail
    // holds a secret; the delivery row holds only a sealed copy.
    assert_eq!(
        text(
            &h,
            format!(
                "SELECT operator_ref || ' ' || approver_ref || ' ' || subject_kind || ' ' || \
                   resulting_id FROM platform_access_admin_operations WHERE idempotency_key = '{key}'"
            )
        )
        .await,
        format!("{HQ} {APPROVER} KEY_ISSUE {credential}")
    );
    let visible = hq_visible(&h).await;
    assert!(!visible.contains("cfmk_"), "{visible}");
    assert!(visible.contains(delivery.as_str().unwrap()));

    // The console lists the pending delivery, without a secret.
    let pending = console(
        &h,
        Method::GET,
        &format!("/internal/v1/delegation/environments/{env}/credential-deliveries"),
        MEMBER,
    )
    .await;
    assert_eq!(pending.status, StatusCode::OK, "{}", pending.body);
    assert_eq!(pending.body["deliveries"][0]["deliveryRef"], delivery);
    assert_eq!(pending.body["deliveries"][0]["state"], "PENDING");
    assert!(!pending.body.to_string().contains("cfmk_"));

    // Only a signed-in human member reveals it; never another environment, a machine
    // credential or the bridge.
    let machine = reveal(&h, env, &delivery, MACHINE).await;
    assert_eq!(machine.code(), "POLICY_VIOLATION", "{}", machine.body);
    let elsewhere = reveal(&h, c.env("TEST"), &delivery, MEMBER).await;
    assert_eq!(elsewhere.status, StatusCode::NOT_FOUND);
    let bridge = h
        .call_with_headers(
            &h.peers.bridge,
            Method::POST,
            &format!(
                "/internal/v1/delegation/environments/{env}/credential-deliveries/{}/reveal",
                delivery.as_str().unwrap()
            ),
            None,
            None,
            None,
            &[("x-cf-delegated-actor", MEMBER.to_string())],
        )
        .await;
    assert_eq!(bridge.status, StatusCode::FORBIDDEN);

    let shown = reveal(&h, env, &delivery, MEMBER).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    assert_eq!(
        shown
            .headers
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap(),
        "no-store"
    );
    let secret = shown.body["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("cfmk_"));
    assert!(secret.ends_with(res["last4"].as_str().unwrap()));
    assert_eq!(shown.body["credential"]["id"], res["id"]);
    assert_eq!(
        h.authenticate(&secret).await.body["outcome"],
        "AUTHENTICATED"
    );

    // Once: the second reveal finds nothing, the sealed copy is gone, the reveal is
    // audited, and HQ sees REVEALED (in its read and in a replayed answer).
    let twice = reveal(&h, env, &delivery, MEMBER).await;
    assert_eq!(twice.status, StatusCode::NOT_FOUND);
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM platform_credential_deliveries \
             WHERE sealed IS NOT NULL OR nonce IS NOT NULL"
                .into()
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &h,
            format!(
                "SELECT count(*) FROM platform_access_audit \
                 WHERE action = 'CREDENTIAL_DELIVERY_REVEALED' AND actor = 'devplat.{MEMBER}' \
                   AND target_id = '{credential}'"
            )
        )
        .await,
        1
    );
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
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    let item = &read.body["items"][0];
    assert_eq!(item["delivery"]["deliveryState"], "REVEALED");
    assert_eq!(item["delivery"]["deliveryRef"], delivery);
    assert!(item["delivery"]["revealedAt"].is_string());
    let replay = send(&h, &issue_path(sa).await, body, key).await;
    assert_eq!(answered(&replay)["resource"]["deliveryState"], "REVEALED");
    assert_eq!(replay.body["status"], "ISSUED");
    let after = console(
        &h,
        Method::GET,
        &format!("/internal/v1/delegation/environments/{env}/credential-deliveries"),
        MEMBER,
    )
    .await;
    assert_eq!(after.body["deliveries"], json!([]));
    assert!(!hq_visible(&h).await.contains(&secret));
}

#[tokio::test]
async fn a_rotated_key_keeps_the_predecessor_working_until_the_partner_has_the_successor() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("deliver-rotate", "EXTERNAL", &["calls.read"])
        .await;
    let env = c.env("TEST");
    let (old, old_secret) = h.issue(c.sa("TEST"), &["calls.read"]).await;
    let old_id = uuid(&old["id"]);
    let path = format!("/internal/v1/operator/credentials/{old_id}/rotate");

    let stale = send(
        &h,
        &path,
        json!({ "expectedVersion": 2, "tenantId": c.tenant_id }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(stale.code(), "VERSION_CONFLICT");
    let negative = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "tenantId": c.tenant_id, "overlapMinutes": -1 }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(negative.code(), "VALIDATION_ERROR");

    let key = Uuid::new_v4();
    let r = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "tenantId": c.tenant_id }),
        key,
    )
    .await;
    let a = answered(&r).clone();
    assert_eq!(a["status"], "ROTATED");
    assert_eq!(a["version"], "1");
    assert_eq!(a["resource"]["rotatedFromCredentialId"], old_id.to_string());
    assert_ne!(a["resource"]["id"], old["id"]);
    assert_eq!(record(&h, key).await.body["source_status"], "ROTATED");
    // The rotated key moved (its old version is stale now) and still works for the
    // delivery window.
    let again = send(
        &h,
        &path,
        json!({ "expectedVersion": 1, "tenantId": c.tenant_id }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(again.code(), "VERSION_CONFLICT");
    h.clock.advance(Duration::days(6));
    assert_eq!(
        h.authenticate(&old_secret).await.body["outcome"],
        "AUTHENTICATED"
    );
    let shown = reveal(&h, env, &a["resource"]["deliveryRef"], MEMBER).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    let secret = shown.body["secret"].as_str().unwrap();
    assert_eq!(
        h.authenticate(secret).await.body["outcome"],
        "AUTHENTICATED"
    );
    assert!(!hq_visible(&h).await.contains(secret));
    assert_eq!(
        scalar(
            &h,
            format!(
                "SELECT count(*) FROM platform_access_audit WHERE action = 'CREDENTIAL_ROTATED' \
                 AND target_id = '{old_id}' AND actor = 'cfmeet.admin-bridge' \
                 AND correlation_id = 'hq_{}'",
                key.simple()
            )
        )
        .await,
        1
    );
}

#[tokio::test]
async fn an_unrevealed_key_is_destroyed_at_expiry_or_revocation_and_never_resent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("deliver-expiry", "EXTERNAL", &["calls.read"])
        .await;
    let (env, sa) = (c.env("SANDBOX"), c.sa("SANDBOX"));
    let first = send(
        &h,
        &issue_path(sa).await,
        json!({ "expectedVersion": 1, "tenantId": c.tenant_id, "scopes": [] }),
        Uuid::new_v4(),
    )
    .await;
    let first = answered(&first).clone();
    let second = send(
        &h,
        &issue_path(sa).await,
        json!({ "expectedVersion": 1, "tenantId": c.tenant_id, "scopes": [] }),
        Uuid::new_v4(),
    )
    .await;
    let second = answered(&second).clone();

    // A revoked key's copy goes at the next sweep.
    let revoked = send(
        &h,
        &format!(
            "/internal/v1/operator/credentials/{}/revoke",
            second["resource"]["id"].as_str().unwrap()
        ),
        json!({ "expectedVersion": 1 }),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    let now = h.clock.now();
    assert_eq!(
        platform_access::identity::credential_delivery::purge_expired(h.conn(), now)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        reveal(&h, env, &second["resource"]["deliveryRef"], MEMBER)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Past the window: the reveal finds nothing and the copy is destroyed.
    h.clock.advance(Duration::days(7));
    assert_eq!(
        reveal(&h, env, &first["resource"]["deliveryRef"], MEMBER)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM platform_credential_deliveries \
             WHERE state = 'EXPIRED' AND sealed IS NULL AND nonce IS NULL"
                .into()
        )
        .await,
        2
    );
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
    for item in read.body["items"].as_array().unwrap() {
        assert_eq!(item["delivery"]["deliveryState"], "EXPIRED", "{item}");
    }
    // The database itself refuses a sealed copy outside PENDING.
    let forged = h
        .conn()
        .execute_unprepared(
            "UPDATE platform_credential_deliveries SET sealed = '\\x00', nonce = '\\x00'",
        )
        .await;
    assert!(forged.is_err());
}

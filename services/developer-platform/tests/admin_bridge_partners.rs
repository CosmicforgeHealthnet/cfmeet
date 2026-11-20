//! ADR 0068 A4 — partner commands and reads relayed by the HQ admin bridge, through the
//! REAL router (CI: real PostgreSQL): workspace state, support tier, incidents, usage
//! reprocessing and reads, and the operation record of each command.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const OPERATOR: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVER: (&str, &str) = ("x-cf-approver", "hq:staff-0002");
const APPROVAL: (&str, &str) = ("x-cf-approval-ref", "appr-1");
const DIGEST: (&str, &str) = (
    "x-cf-intent-digest",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
);

async fn org(c: &Control, email: &str) -> Uuid {
    let (owner, _) = c.sessions.sign_in(email);
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    Uuid::parse_str(r.json["id"].as_str().unwrap()).unwrap()
}

async fn org_version(c: &Control, o: Uuid) -> i64 {
    let r = c
        .bridge(
            &format!("/internal/v1/operator/organizations/{o}"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json["version"].as_i64().unwrap()
}

fn standard(r: &Response, status: &str) {
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AdminCommandResult"), &r.json);
    assert_eq!(r.json["status"], status, "{}", r.json);
    let text = r.json.to_string();
    assert!(
        !text.contains("@"),
        "no address in a command answer: {text}"
    );
}

async fn record(c: &Control, key: &str) -> Value {
    let r = c
        .bridge(
            &format!("/internal/v1/operator/operations/{key}"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AdminOperationRecord"), &r.json);
    r.json
}

#[tokio::test]
async fn hq_suspends_a_workspace_with_two_people_at_its_version() {
    let Some(c) = Control::new().await else {
        return;
    };
    let o = org(&c, "owner@state.example").await;
    let path = format!("/internal/v1/operator/organizations/{o}/state");
    let v = org_version(&c, o).await;
    let key = Uuid::new_v4().to_string();
    let body = json!({ "state": "SUSPENDED", "reason": "ABUSE_REPORTED", "expectedVersion": v });
    let two = [
        OPERATOR,
        APPROVER,
        APPROVAL,
        DIGEST,
        ("idempotency-key", key.as_str()),
    ];
    // R3: no approval → refused, nothing done.
    let r = c
        .bridge_send(
            Method::POST,
            &path,
            &[OPERATOR, ("idempotency-key", &key)],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    // No key / no version / a stale version.
    let r = c
        .bridge_send(
            Method::POST,
            &path,
            &[OPERATOR, APPROVER, APPROVAL, DIGEST],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let r = c
        .bridge_send(
            Method::POST,
            &path,
            &two,
            Some(json!({ "state": "SUSPENDED", "reason": "ABUSE_REPORTED" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let r = c
        .bridge_send(
            Method::POST,
            &path,
            &two,
            Some(json!({ "state": "SUSPENDED", "reason": "ABUSE_REPORTED", "expectedVersion": v + 5 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(
        c.pa.tenant_status(o),
        "ACTIVE",
        "refused before Platform Access"
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_admin_operations")
            .await,
        0
    );
    // Applied once.
    let r = c
        .bridge_send(Method::POST, &path, &two, Some(body.clone()))
        .await;
    standard(&r, "SUSPENDED");
    assert_eq!(r.json["version"], (v + 1).to_string());
    assert_eq!(r.json["resource"]["organizationId"], json!(o));
    assert_eq!(c.pa.tenant_status(o), "SUSPENDED");
    let events = c
        .count(&format!(
            "SELECT count(*) FROM devplat_activity_events WHERE tenant_id = '{o}' AND action = 'organization.state_changed'"
        ))
        .await;
    // The same key again: answered, never applied twice.
    let again = c
        .bridge_send(Method::POST, &path, &two, Some(body.clone()))
        .await;
    standard(&again, "SUSPENDED");
    assert_eq!(again.json["version"], r.json["version"]);
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_activity_events WHERE tenant_id = '{o}' AND action = 'organization.state_changed'"
        ))
        .await,
        events
    );
    // The same key on another command: a conflict.
    let other = c
        .bridge_send(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/support-profile"),
            &[OPERATOR, ("idempotency-key", &key)],
            Some(json!({ "supportTier": "PREMIUM", "expectedVersion": 0 })),
        )
        .await;
    assert_eq!(other.status, StatusCode::CONFLICT, "{}", other.json);
    // The settlement record matches the answer.
    let rec = record(&c, &key).await;
    assert_eq!(rec["source_status"], "SUSPENDED");
    assert_eq!(rec["source_version"], r.json["version"]);
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_admin_operations WHERE idempotency_key = '{key}' \
             AND subject_kind = 'ORGANIZATION' AND approver_ref = 'hq:staff-0002'"
        ))
        .await,
        1
    );
    // The operator console keeps its own shape (no key, no approval).
    let console = c
        .operator(
            Method::POST,
            &path,
            Some(json!({ "state": "ACTIVE", "reason": "REVIEWED" })),
        )
        .await;
    assert_eq!(console.status, StatusCode::OK, "{}", console.json);
    assert_eq!(console.json["state"], "ACTIVE");
    assert_eq!(c.pa.tenant_status(o), "ACTIVE");
    // The console has no bridge records to read.
    let r = c
        .operator(
            Method::GET,
            &format!("/internal/v1/operator/operations/{key}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hq_sets_the_support_tier_and_records_incidents() {
    let Some(c) = Control::new().await else {
        return;
    };
    let o = org(&c, "owner@support.example").await;
    let tier = format!("/internal/v1/operator/organizations/{o}/support-profile");
    let incidents = format!("/internal/v1/operator/organizations/{o}/incidents");
    // R2: no approval; a profile never set is at version 0.
    let k1 = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &tier,
            &[OPERATOR],
            Some(json!({ "supportTier": "PREMIUM", "expectedVersion": 0 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "no key: {}", r.json);
    let r = c
        .bridge_send(
            Method::POST,
            &tier,
            &[OPERATOR, ("idempotency-key", &k1)],
            Some(json!({ "supportTier": "PREMIUM", "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "no profile at version 1: {}",
        r.json
    );
    let r = c
        .bridge_send(
            Method::POST,
            &tier,
            &[OPERATOR, ("idempotency-key", &k1)],
            Some(json!({ "supportTier": "PREMIUM", "expectedVersion": 0 })),
        )
        .await;
    standard(&r, "PREMIUM");
    let v: i64 = r.json["version"].as_str().unwrap().parse().unwrap();
    assert_eq!(r.json["resource"]["supportTier"], "PREMIUM");
    let replay = c
        .bridge_send(
            Method::POST,
            &tier,
            &[OPERATOR, ("idempotency-key", &k1)],
            Some(json!({ "supportTier": "PREMIUM", "expectedVersion": 0 })),
        )
        .await;
    standard(&replay, "PREMIUM");
    assert_eq!(replay.json["version"], r.json["version"]);
    assert_eq!(record(&c, &k1).await["source_status"], "PREMIUM");
    // Version 0 again: the profile exists now.
    let r = c
        .bridge_send(
            Method::POST,
            &tier,
            &[OPERATOR, ("idempotency-key", &Uuid::new_v4().to_string())],
            Some(json!({ "supportTier": "ENTERPRISE", "expectedVersion": 0 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // An incident at the current version.
    let k2 = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &incidents,
            &[OPERATOR, ("idempotency-key", &k2)],
            Some(
                json!({ "incidentRef": "INC-2026-11-20-1", "occurredAt": "2026-11-20T08:00:00Z",
                         "expectedVersion": v }),
            ),
        )
        .await;
    standard(&r, "RECORDED");
    assert_eq!(
        r.json["resource"]["lastIncident"]["reference"],
        "INC-2026-11-20-1"
    );
    let rec = record(&c, &k2).await;
    assert_eq!(rec["source_status"], "RECORDED");
    assert_eq!(rec["source_version"], r.json["version"]);
    // A stale version.
    let r = c
        .bridge_send(
            Method::POST,
            &incidents,
            &[OPERATOR, ("idempotency-key", &Uuid::new_v4().to_string())],
            Some(
                json!({ "incidentRef": "INC-2", "occurredAt": "2026-11-20T09:00:00Z",
                         "expectedVersion": v }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // Unknown fields are refused.
    let r = c
        .bridge_send(
            Method::POST,
            &incidents,
            &[OPERATOR, ("idempotency-key", &Uuid::new_v4().to_string())],
            Some(
                json!({ "incidentRef": "INC-3", "occurredAt": "2026-11-20T09:00:00Z",
                         "expectedVersion": v + 1, "note": "x" }),
            ),
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.json);
    // The console keeps PUT and its own shape.
    let console = c
        .operator(
            Method::PUT,
            &tier,
            Some(json!({ "supportTier": "STANDARD" })),
        )
        .await;
    assert_eq!(console.status, StatusCode::OK, "{}", console.json);
    assert_eq!(console.json["supportTier"], "STANDARD");

    // The incidents read: every record, newest first, filterable by workspace.
    let list = c
        .bridge(
            &format!("/internal/v1/operator/incidents?organizationId={o}"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    conforms(&format!("{CTRL}OperatorIncidentList"), &list.json);
    assert_eq!(list.json["items"][0]["incidentRef"], "INC-2026-11-20-1");
    assert_eq!(list.json["items"][0]["organizationId"], json!(o));
    assert!(!list.json.to_string().contains("hq:staff"), "no operator");
    let all = c
        .bridge("/internal/v1/operator/incidents?limit=1", &[OPERATOR])
        .await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.json);
    for bad in [format!(
        "/internal/v1/operator/incidents?organizationId={}",
        Uuid::new_v4()
    )] {
        assert_eq!(
            c.bridge(&bad, &[OPERATOR]).await.status,
            StatusCode::NOT_FOUND
        );
    }
    for bad in [
        "/internal/v1/operator/incidents?limit=0",
        "/internal/v1/operator/incidents?cursor=nope",
        "/internal/v1/operator/incidents?since=2026-10-01T00:00:00Z&until=2026-11-20T00:00:00Z",
        "/internal/v1/operator/incidents?state=OPEN",
    ] {
        assert_eq!(
            c.bridge(bad, &[OPERATOR]).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn hq_reprocesses_usage_and_reads_a_workspace_usage() {
    let Some(c) = Control::new().await else {
        return;
    };
    let o = org(&c, "owner@usage.example").await;
    let path = "/internal/v1/operator/usage/rollups/rebuild";
    let body = json!({ "from": "2026-11-01", "to": "2026-11-19" });
    // Unversioned: an expectedVersion is refused, the key is required.
    let r = c
        .bridge_send(Method::POST, path, &[OPERATOR], Some(body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            path,
            &[OPERATOR, ("idempotency-key", &key)],
            Some(json!({ "from": "2026-11-01", "to": "2026-11-19", "expectedVersion": 1 })),
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.json);
    let r = c
        .bridge_send(
            Method::POST,
            path,
            &[OPERATOR, ("idempotency-key", &key)],
            Some(body.clone()),
        )
        .await;
    standard(&r, "REPROCESSED");
    assert!(r.json["version"].is_null());
    assert_eq!(r.json["resource"]["from"], "2026-11-01");
    let again = c
        .bridge_send(
            Method::POST,
            path,
            &[OPERATOR, ("idempotency-key", &key)],
            Some(body.clone()),
        )
        .await;
    standard(&again, "REPROCESSED");
    assert_eq!(
        c.count(
            "SELECT count(*) FROM devplat_operator_audit WHERE operator = 'hq:staff-0001'              AND action = 'usage.rollups_rebuilt'"
        )
        .await,
        1,
        "audited once, under the HQ staff member"
    );
    let rec = record(&c, &key).await;
    assert_eq!(rec["source_status"], "REPROCESSED");
    assert!(rec["source_version"].is_null());
    // The console keeps its own answer.
    let console = c.operator(Method::POST, path, Some(body)).await;
    assert_eq!(console.status, StatusCode::OK, "{}", console.json);
    assert!(console.json.get("rebuilt").is_some());

    // The workspace's usage over a window.
    let usage = format!("/internal/v1/operator/organizations/{o}/usage");
    let r = c.bridge(&usage, &[OPERATOR]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}OperatorUsageList"), &r.json);
    assert!(r.json["items"].as_array().unwrap().is_empty());
    let r = c
        .bridge(
            &format!("{usage}?since=2026-11-01T00:00:00Z&until=2026-11-20T00:00:00Z&limit=10"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["from"], "2026-11-01");
    assert_eq!(r.json["to"], "2026-11-19");
    for bad in [
        format!("{usage}?since=2026-10-01T00:00:00Z&until=2026-11-20T00:00:00Z"),
        format!("{usage}?cursor=nope"),
        format!("{usage}?limit=101"),
    ] {
        assert_eq!(
            c.bridge(&bad, &[OPERATOR]).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    let missing = c
        .bridge(
            &format!(
                "/internal/v1/operator/organizations/{}/usage",
                Uuid::new_v4()
            ),
            &[OPERATOR],
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bridge_only_commands_refuse_the_console_and_unknown_subjects() {
    let Some(c) = Control::new().await else {
        return;
    };
    let o = org(&c, "owner@only.example").await;
    let (pk, d) = (Uuid::new_v4(), Uuid::new_v4());
    let disable = format!("/internal/v1/operator/organizations/{o}/push-providers/{pk}/disable");
    let retry = format!("/internal/v1/operator/organizations/{o}/webhook-deliveries/{d}/retry");
    let r = c
        .operator(
            Method::POST,
            &disable,
            Some(json!({ "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    let r = c.operator(Method::POST, &retry, Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    // A caller that may not claim an operator at all.
    let r = c
        .internal(&c.peers.gateway, Method::POST, &retry, Some(json!({})))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    // Push credentials are off in this deployment: 404, nothing done.
    let r = c
        .bridge_send(
            Method::POST,
            &disable,
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &Uuid::new_v4().to_string()),
            ],
            Some(json!({ "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    // Webhooks are off in this harness: 404.
    let r = c
        .bridge_send(
            Method::POST,
            &retry,
            &[OPERATOR, ("idempotency-key", &Uuid::new_v4().to_string())],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
}

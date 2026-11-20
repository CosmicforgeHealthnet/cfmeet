//! ADR 0068 A3 — production-access decisions relayed by the HQ admin bridge, and the
//! verification queue HQ reads, through the REAL router (CI: real PostgreSQL).

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

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

/// A partner with a project and an open production-access request: (request, owner token,
/// owner user id).
async fn requested(c: &Control, email: &str) -> (Uuid, String, Uuid) {
    let (owner, owner_id) = c.sessions.sign_in(email);
    let o = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{}/projects", id(&o.json)),
            Some(json!({ "displayName": "Backend" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let project = id(&p.json);
    let e = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/production-access-requests"),
            Some(json!({ "capabilities": ["MEETINGS"], "useCaseSummary": "Appointments" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    (id(&r.json), owner, owner_id)
}

fn decide(rid: Uuid, command: &str) -> String {
    format!("/internal/v1/operator/production-access-requests/{rid}/{command}")
}

fn case(rid: Uuid) -> String {
    format!("/internal/v1/operator/verification/cases/cfmeet:PARTNER_GO_LIVE:{rid}")
}

async fn version(c: &Control, rid: Uuid) -> i64 {
    let r = c.bridge(&case(rid), &[OPERATOR]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json["version"].as_i64().unwrap()
}

#[tokio::test]
async fn hq_claims_and_approves_with_two_people_and_the_command_is_recorded_once() {
    // Named reviewers are configured: they bind the operator console, not HQ.
    let Some(c) =
        Control::with_state(|s| s.with_production_reviewers(vec!["github-alice".into()])).await
    else {
        return;
    };
    let (rid, _, _) = requested(&c, "owner@acme.example").await;

    // The queue shows it OPEN, with hashes for evidence and no partner text.
    let list = c
        .bridge("/internal/v1/operator/verification/cases", &[OPERATOR])
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    conforms(&format!("{CTRL}VerificationCaseList"), &list.json);
    let item = &list.json["items"][0];
    assert_eq!(item["case_id"], format!("cfmeet:PARTNER_GO_LIVE:{rid}"));
    assert_eq!(item["state"], "OPEN");
    assert_eq!(item["approvals_required"], 2);
    assert_eq!(item["evidence"][0]["kind"], "USE_CASE_SUMMARY");
    assert_eq!(item["evidence"][0]["sha256"].as_str().unwrap().len(), 64);
    assert!(!list.json.to_string().contains("Appointments"));
    assert!(item.get("details").is_none(), "details only on read");

    // Claim (R2: no second person).
    let v1 = version(&c, rid).await;
    let claim_key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "review"),
            &[OPERATOR, ("idempotency-key", &claim_key)],
            Some(json!({ "expectedVersion": v1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let read = c.bridge(&case(rid), &[OPERATOR]).await;
    assert_eq!(read.json["state"], "CLAIMED");
    conforms(&format!("{CTRL}VerificationCaseDetail"), &read.json);
    assert_eq!(read.json["assignee_ref"], "hq:staff-0001");
    assert_eq!(
        read.json["details"]["requested_capabilities"],
        json!(["MEETINGS"])
    );
    let v2 = read.json["version"].as_i64().unwrap();
    assert!(v2 > v1, "every transition increments the version");

    // Approve without the second person: refused (nothing done).
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "approve"),
            &[OPERATOR, ("idempotency-key", &key)],
            Some(json!({ "expectedVersion": v2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    // A stale version: VERSION_CONFLICT.
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "approve"),
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
            ],
            Some(json!({ "expectedVersion": v1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // No key: refused.
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "approve"),
            &[OPERATOR, APPROVER, APPROVAL, DIGEST],
            Some(json!({ "expectedVersion": v2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    // Two people, no linked account (UNLINKED, allowed).
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "approve"),
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
            ],
            Some(json!({ "expectedVersion": v2, "capabilities": ["MEETINGS"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["state"], "APPROVED");
    let transitions = c
        .count(&format!(
            "SELECT count(*) FROM devplat_production_access_transitions WHERE request_id = '{rid}'"
        ))
        .await;
    // The same key again: answered, never applied twice.
    let again = c
        .bridge_send(
            Method::POST,
            &decide(rid, "approve"),
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
            ],
            Some(json!({ "expectedVersion": v2, "capabilities": ["MEETINGS"] })),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json);
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_production_access_transitions WHERE request_id = '{rid}'"
        ))
        .await,
        transitions
    );
    // The same key for another command: a conflict.
    let other = c
        .bridge_send(
            Method::POST,
            &decide(rid, "reject"),
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
            ],
            Some(json!({ "expectedVersion": v2 })),
        )
        .await;
    assert_eq!(other.status, StatusCode::CONFLICT, "{}", other.json);

    // The record the bridge settles from.
    let rec = c
        .bridge(
            &format!("/internal/v1/operator/operations/{key}"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(rec.status, StatusCode::OK, "{}", rec.json);
    conforms(&format!("{CTRL}AdminOperationRecord"), &rec.json);
    assert_eq!(rec.json["outcome"], "APPLIED");
    assert_eq!(rec.json["source_status"], "APPROVED");
    assert_eq!(rec.json["idempotency_key"], key);
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_admin_operations WHERE idempotency_key = '{key}' \
             AND staff_link = 'UNLINKED' AND approver_ref = 'hq:staff-0002' AND operator_ref = 'hq:staff-0001'"
        ))
        .await,
        1
    );
    let missing = c
        .bridge(
            &format!("/internal/v1/operator/operations/{}", Uuid::new_v4()),
            &[OPERATOR],
        )
        .await;
    assert_eq!(
        missing.status,
        StatusCode::NOT_FOUND,
        "a key never applied has no record"
    );
    // The console has no bridge records to read.
    let console = c
        .operator(
            Method::GET,
            &format!("/internal/v1/operator/operations/{key}"),
            None,
        )
        .await;
    assert_eq!(console.status, StatusCode::NOT_FOUND);

    // History: staff refs, partner actions without an account id.
    let h = c
        .bridge(&format!("{}/history", case(rid)), &[OPERATOR])
        .await;
    assert_eq!(h.status, StatusCode::OK, "{}", h.json);
    conforms(&format!("{CTRL}VerificationCaseHistory"), &h.json);
    let items = h.json["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["to_state"], "APPROVED");
    assert_eq!(items.last().unwrap()["actor_ref"], "hq:staff-0001");
    assert!(items
        .iter()
        .filter(|i| i["actor_kind"] == "PARTNER")
        .all(|i| i["actor_ref"].is_null()));
}

#[tokio::test]
async fn a_linked_member_of_the_partner_cannot_decide_its_case() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (rid, _, owner_id) = requested(&c, "owner@self.example").await;
    let v = version(&c, rid).await;
    let key = Uuid::new_v4().to_string();
    let owner = owner_id.to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "approve"),
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
                ("x-cf-operator-user-id", &owner),
            ],
            Some(json!({ "expectedVersion": v })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_admin_operations")
            .await,
        0
    );
    // A linked staff member from outside the workspace may decide (LINKED).
    let outsider = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "reject"),
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
                ("x-cf-operator-user-id", &outsider),
            ],
            Some(json!({ "expectedVersion": v, "reasonCode": "NOT_ELIGIBLE" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_admin_operations WHERE staff_link = 'LINKED'")
            .await,
        1
    );
}

#[tokio::test]
async fn a_withdrawn_request_is_read_as_cancelled_and_never_decided() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (rid, owner, _) = requested(&c, "owner@withdraw.example").await;
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/production-access-requests/{rid}/cancel"),
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.json);
    let list = c
        .bridge("/internal/v1/operator/verification/cases", &[OPERATOR])
        .await;
    assert!(
        !list.json.to_string().contains(&rid.to_string()),
        "not queue work"
    );
    let read = c.bridge(&case(rid), &[OPERATOR]).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.json);
    assert_eq!(read.json["state"], "CANCELLED");
    let v = read.json["version"].as_i64().unwrap();
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &decide(rid, "review"),
            &[OPERATOR, ("idempotency-key", &key)],
            Some(json!({ "expectedVersion": v })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "a definite refusal: {}",
        r.json
    );
    // Unknown and malformed case ids.
    for bad in [
        format!(
            "/internal/v1/operator/verification/cases/cfmeet:PARTNER_GO_LIVE:{}",
            Uuid::new_v4()
        ),
        "/internal/v1/operator/verification/cases/cfmeet:APP_PUBLISH:x".to_string(),
    ] {
        assert_eq!(
            c.bridge(&bad, &[OPERATOR]).await.status,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }
    for bad in [
        "/internal/v1/operator/verification/cases?limit=0",
        "/internal/v1/operator/verification/cases?cursor=nope",
        "/internal/v1/operator/verification/cases?state=OPEN",
    ] {
        assert_eq!(
            c.bridge(bad, &[OPERATOR]).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn the_queue_pages_newest_first() {
    let Some(c) = Control::new().await else {
        return;
    };
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(requested(&c, &format!("owner{n}@page.example")).await.0);
    }
    let first = c
        .bridge(
            "/internal/v1/operator/verification/cases?limit=2",
            &[OPERATOR],
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json);
    assert_eq!(first.json["items"].as_array().unwrap().len(), 2);
    let cursor = first.json["next_cursor"].as_str().unwrap().to_string();
    let second = c
        .bridge(
            &format!("/internal/v1/operator/verification/cases?limit=2&cursor={cursor}"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.json);
    assert_eq!(second.json["items"].as_array().unwrap().len(), 1);
    assert!(second.json["next_cursor"].is_null());
    let seen: Vec<String> = first.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second.json["items"].as_array().unwrap())
        .map(|i| i["case_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        seen[0],
        format!("cfmeet:PARTNER_GO_LIVE:{}", ids[2]),
        "newest first"
    );
    assert_eq!(seen.len(), 3);
}

//! Verification (A3) through the full chain (real mTLS, signed HQ request, staff JWS)
//! against a fake Developer Platform: strict case shapes, the owner route of each command,
//! the forwarded body and approval, and settlement through the command core.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{CommandCall, CommandOutcome, OwnerError, OwnerPort},
    wire,
};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const RID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const APP: &str = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
const PARTNER: &str = "4d1f9a2e-7b3c-4e5d-8f6a-9b0c1d2e3f4a";

fn pgl() -> String {
    format!("cfmeet:PARTNER_GO_LIVE:{RID}")
}

fn case(kind: &str, id: &str, state: &str, version: i64) -> Value {
    json!({
        "case_id": format!("cfmeet:{kind}:{id}"), "product": "cfmeet", "kind": kind,
        "subject_ref": APP, "state": state, "version": version, "assignee_ref": null,
        "submitted_at": "2026-11-20T10:00:00Z", "updated_at": "2026-11-20T11:00:00Z",
        "evidence": [{"kind": "USE_CASE_SUMMARY", "ref": format!("{id}:use-case"), "sha256": "a".repeat(64)}],
        "approvals_required": 2, "partner_tenant_id": PARTNER,
    })
}

fn detail(state: &str, version: i64) -> Value {
    let mut c = case("PARTNER_GO_LIVE", RID, state, version);
    c["details"] =
        json!({"requested_capabilities": ["MEETINGS", "SMS"], "approved_capabilities": []});
    c
}

/// (path, query, operator) of one read.
type Read = (String, Vec<(String, String)>, String);
/// (path, body, operator, approver, idempotency key) of one command.
type Sent = (String, Value, String, Option<String>, String);

#[derive(Default)]
struct FakeDp {
    reads: Mutex<Vec<Read>>,
    commands: Mutex<Vec<Sent>>,
    /// The case answer, or a leaked extra field / bad state to test strictness.
    case: Mutex<Option<Value>>,
}

#[async_trait]
impl OwnerPort for FakeDp {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        assert_eq!(owner, Owner::DeveloperPlatform);
        self.reads.lock().unwrap().push((
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            operator.to_string(),
        ));
        let p = path.trim_start_matches("/internal/v1/operator/verification/cases");
        Ok(match p {
            "" => {
                json!({"items": [case("PARTNER_GO_LIVE", RID, "OPEN", 3), case("APP_REVIEW", APP, "CLAIMED", 9)],
                         "next_cursor": "1795000000123456.A.0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a"})
            }
            x if x.ends_with("/history") => json!({"case_id": pgl(), "items": [
                {"sequence": 1, "from_state": null, "to_state": "OPEN", "actor_kind": "PARTNER", "actor_ref": null,
                 "reason_code": null, "occurred_at": "2026-11-20T10:00:00Z"},
                {"sequence": 2, "from_state": "OPEN", "to_state": "CLAIMED", "actor_kind": "STAFF", "actor_ref": "hq:staff-0001",
                 "reason_code": null, "occurred_at": "2026-11-20T11:00:00Z"}], "next_cursor": null}),
            _ => self
                .case
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| detail("OPEN", 3)),
        })
    }

    async fn command(&self, _owner: Owner, call: CommandCall<'_>) -> CommandOutcome {
        self.commands.lock().unwrap().push((
            call.path.to_string(),
            call.body.clone(),
            call.operator.to_string(),
            call.approval.map(|a| a.approver.clone()),
            call.idempotency_key.to_string(),
        ));
        let version = call.body["expectedVersion"].as_i64().unwrap_or(0);
        let state = if call.path.ends_with("/review") {
            "CLAIMED"
        } else {
            "APPROVED"
        };
        CommandOutcome::Applied(detail(state, version + 1))
    }
}

async fn harness(dp: Arc<FakeDp>) -> Harness {
    start(Options {
        owners: Some(dp),
        switches_all_on: true,
        ..Default::default()
    })
    .await
}

fn reviewer_call(path: &str, query: &str, permission: &str) -> Call {
    call(path, query, permission, &["cfmeet.verification.reviewer"])
}

#[tokio::test]
async fn verification_reads_are_strict_hq_case_shapes() {
    let dp = Arc::new(FakeDp::default());
    let h = harness(dp.clone()).await;
    let q = format!("{}&limit=20", scoped("verification-review"));
    let (status, v) = send(
        &h,
        &reviewer_call("/verification/cases", &q, "verification.cases.read"),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_envelope(&v, true);
    assert_eq!(v["result"]["items"][0]["case_id"], pgl());
    assert_eq!(v["result"]["items"][1]["state"], "CLAIMED");
    assert!(v["result"]["next_cursor"].is_string());
    let reads = dp.reads.lock().unwrap().clone();
    assert_eq!(reads[0].0, "/internal/v1/operator/verification/cases");
    assert_eq!(reads[0].1, vec![("limit".to_string(), "20".to_string())]);
    assert_eq!(reads[0].2, "hq:staff-0001");

    let (status, v) = send(
        &h,
        &reviewer_call(
            &format!("/verification/cases/{}", pgl()),
            &scoped("verification-review"),
            "verification.cases.read",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v["result"]["details"]["requested_capabilities"],
        json!(["MEETINGS", "SMS"])
    );

    let (status, v) = send(
        &h,
        &reviewer_call(
            &format!("/verification/cases/{}/history", pgl()),
            &scoped("verification-review"),
            "verification.case-history.read",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["items"][1]["actor_ref"], "hq:staff-0001");
    assert_eq!(v["result"]["case_id"], pgl());

    // Anything outside the shape fails the answer: an extra key is never relayed, a state
    // outside HQ's set is refused.
    let mut leaky = detail("OPEN", 3);
    leaky["use_case_summary"] = json!("call me on +15551234567");
    *dp.case.lock().unwrap() = Some(leaky);
    let (status, v) = send(
        &h,
        &reviewer_call(
            &format!("/verification/cases/{}", pgl()),
            &scoped("verification-review"),
            "verification.cases.read",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert!(v["result"].get("use_case_summary").is_none());
    *dp.case.lock().unwrap() = Some(detail("LIVE", 3));
    let (status, v) = send(
        &h,
        &reviewer_call(
            &format!("/verification/cases/{}", pgl()),
            &scoped("verification-review"),
            "verification.cases.read",
        ),
    )
    .await;
    assert_eq!((status, code(&v)), (500, "INTERNAL"));

    // Unknown kinds name nothing.
    let (_, v) = send(
        &h,
        &reviewer_call(
            &format!("/verification/cases/cfmeet:APP_PUBLISH:{APP}"),
            &scoped("verification-review"),
            "verification.cases.read",
        ),
    )
    .await;
    assert_eq!(code(&v), "RESOURCE_NOT_FOUND");
}

fn command(path: &str, permission: &str, body: Value) -> Call {
    let mut c = reviewer_call(path, &scoped("verification-review"), permission);
    c.method = "POST";
    c.body = serde_json::to_vec(&body).unwrap();
    c.idempotency_key = body["operation_id"].as_str().map(str::to_string);
    c
}

fn approval(capability: &str) -> Value {
    let now = Utc::now();
    json!({
        "approval_id": Uuid::new_v4().to_string(),
        "capability": capability,
        "requester_ref": "staff-0001",
        "approver_ref": "staff-0002",
        "intent_digest": wire::sha256_hex(b"intent"),
        "decided_at": (now - Duration::minutes(1)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "expires_at": (now + Duration::hours(11)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

#[tokio::test]
async fn verification_commands_reach_the_right_owner_route_once() {
    let dp = Arc::new(FakeDp::default());
    let h = harness(dp.clone()).await;

    // Claim (R2): the production-access review route, no approver.
    let key = Uuid::new_v4().to_string();
    let (status, v) = send(
        &h,
        &command(
            &format!("/verification/cases/{}/claim", pgl()),
            "verification.case.claim",
            json!({"operation_id": key, "expected_version": 3}),
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["operation"]["status"], "SUCCEEDED");
    assert_eq!(v["result"]["operation"]["source_status"], "CLAIMED");
    assert_eq!(v["result"]["operation"]["source_version"], "4");
    assert_eq!(v["result"]["resource"]["state"], "CLAIMED");
    {
        let sent = dp.commands.lock().unwrap();
        assert_eq!(
            sent[0].0,
            format!("/internal/v1/operator/production-access-requests/{RID}/review")
        );
        assert_eq!(sent[0].1, json!({"expectedVersion": 3}));
        assert_eq!(sent[0].2, "hq:staff-0001");
        assert_eq!(sent[0].3, None);
        assert_eq!(sent[0].4, key);
    }

    // Approve (R4): needs the approval; the approved subset is forwarded.
    let approve = "verification.partner-go-live.approve";
    let key = Uuid::new_v4().to_string();
    let mut c = command(
        &format!("/verification/cases/{}/partner-go-live/approve", pgl()),
        approve,
        json!({"operation_id": key, "expected_version": 4, "approved_capabilities": ["MEETINGS"]}),
    );
    assert_eq!(code(&send(&h, &c).await.1), "APPROVAL_REQUIRED");
    c.approval = Some(approval(approve));
    // A new request: fresh nonce and token id.
    c.nonce = hex::encode(rand_bytes());
    c.jti = Uuid::new_v4().to_string();
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    {
        let sent = dp.commands.lock().unwrap();
        let last = sent.last().unwrap();
        assert_eq!(
            last.0,
            format!("/internal/v1/operator/production-access-requests/{RID}/approve")
        );
        assert_eq!(
            last.1,
            json!({"expectedVersion": 4, "capabilities": ["MEETINGS"]})
        );
        assert_eq!(last.3.as_deref(), Some("hq:staff-0002"));
    }

    // A command of the other kind names nothing; a reject needs its reason; the version is
    // an integer.
    let n = dp.commands.lock().unwrap().len();
    let mut wrong_kind = command(
        &format!("/verification/cases/cfmeet:APP_REVIEW:{APP}/partner-go-live/approve"),
        approve,
        json!({"operation_id": Uuid::new_v4(), "expected_version": 9}),
    );
    wrong_kind.approval = Some(approval(approve));
    assert_eq!(code(&send(&h, &wrong_kind).await.1), "RESOURCE_NOT_FOUND");
    let reject = "verification.app-review.reject";
    let mut no_reason = command(
        &format!("/verification/cases/cfmeet:APP_REVIEW:{APP}/app-review/reject"),
        reject,
        json!({"operation_id": Uuid::new_v4(), "expected_version": 9}),
    );
    no_reason.approval = Some(approval(reject));
    assert_eq!(code(&send(&h, &no_reason).await.1), "COMMAND_BODY_INVALID");
    let claim = "verification.case.claim";
    let string_version = command(
        &format!("/verification/cases/cfmeet:APP_REVIEW:{APP}/claim"),
        claim,
        json!({"operation_id": Uuid::new_v4(), "expected_version": "9"}),
    );
    assert_eq!(
        code(&send(&h, &string_version).await.1),
        "COMMAND_BODY_INVALID"
    );
    assert_eq!(
        dp.commands.lock().unwrap().len(),
        n,
        "nothing reached the owner"
    );

    // App review: the app routes.
    let publish = "verification.app-review.publish";
    let key = Uuid::new_v4().to_string();
    let mut p = command(
        &format!("/verification/cases/cfmeet:APP_REVIEW:{APP}/app-review/publish"),
        publish,
        json!({"operation_id": key, "expected_version": 9}),
    );
    p.approval = Some(approval(publish));
    let (status, v) = send(&h, &p).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        dp.commands.lock().unwrap().last().unwrap().0,
        format!("/internal/v1/operator/apps/{APP}/publish")
    );

    // The readback finds the operation under the command's purpose and scope.
    let (status, v) = send(
        &h,
        &reviewer_call(
            &format!("/hq/operations/{key}"),
            &scoped("verification-review"),
            "bridge.operations.read",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["capability_id"], publish);
    assert_eq!(v["result"]["status"], "SUCCEEDED");
}

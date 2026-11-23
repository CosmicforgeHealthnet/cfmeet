//! A3.0 command core over the real HQ chain (mTLS, HMAC v2, staff assertion): send once,
//! idempotent replay, version slots, outcome classification, settlement from the owner's
//! record (never a resend) and the private operations readback.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::contract::catalog::catalog;
use admin_bridge::contract::model::{Capability, Owner};
use admin_bridge::owners::{CommandCall, CommandOutcome, OwnerError, OwnerPort};
use admin_bridge::routes::commands;
use admin_bridge::store::{OpStatus, Store};
use admin_bridge::wire;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const RETRY: &str = "partners.webhook-delivery.retry";
const RETRY_PATH: &str = "/partners/workspaces/ws-1/webhook-deliveries/d-1/retry";
const APPROVE: &str = "verification.partner-go-live.approve";
const APPROVE_PATH: &str = "/verification/cases/case-1/partner-go-live/approve";

#[derive(Debug, Clone)]
struct Sent {
    path: String,
    body: Value,
    operator: String,
    idempotency_key: String,
    approver: Option<String>,
    approval_ref: Option<String>,
}

#[derive(Default)]
struct FakeOwner {
    outcome: Mutex<Option<CommandOutcome>>,
    record: Mutex<Option<Value>>,
    sent: Mutex<Vec<Sent>>,
    record_lookups: Mutex<u32>,
}

impl FakeOwner {
    fn answering(outcome: CommandOutcome) -> Arc<Self> {
        let f = FakeOwner::default();
        *f.outcome.lock().unwrap() = Some(outcome);
        Arc::new(f)
    }
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }
    fn set_record(&self, v: Option<Value>) {
        *self.record.lock().unwrap() = v;
    }
}

#[async_trait]
impl OwnerPort for FakeOwner {
    async fn read(
        &self,
        _: Owner,
        _: &str,
        _: &[(&str, String)],
        _: &str,
    ) -> Result<Value, OwnerError> {
        Err(OwnerError::Unavailable)
    }

    async fn command(&self, _owner: Owner, call: CommandCall<'_>) -> CommandOutcome {
        self.sent.lock().unwrap().push(Sent {
            path: call.path.to_string(),
            body: call.body.clone(),
            operator: call.operator.to_string(),
            idempotency_key: call.idempotency_key.to_string(),
            approver: call.approval.map(|a| a.approver.clone()),
            approval_ref: call.approval.map(|a| a.approval_ref.clone()),
        });
        self.outcome
            .lock()
            .unwrap()
            .clone()
            .expect("scripted outcome")
    }

    async fn operation_record(
        &self,
        _: Owner,
        _: &str,
        _: &str,
    ) -> Result<Option<Value>, OwnerError> {
        *self.record_lookups.lock().unwrap() += 1;
        Ok(self.record.lock().unwrap().clone())
    }
}

fn served() -> Vec<Capability> {
    catalog()
        .into_iter()
        .map(|mut c| {
            if c.id == RETRY || c.id == APPROVE {
                c.served = true;
            }
            c
        })
        .collect()
}

async fn harness(owner: Arc<FakeOwner>) -> Harness {
    start(Options {
        owners: Some(owner),
        catalog: Some(served()),
        switches_all_on: true,
        test_commands: vec![
            (
                RETRY,
                Owner::DeveloperPlatform,
                "/internal/v1/operator/webhook-deliveries/{delivery_id}/retry",
            ),
            (
                APPROVE,
                Owner::DeveloperPlatform,
                "/internal/v1/operator/production-access/{case_id}/approve",
            ),
        ],
        ..Default::default()
    })
    .await
}

fn retry(key: Uuid, version: &str) -> Call {
    let mut c = call(
        RETRY_PATH,
        &scoped("ops-incident"),
        RETRY,
        &["cfmeet.partner_ops"],
    );
    c.method = "POST";
    c.body =
        serde_json::to_vec(&json!({"operation_id": key, "expected_version": version})).unwrap();
    c.idempotency_key = Some(key.to_string());
    c
}

fn readback(key: Uuid, purpose: &str) -> Call {
    call(
        &format!("/hq/operations/{key}"),
        &scoped(purpose),
        "bridge.operations.read",
        &["cfmeet.partner_ops"],
    )
}

fn applied(status: &str, version: &str) -> CommandOutcome {
    CommandOutcome::Applied(json!({"status": status, "version": version}))
}

#[tokio::test]
async fn a_command_is_sent_once_and_a_replay_returns_the_stored_outcome() {
    let owner = FakeOwner::answering(applied("RETRY_QUEUED", "4"));
    let h = harness(owner.clone()).await;
    let key = Uuid::new_v4();

    let (status, v) = send(&h, &retry(key, "3")).await;
    assert_eq!(status, 200, "{v}");
    assert_envelope(&v, true);
    let op = &v["result"]["operation"];
    assert_eq!(op["status"], "SUCCEEDED");
    assert_eq!(op["idempotency_key"], key.to_string());
    assert_eq!(op["source_status"], "RETRY_QUEUED");
    assert_eq!(op["source_version"], "4");
    assert_eq!(v["result"]["resource"]["status"], "RETRY_QUEUED");

    let sent = owner.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].path,
        "/internal/v1/operator/webhook-deliveries/d-1/retry"
    );
    assert_eq!(sent[0].operator, "hq:staff-0001");
    assert_eq!(sent[0].idempotency_key, key.to_string());
    assert_eq!(sent[0].approver, None, "R2: no approval headers");
    assert_eq!(sent[0].body["expected_version"], "3");

    // HQ replays the same command (fresh nonce and token, same body and key).
    let (status, v) = send(&h, &retry(key, "3")).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["operation"]["status"], "SUCCEEDED");
    assert_eq!(owner.sent().len(), 1, "never sent twice");

    // The outcome is audited after the accepted request.
    let decisions: Vec<String> = h
        .store
        .audit_entries()
        .iter()
        .map(|e| e.0.decision.clone())
        .collect();
    assert!(
        decisions.windows(2).any(|w| w == ["ALLOWED", "SUCCEEDED"]),
        "{decisions:?}"
    );

    let (status, v) = send(&h, &readback(key, "ops-incident")).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["status"], "SUCCEEDED");
    assert_eq!(v["result"]["capability_id"], RETRY);
    assert!(v["result"]["settled_at"].is_string());
}

#[tokio::test]
async fn the_key_is_bound_to_one_intent_and_one_target_version() {
    let owner = FakeOwner::answering(applied("RETRY_QUEUED", "4"));
    let h = harness(owner.clone()).await;
    let key = Uuid::new_v4();
    assert_eq!(send(&h, &retry(key, "3")).await.0, 200);

    // Same key, another body.
    assert_eq!(
        code(&send(&h, &retry(key, "9")).await.1),
        "IDEMPOTENCY_CONFLICT"
    );
    // Another key, the version the first one already used.
    assert_eq!(
        code(&send(&h, &retry(Uuid::new_v4(), "3")).await.1),
        "VERSION_CONFLICT"
    );
    assert_eq!(owner.sent().len(), 1, "nothing else reached the owner");

    // operation_id must be the Idempotency-Key; unknown fields are refused. (Another staff
    // member: the harness allows 5 requests per minute and permission.)
    let retry = |key: Uuid, version: &str| {
        let mut c = retry(key, version);
        c.sub = "staff-0003".into();
        c
    };
    let mut mismatched = retry(Uuid::new_v4(), "5");
    mismatched.body =
        serde_json::to_vec(&json!({"operation_id": Uuid::new_v4(), "expected_version": "5"}))
            .unwrap();
    assert_eq!(code(&send(&h, &mismatched).await.1), "COMMAND_BODY_INVALID");
    let key2 = Uuid::new_v4();
    let mut extra = retry(key2, "5");
    extra.body =
        serde_json::to_vec(&json!({"operation_id": key2, "expected_version": "5", "force": true}))
            .unwrap();
    assert_eq!(code(&send(&h, &extra).await.1), "COMMAND_BODY_INVALID");
    let mut no_version = retry(key2, "5");
    no_version.body = serde_json::to_vec(&json!({"operation_id": key2})).unwrap();
    assert_eq!(code(&send(&h, &no_version).await.1), "COMMAND_BODY_INVALID");
    let mut negative = retry(key2, "5");
    negative.body =
        serde_json::to_vec(&json!({"operation_id": key2, "expected_version": -1})).unwrap();
    assert_eq!(code(&send(&h, &negative).await.1), "COMMAND_BODY_INVALID");
    assert_eq!(owner.sent().len(), 1);
}

#[tokio::test]
async fn an_integer_expected_version_is_the_same_slot_as_its_string() {
    let owner = FakeOwner::answering(applied("RETRY_QUEUED", "8"));
    let h = harness(owner.clone()).await;
    let key = Uuid::new_v4();
    let mut c = retry(key, "7");
    c.body = serde_json::to_vec(&json!({"operation_id": key, "expected_version": 7})).unwrap();
    assert_eq!(send(&h, &c).await.0, 200);
    assert_eq!(
        owner.sent()[0].body["expected_version"],
        7,
        "forwarded as sent"
    );
    let op = h
        .store
        .operations()
        .into_iter()
        .find(|o| o.idempotency_key == key)
        .unwrap();
    assert_eq!(op.expected_version, "7");
    assert_eq!(
        code(&send(&h, &retry(Uuid::new_v4(), "7")).await.1),
        "VERSION_CONFLICT"
    );
}

#[tokio::test]
async fn owner_refusals_fail_the_operation_and_free_the_version() {
    let owner = FakeOwner::answering(CommandOutcome::Refused {
        status: 409,
        code: Some("VERSION_MISMATCH".into()),
    });
    let h = harness(owner.clone()).await;
    let key = Uuid::new_v4();
    let (status, v) = send(&h, &retry(key, "3")).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"));
    let op = h
        .store
        .operations()
        .into_iter()
        .find(|o| o.idempotency_key == key)
        .unwrap();
    assert_eq!(op.status, OpStatus::Failed);
    assert_eq!(op.source_status.as_deref(), Some("VERSION_MISMATCH"));

    *owner.outcome.lock().unwrap() = Some(CommandOutcome::Refused {
        status: 422,
        code: None,
    });
    let (status, v) = send(&h, &retry(Uuid::new_v4(), "3")).await;
    assert_eq!(
        (status, code(&v)),
        (422, "OWNER_REFUSED"),
        "a FAILED operation frees its version slot"
    );

    *owner.outcome.lock().unwrap() = Some(CommandOutcome::Refused {
        status: 404,
        code: None,
    });
    assert_eq!(
        code(&send(&h, &retry(Uuid::new_v4(), "3")).await.1),
        "RESOURCE_NOT_FOUND"
    );

    // Not configured: never sent, nothing done.
    *owner.outcome.lock().unwrap() = Some(CommandOutcome::NotSent { configured: false });
    let key = Uuid::new_v4();
    assert_eq!(
        code(&send(&h, &retry(key, "3")).await.1),
        "CAPABILITY_DISABLED"
    );
    let op = h
        .store
        .operations()
        .into_iter()
        .find(|o| o.idempotency_key == key)
        .unwrap();
    assert_eq!(
        (op.status, op.source_status.as_deref()),
        (OpStatus::Failed, Some("NOT_SENT"))
    );
}

#[tokio::test]
async fn an_unknown_outcome_is_settled_from_the_owner_record_never_resent() {
    let owner = FakeOwner::answering(CommandOutcome::Unknown);
    let h = harness(owner.clone()).await;
    let key = Uuid::new_v4();
    let (status, v) = send(&h, &retry(key, "3")).await;
    assert_eq!((status, code(&v)), (502, "UNKNOWN_OUTCOME"));

    // The owner has no record yet and little time has passed: still unknown.
    let (_, v) = send(&h, &readback(key, "ops-incident")).await;
    assert_eq!(v["result"]["status"], "UNKNOWN_OUTCOME");

    // A replay does not resend either.
    assert_eq!(code(&send(&h, &retry(key, "3")).await.1), "UNKNOWN_OUTCOME");
    assert_eq!(owner.sent().len(), 1);

    // The owner's record says it was applied.
    owner.set_record(Some(json!({
        "idempotency_key": key, "outcome": "APPLIED", "source_status": "RETRY_QUEUED", "source_version": "4"
    })));
    let (_, v) = send(&h, &readback(key, "ops-incident")).await;
    assert_eq!(v["result"]["status"], "SUCCEEDED");
    assert_eq!(v["result"]["source_version"], "4");
    assert_eq!(
        owner.sent().len(),
        1,
        "settled by reading, never by resending"
    );

    // A record for another key is off-contract and settles nothing.
    let other = Uuid::new_v4();
    assert_eq!(
        code(&send(&h, &retry(other, "7")).await.1),
        "UNKNOWN_OUTCOME"
    );
    owner.set_record(Some(
        json!({"idempotency_key": key, "outcome": "APPLIED", "source_status": "X_Y"}),
    ));
    let (_, v) = send(&h, &readback(other, "ops-incident")).await;
    assert_eq!(v["result"]["status"], "UNKNOWN_OUTCOME");
}

#[tokio::test]
async fn no_owner_record_after_the_answer_window_means_never_received() {
    let owner = FakeOwner::answering(CommandOutcome::Unknown);
    let h = harness(owner.clone()).await;
    let key = Uuid::new_v4();
    assert_eq!(code(&send(&h, &retry(key, "3")).await.1), "UNKNOWN_OUTCOME");
    h.store
        .backdate_operation(key, commands::NOT_RECEIVED_AFTER_SECONDS + 1);
    let (_, v) = send(&h, &readback(key, "ops-incident")).await;
    assert_eq!(v["result"]["status"], "FAILED");
    assert_eq!(v["result"]["source_status"], "NOT_RECEIVED");
    // Settled rows never change, even if a record shows up later.
    owner.set_record(Some(
        json!({"idempotency_key": key, "outcome": "APPLIED", "source_status": "RETRY_QUEUED"}),
    ));
    let (_, v) = send(&h, &readback(key, "ops-incident")).await;
    assert_eq!(v["result"]["status"], "FAILED");
}

#[tokio::test]
async fn the_sweep_fails_never_sent_rows_and_settles_sent_ones() {
    let owner = FakeOwner::answering(CommandOutcome::Unknown);
    let h = harness(owner.clone()).await;
    // A PENDING row whose request died before sending.
    let pending = admin_bridge::store::Operation {
        principal_sub: "staff-0001".into(),
        idempotency_key: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        capability: RETRY.into(),
        owner: "developer-platform".into(),
        purpose: "ops-incident".into(),
        scope: json!({}),
        intent_sha256: wire::sha256_hex(b"i"),
        target: "t:pending".into(),
        expected_version: "1".into(),
        approval_id: None,
        correlation_id: Uuid::new_v4(),
        status: OpStatus::Pending,
        source_status: None,
        source_version: None,
        error_code: None,
        created_at: Utc::now() - Duration::seconds(commands::PENDING_GRACE_SECONDS + 1),
        sent_at: None,
        settled_at: None,
    };
    h.store.begin_operation(&pending).await.unwrap();
    let sent = Uuid::new_v4();
    assert_eq!(
        code(&send(&h, &retry(sent, "3")).await.1),
        "UNKNOWN_OUTCOME"
    );
    h.store
        .backdate_operation(sent, commands::PENDING_GRACE_SECONDS + 1);
    owner.set_record(Some(
        json!({"idempotency_key": sent, "outcome": "REFUSED", "source_status": "DELIVERY_GONE"}),
    ));

    assert_eq!(commands::sweep(&h.bridge).await, 2);
    let ops = h.store.operations();
    let p = ops
        .iter()
        .find(|o| o.idempotency_key == pending.idempotency_key)
        .unwrap();
    assert_eq!(
        (p.status, p.source_status.as_deref()),
        (OpStatus::Failed, Some("NOT_SENT"))
    );
    let s = ops.iter().find(|o| o.idempotency_key == sent).unwrap();
    assert_eq!(
        (s.status, s.source_status.as_deref()),
        (OpStatus::Failed, Some("DELIVERY_GONE"))
    );
    assert_eq!(owner.sent().len(), 1);
}

#[tokio::test]
async fn the_readback_is_private_to_the_staff_member_and_the_operation_scope() {
    let owner = FakeOwner::answering(applied("RETRY_QUEUED", "4"));
    let h = harness(owner.clone()).await;
    let key = Uuid::new_v4();
    assert_eq!(send(&h, &retry(key, "3")).await.0, 200);

    let mut other_staff = readback(key, "ops-incident");
    other_staff.sub = "staff-0002".into();
    assert_eq!(code(&send(&h, &other_staff).await.1), "RESOURCE_NOT_FOUND");

    let mut other_tenant = call(
        &format!("/hq/operations/{key}"),
        &format!("market=GLOBAL&organization=cfmeet&zone=production&tenant={TENANT_ID}&purpose=ops-incident"),
        "bridge.operations.read",
        &["cfmeet.partner_ops"],
    );
    other_tenant.tenant_scopes = vec![TENANT, TENANT_ID];
    assert_eq!(code(&send(&h, &other_tenant).await.1), "SCOPE_DENIED");

    assert_eq!(
        code(&send(&h, &readback(Uuid::new_v4(), "ops-incident")).await.1),
        "RESOURCE_NOT_FOUND"
    );
}

fn approval(approval_id: Uuid) -> Value {
    let now = Utc::now();
    json!({
        "approval_id": approval_id.to_string(),
        "capability": APPROVE,
        "requester_ref": "staff-0001",
        "approver_ref": "staff-0002",
        "intent_digest": wire::sha256_hex(b"intent"),
        "decided_at": (now - Duration::minutes(5)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "expires_at": (now + Duration::hours(11)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

fn approve(key: Uuid, approval_id: Uuid) -> Call {
    let mut c = call(
        APPROVE_PATH,
        &scoped("verification-review"),
        APPROVE,
        &["cfmeet.verification.reviewer"],
    );
    c.method = "POST";
    c.body = serde_json::to_vec(&json!({"operation_id": key, "expected_version": "7"})).unwrap();
    c.idempotency_key = Some(key.to_string());
    c.approval = Some(approval(approval_id));
    c
}

#[tokio::test]
async fn two_person_commands_forward_the_approval_and_replay_without_reusing_it() {
    let owner = FakeOwner::answering(applied("APPROVED", "8"));
    let h = harness(owner.clone()).await;
    let (key, approval_id) = (Uuid::new_v4(), Uuid::new_v4());
    let (status, v) = send(&h, &approve(key, approval_id)).await;
    assert_eq!(status, 200, "{v}");
    let sent = owner.sent();
    assert_eq!(sent[0].approver.as_deref(), Some("hq:staff-0002"));
    assert_eq!(
        sent[0].approval_ref.as_deref(),
        Some(approval_id.to_string().as_str())
    );

    // The replay returns the stored outcome; it does not consume the approval again.
    let (status, v) = send(&h, &approve(key, approval_id)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(owner.sent().len(), 1);
    // The same approval on another command is still single use.
    assert_eq!(
        code(&send(&h, &approve(Uuid::new_v4(), approval_id)).await.1),
        "APPROVAL_REUSED"
    );
    assert_eq!(owner.sent().len(), 1);
}

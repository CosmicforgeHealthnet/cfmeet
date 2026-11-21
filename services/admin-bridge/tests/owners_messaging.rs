//! Messaging reads through the full chain (real mTLS, signed HQ request) against a fake
//! Messaging that records every call: paths, query mapping, the operator attribution,
//! strict translation, and that nothing beyond ids, states, counts and times is relayed
//! even when the owner sends more.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T0: &str = "2026-11-01T10:00:00.123456Z";
const CUR: &str = "1793527200123456.6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const ROOT: &str = "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899";

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakeMessaging {
    calls: Mutex<Vec<Recorded>>,
}

fn scope() -> Value {
    json!({"tenantId": ID, "applicationId": ID, "environmentId": ID})
}

fn with_scope(mut v: Value) -> Value {
    for (k, x) in scope().as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn report_json() -> Value {
    with_scope(json!({
        "id": ID, "conversationId": ID, "securityProfile": "PRIVATE_E2EE",
        "reporterPrincipalId": ID, "reportedPrincipalId": null, "messageIds": [ID],
        "category": "HARASSMENT", "evidenceKind": "REPORTER_DISCLOSURE", "status": "OPEN",
        "resolution": null, "resolvedAt": null, "createdAt": T0, "version": 3,
        // Fields the bridge must drop even if an owner ever sent them.
        "sealedEvidence": "c2VhbGVkLWNpcGhlcnRleHQ=", "plaintext": "never-relay-this-text"
    }))
}

#[async_trait]
impl OwnerPort for FakeMessaging {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        self.calls.lock().unwrap().push((
            owner,
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            operator.to_string(),
        ));
        let p = path.trim_start_matches("/internal/v1/operator");
        Ok(match p {
            "/reports" => json!({"items": [report_json()], "nextCursor": CUR}),
            x if x == format!("/reports/{ID}") => {
                let mut r = report_json();
                r["actions"] = json!([{
                    "id": ID, "kind": "REMOVE_MESSAGE", "reason": "POLICY_VIOLATION", "conversationId": ID,
                    "messageId": ID, "principalId": null, "effectiveUntil": null, "createdAt": T0
                }]);
                r
            }
            "/holds" => json!({"items": [with_scope(json!({
                "id": ID, "scopeKind": "CONVERSATION", "conversationId": ID, "principalId": null,
                "reasonCategory": "LITIGATION", "status": "ACTIVE", "createdAt": T0, "expiresAt": null,
                "releasedAt": null, "releaseReason": null, "caseReference": "CASE-SECRET-42", "version": 2
            }))], "nextCursor": null}),
            "/exports" => json!({"items": [with_scope(json!({
                "id": ID, "conversationId": ID, "status": "READY", "size": 2048, "messageCount": 12,
                "failure": null, "attempts": 1, "createdAt": T0, "completedAt": T0, "expiresAt": T0,
                "objectKey": "exp/aaaaaaaa/bbbbbbbb"
            }))], "nextCursor": null}),
            "/deletion-requests" => json!({"items": [with_scope(json!({
                "id": ID, "kind": "PRINCIPAL_ERASURE", "principalId": ID, "conversationId": null,
                "status": "PARTIALLY_HELD", "messagesPurged": 10, "attachmentsDeleted": 2, "heldItems": 3,
                "createdAt": T0, "completedAt": T0
            }))], "nextCursor": null}),
            "/audit" => json!({"items": [with_scope(json!({
                "id": ID, "occurredAt": T0, "credentialId": null, "principalId": ID, "action": "REPORT_CREATED",
                "targetKind": "REPORT", "targetId": ID, "correlationId": "corr_12345678",
                "detail": {"note": "detail-never-relayed"}
            }))], "nextCursor": CUR}),
            "/switches" => json!({"items": [{
                "switchName": "MESSAGE_SEND", "scopeKey": "global", "engaged": false, "reason": "INITIAL",
                "changedBy": "system", "changedAt": T0, "version": 1
            }], "nextCursor": null}),
            "/incidents" => json!({"items": [{
                "id": ID, "category": "OUTBOX_DEAD_LETTER", "severity": "HIGH", "status": "OPEN",
                "occurrences": 3, "firstSeenAt": T0, "lastSeenAt": T0, "resolvedAt": null, "resolvedBy": null,
                "summary": "summary-never-relayed"
            }], "nextCursor": null}),
            "/failed-messages" => json!({"items": [{
                "id": ID, "sequence": 9, "eventType": "messaging.message.created", "aggregateType": "MESSAGE",
                "aggregateId": ID, "tenantId": ID, "environmentId": ID, "attempts": 8, "occurredAt": T0,
                "payload": {"ciphertext": "payload-never-relayed"}, "lastError": "error-never-relayed"
            }], "nextCursor": null}),
            "/key-transparency" => json!({
                "head": {"treeSize": 42, "rootHash": ROOT, "updatedAt": T0},
                "witnesses": [{"witnessId": "witness-1.example", "treeSize": 42, "rootHash": ROOT, "updatedAt": T0, "lastCosignedSize": 40}]
            }),
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakeMessaging>) {
    let fake = Arc::new(FakeMessaging::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn last_call(fake: &FakeMessaging) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

fn ts() -> &'static [&'static str] {
    &["cfmeet.trust_safety"]
}

#[tokio::test]
async fn messaging_reports_relay_no_evidence() {
    let (h, fake) = harness().await;
    let c = call(
        "/messaging/reports",
        &format!("{}&limit=5&cursor={CUR}", scoped("abuse-review")),
        "messaging.reports.read",
        ts(),
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (owner, path, query, operator) = last_call(&fake);
    assert_eq!(
        (owner, path.as_str(), operator.as_str()),
        (
            Owner::Messaging,
            "/internal/v1/operator/reports",
            "hq:staff-0001"
        )
    );
    assert_eq!(
        query,
        vec![
            ("limit".to_string(), "5".to_string()),
            ("cursor".to_string(), CUR.to_string())
        ]
    );
    let item = &v["result"]["items"][0];
    assert_eq!(item["report_id"], ID);
    assert_eq!(item["evidence_kind"], "REPORTER_DISCLOSURE");
    assert_eq!(item["message_ids"][0], ID);
    assert_eq!(item["created_at"], "2026-11-01T10:00:00Z");
    // The row version the moderation commands act on.
    assert_eq!(item["version"], 3);
    assert_eq!(v["result"]["next_cursor"], CUR);
    let text = v.to_string();
    assert!(
        !text.contains("never-relay-this-text") && !text.contains("c2VhbGVk"),
        "evidence and plaintext never reach HQ"
    );
    // a malformed cursor is the caller's error and never reaches the owner
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        "/messaging/reports",
        &format!("{}&cursor=abc", scoped("abuse-review")),
        "messaging.reports.read",
        ts(),
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn messaging_report_detail_carries_actions_not_evidence() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/messaging/reports/{ID}"),
        &scoped("abuse-review"),
        "messaging.reports.read",
        ts(),
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last_call(&fake).1,
        format!("/internal/v1/operator/reports/{ID}")
    );
    assert_eq!(v["result"]["actions"][0]["kind"], "REMOVE_MESSAGE");
    assert!(!v.to_string().contains("never-relay-this-text"));
    // a non-UUID report id cannot exist: refused without an owner call
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        "/messaging/reports/not-a-uuid",
        &scoped("abuse-review"),
        "messaging.reports.read",
        ts(),
    );
    assert_eq!(code(&send(&h, &bad).await.1), "RESOURCE_NOT_FOUND");
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn messaging_lists_translate_strictly() {
    let (h, fake) = harness().await;
    for (path, perm, owner_path, key, value) in [
        (
            "/messaging/holds",
            "messaging.holds.read",
            "/internal/v1/operator/holds",
            "reason_category",
            "LITIGATION",
        ),
        (
            "/messaging/holds",
            "messaging.holds.read",
            "/internal/v1/operator/holds",
            "version",
            "2",
        ),
        (
            "/messaging/exports",
            "messaging.exports.read",
            "/internal/v1/operator/exports",
            "size_bytes",
            "2048",
        ),
        (
            "/messaging/deletion-requests",
            "messaging.deletion-requests.read",
            "/internal/v1/operator/deletion-requests",
            "held_items",
            "3",
        ),
    ] {
        let c = call(
            path,
            &scoped("privacy-request"),
            perm,
            &["cfmeet.trust_safety"],
        );
        let (status, v) = send(&h, &c).await;
        assert_eq!(status, 200, "{path}: {v}");
        assert_eq!(last_call(&fake).1, owner_path);
        assert_eq!(
            v["result"]["items"][0][key].to_string().trim_matches('"'),
            value,
            "{path}"
        );
        let text = v.to_string();
        assert!(
            !text.contains("CASE-SECRET") && !text.contains("exp/aaaa"),
            "{path}: case references and object keys never reach HQ"
        );
    }
}

#[tokio::test]
async fn messaging_windowed_lists_map_since_and_until() {
    let (h, fake) = harness().await;
    for (path, perm, owner_path) in [
        (
            "/messaging/audit",
            "messaging.audit.read",
            "/internal/v1/operator/audit",
        ),
        (
            "/messaging/incidents",
            "messaging.incidents.read",
            "/internal/v1/operator/incidents",
        ),
        (
            "/messaging/failed-messages",
            "messaging.failed-messages.read",
            "/internal/v1/operator/failed-messages",
        ),
    ] {
        let c = call(
            path,
            &format!("{}{}", scoped("ops-incident"), window(3)),
            perm,
            &["cfmeet.platform_ops"],
        );
        let (status, v) = send(&h, &c).await;
        assert_eq!(status, 200, "{path}: {v}");
        let (_, p, query, _) = last_call(&fake);
        assert_eq!(p, owner_path);
        let keys: Vec<&str> = query.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["since", "until", "limit"], "{path}");
        let text = v.to_string();
        for leak in [
            "detail-never-relayed",
            "summary-never-relayed",
            "payload-never-relayed",
            "error-never-relayed",
        ] {
            assert!(!text.contains(leak), "{path}: {leak}");
        }
    }
}

#[tokio::test]
async fn messaging_switches_are_served() {
    let (h, fake) = harness().await;
    let c = call(
        "/messaging/switches",
        &scoped("ops-incident"),
        "messaging.switches.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last_call(&fake).1, "/internal/v1/operator/switches");
    assert_eq!(v["result"]["items"][0]["switch_name"], "MESSAGE_SEND");
    assert!(v["result"]["next_cursor"].is_null());
}

#[tokio::test]
async fn messaging_key_transparency_is_hashes_and_sizes_only() {
    let (h, fake) = harness().await;
    let c = call(
        "/messaging/key-transparency",
        &scoped("ops-incident"),
        "messaging.key-transparency.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last_call(&fake).1, "/internal/v1/operator/key-transparency");
    let r = &v["result"];
    assert_eq!(r["head"]["tree_size"], 42);
    assert_eq!(r["head"]["root_hash"], ROOT);
    assert_eq!(r["witnesses"][0]["last_cosigned_size"], 40);
}

#[tokio::test]
async fn messaging_policy_stays_blocked() {
    let (h, fake) = harness().await;
    let c = call(
        "/messaging/policy",
        &scoped("ops-incident"),
        "messaging.policy.read",
        &["cfmeet.platform_ops"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "CAPABILITY_DISABLED");
    assert!(fake.calls.lock().unwrap().is_empty());
}

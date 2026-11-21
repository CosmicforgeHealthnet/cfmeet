//! Cross-service operations through the full chain against fake owners: the incident
//! list across Messaging, Calling and the Developer Platform (window, per-owner cursors,
//! PARTIAL when an owner is down), incident resolve and switch change routed to the owning
//! service through the command core with the owner's real target, and the switch list.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::contract::model::Owner;
use admin_bridge::owners::{CommandCall, CommandOutcome, OwnerError, OwnerPort};
use admin_bridge::wire;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T1: &str = "2026-11-03T10:00:00Z";
const T2: &str = "2026-11-05T10:00:00Z";
const M_CURSOR: &str = "1793527200123456.6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

type Read = (Owner, String, Vec<(String, String)>);

#[derive(Default)]
struct FakeOwners {
    reads: Mutex<Vec<Read>>,
    sent: Mutex<Vec<(Owner, String, Value)>>,
    down: Mutex<Vec<Owner>>,
}

#[async_trait]
impl OwnerPort for FakeOwners {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        _: &str,
    ) -> Result<Value, OwnerError> {
        self.reads.lock().unwrap().push((
            owner,
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        ));
        if self.down.lock().unwrap().contains(&owner) {
            return Err(OwnerError::Unavailable);
        }
        let p = path.trim_start_matches("/internal/v1/");
        Ok(match (owner, p) {
            (Owner::Messaging, "operator/incidents") => json!({"items": [{
                "id": ID, "category": "OUTBOX_DEAD_LETTER", "severity": "HIGH", "status": "OPEN",
                "occurrences": 3, "firstSeenAt": T1, "lastSeenAt": T2, "resolvedAt": null,
                "resolvedBy": null, "detail": "secret-meta"}], "nextCursor": M_CURSOR}),
            (Owner::Calling, "operator/incidents") => json!({"items": [{
                "incidentId": ID, "scope": "TENANT", "category": "MEDIA_FAILURE", "severity": "LOW",
                "status": "RESOLVED", "callId": null, "resourceType": null, "resourceId": null,
                "occurrences": 1, "firstSeenAt": T2, "lastSeenAt": T2, "resolvedAt": T2}], "nextCursor": null}),
            (Owner::DeveloperPlatform, "operator/incidents") => json!({"items": [{
                "incidentRecordId": ID, "organizationId": ID, "incidentRef": "INC-1",
                "occurredAt": T1, "recordedAt": T1}], "nextCursor": null}),
            (Owner::Messaging, "operator/switches") => json!({"items": [
                {"switchName": "MESSAGE_SEND", "scopeKey": "GLOBAL", "engaged": false, "reason": "NONE",
                 "changedBy": "system", "changedAt": T1, "version": 2},
                {"switchName": "SEARCH", "scopeKey": ID, "engaged": true, "reason": "INCIDENT",
                 "changedBy": "hq:staff-0002", "changedAt": T2, "version": 1}], "nextCursor": null}),
            (Owner::Notifications, "admin/routing") => json!({"routing": [
                {"category": "SECURITY_ALERT", "channel": "EMAIL", "mode": "ACTIVE", "version": 3,
                 "canaryPercent": 100, "fenceAt": T1}], "audit": [], "routingProtocol": [1]}),
            _ => return Err(OwnerError::NotFound),
        })
    }

    async fn command(&self, owner: Owner, call: CommandCall<'_>) -> CommandOutcome {
        self.sent
            .lock()
            .unwrap()
            .push((owner, call.path.to_string(), call.body.clone()));
        let answer = match owner {
            Owner::Messaging if call.path.ends_with("/resolve") => {
                json!({"status": "RESOLVED", "version": null,
                "resource": {"incidentId": ID, "category": "OUTBOX_DEAD_LETTER", "incidentStatus": "RESOLVED", "resolvedAt": T2}})
            }
            Owner::Calling => json!({"status": "RESOLVED", "version": null,
                "resource": {"incidentId": ID, "applicationId": null, "environmentId": null, "category": "MEDIA_FAILURE",
                             "severity": "LOW", "status": "RESOLVED", "resolvedAt": T2}}),
            Owner::Messaging => json!({"status": "ENGAGED", "version": "3",
                "resource": {"switchName": "SEARCH", "scopeKey": ID, "engaged": true, "reason": "INCIDENT", "changedAt": T2}}),
            _ => json!({"status": "SHADOW", "version": "4",
                "resource": {"category": "SECURITY_ALERT", "channel": "EMAIL", "mode": "SHADOW", "version": 4,
                             "canaryPercent": 0, "fenceAt": T2, "changedAt": T2}}),
        };
        CommandOutcome::Applied(answer)
    }
}

async fn harness() -> (Harness, Arc<FakeOwners>) {
    let fake = Arc::new(FakeOwners::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn tenant_query(purpose: &str, extra: &str) -> String {
    format!("market=GLOBAL&organization=cfmeet&zone=production&tenant={TENANT_ID}&purpose={purpose}{extra}")
}

fn get(path: &str, extra: &str, perm: &str, sub: &str) -> Call {
    let mut c = call(
        path,
        &tenant_query("ops-incident", extra),
        perm,
        &["cfmeet.platform_ops"],
    );
    c.tenant_scopes = vec![TENANT_ID];
    c.sub = sub.into();
    c
}

fn post(path: &str, perm: &str, body: Value, sub: &str, two_person: bool) -> Call {
    let mut c = get(path, "", perm, sub);
    c.method = "POST";
    let key = body["operation_id"].as_str().unwrap().to_string();
    c.body = serde_json::to_vec(&body).unwrap();
    c.idempotency_key = Some(key);
    if two_person {
        let now = Utc::now();
        c.approval = Some(json!({
            "approval_id": Uuid::new_v4().to_string(), "capability": perm, "requester_ref": sub,
            "approver_ref": "staff-9999", "intent_digest": wire::sha256_hex(b"intent"),
            "decided_at": (now - Duration::minutes(1)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "expires_at": (now + Duration::hours(11)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        }));
    }
    c
}

const WINDOW: &str = "&limit=10&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z";

#[tokio::test]
async fn operations_incidents_aggregate_the_owners() {
    let (h, fake) = harness().await;
    let (status, v) = send(
        &h,
        &get(
            "/operations/incidents",
            WINDOW,
            "operations.incidents.read",
            "staff-0101",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["freshness"], "LIVE");
    let items = v["result"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    // Newest first; ids name their owner.
    assert_eq!(items[0]["incident_id"], format!("calling:{ID}"));
    assert_eq!(items[0]["resolvable"], false);
    let partner = items
        .iter()
        .find(|i| i["owner"] == "developer-platform")
        .unwrap();
    assert_eq!(
        (partner["status"].clone(), partner["resolvable"].clone()),
        (json!("RECORDED"), json!(false))
    );
    assert!(!v.to_string().contains("secret-meta"));
    assert_eq!(v["result"]["next_cursor"], format!("m.{M_CURSOR}"));
    let reads = fake.reads.lock().unwrap().clone();
    let calling = reads.iter().find(|r| r.0 == Owner::Calling).unwrap();
    assert_eq!(
        calling.2[0],
        ("tenantId".to_string(), TENANT_ID.to_string())
    );
    assert!(calling
        .2
        .contains(&("since".into(), "2026-11-01T00:00:00Z".into())));

    // The next page asks only Messaging, from its own cursor.
    fake.reads.lock().unwrap().clear();
    let (status, _) = send(
        &h,
        &get(
            "/operations/incidents",
            &format!(
                "&limit=10&cursor=m.{M_CURSOR}&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z"
            ),
            "operations.incidents.read",
            "staff-0102",
        ),
    )
    .await;
    assert_eq!(status, 200);
    let reads = fake.reads.lock().unwrap().clone();
    assert_eq!(reads.len(), 1);
    assert!(reads[0].2.contains(&("cursor".into(), M_CURSOR.into())));

    // An owner down: PARTIAL, never an error or invented data.
    fake.down.lock().unwrap().push(Owner::Calling);
    let (status, v) = send(
        &h,
        &get(
            "/operations/incidents",
            WINDOW,
            "operations.incidents.read",
            "staff-0103",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["freshness"], "PARTIAL");
    assert_eq!(v["result"]["items"].as_array().unwrap().len(), 2);
    // A malformed cursor never reaches an owner.
    let (_, v) = send(
        &h,
        &get(
            "/operations/incidents",
            "&limit=10&cursor=x.abc&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
            "operations.incidents.read",
            "staff-0104",
        ),
    )
    .await;
    assert_eq!(code(&v), "QUERY_INVALID");
}

#[tokio::test]
async fn operations_incident_resolve_routes_to_the_owner() {
    let (h, fake) = harness().await;
    let key = Uuid::new_v4().to_string();
    let body = json!({"operation_id": key, "reason_code": "MITIGATED"});
    let (status, v) = send(
        &h,
        &post(
            &format!("/operations/incidents/messaging:{ID}/resolve"),
            "operations.incident.resolve",
            body,
            "staff-0201",
            false,
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"]["resource"];
    assert_eq!(
        (r["incident_id"].clone(), r["incident_status"].clone()),
        (json!(format!("messaging:{ID}")), json!("RESOLVED"))
    );
    let sent = fake.sent.lock().unwrap().last().cloned().unwrap();
    assert_eq!(sent.0, Owner::Messaging);
    assert_eq!(
        sent.1,
        format!("/internal/v1/operator/incidents/{ID}/resolve")
    );
    assert_eq!(sent.2, json!({"reasonCode": "MITIGATED"}));
    let op = h
        .store
        .operations()
        .into_iter()
        .find(|o| o.idempotency_key.to_string() == key)
        .unwrap();
    assert!(
        op.target.starts_with(&format!("messaging:incident:{ID}")),
        "the real target: {}",
        op.target
    );

    // Calling: the tenant from the scope; the reason stays in the bridge's record.
    let body = json!({"operation_id": Uuid::new_v4().to_string(), "reason_code": "MITIGATED"});
    let (status, v) = send(
        &h,
        &post(
            &format!("/operations/incidents/calling:{ID}/resolve"),
            "operations.incident.resolve",
            body,
            "staff-0202",
            false,
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        fake.sent.lock().unwrap().last().unwrap().2,
        json!({"tenantId": TENANT_ID})
    );

    // A partner incident record has nothing to resolve; an unknown owner names nothing.
    let before = fake.sent.lock().unwrap().len();
    for (n, id) in [
        format!("developer-platform:{ID}"),
        format!("meeting-service:{ID}"),
        "messaging:nope".into(),
    ]
    .into_iter()
    .enumerate()
    {
        let body = json!({"operation_id": Uuid::new_v4().to_string(), "reason_code": "MITIGATED"});
        let (_, v) = send(
            &h,
            &post(
                &format!("/operations/incidents/{id}/resolve"),
                "operations.incident.resolve",
                body,
                &format!("staff-021{n}"),
                false,
            ),
        )
        .await;
        assert_eq!(code(&v), "RESOURCE_NOT_FOUND", "{id}");
    }
    assert_eq!(fake.sent.lock().unwrap().len(), before);
}

#[tokio::test]
async fn operations_switches_join_kill_switches_and_routing_modes() {
    let (h, fake) = harness().await;
    let (status, v) = send(
        &h,
        &get(
            "/operations/switches",
            "",
            "operations.switches.read",
            "staff-0301",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let items = v["result"]["items"].as_array().unwrap();
    let ids: Vec<&str> = items
        .iter()
        .map(|i| i["switch_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "messaging:MESSAGE_SEND".to_string(),
            format!("messaging:SEARCH:{ID}"),
            "notifications:SECURITY_ALERT:EMAIL".into()
        ]
    );
    assert_eq!(items[1]["state"], "ENGAGED");
    assert_eq!(items[2]["state"], "ACTIVE");
    assert_eq!(items[2]["version"], 3);
    fake.down.lock().unwrap().push(Owner::Notifications);
    let (_, v) = send(
        &h,
        &get(
            "/operations/switches",
            "",
            "operations.switches.read",
            "staff-0302",
        ),
    )
    .await;
    assert_eq!(v["freshness"], "PARTIAL");
    let (_, v) = send(
        &h,
        &get(
            "/operations/switches",
            "&cursor=abc",
            "operations.switches.read",
            "staff-0303",
        ),
    )
    .await;
    assert_eq!(code(&v), "QUERY_INVALID");
}

#[tokio::test]
async fn operations_switch_change_routes_to_the_owner() {
    let (h, fake) = harness().await;
    let key = Uuid::new_v4().to_string();
    let body = json!({"operation_id": key, "expected_version": 1, "reason_code": "INCIDENT", "engaged": true});
    let (status, v) = send(
        &h,
        &post(
            &format!("/operations/switches/messaging:SEARCH:{ID}"),
            "operations.switch.change",
            body,
            "staff-0401",
            true,
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"]["resource"];
    assert_eq!(
        (r["state"].clone(), r["version"].clone()),
        (json!("ENGAGED"), json!(3))
    );
    let sent = fake.sent.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        (sent.0, sent.1.as_str()),
        (Owner::Messaging, "/internal/v1/operator/switches/SEARCH")
    );
    assert_eq!(
        sent.2,
        json!({"environmentId": ID, "engaged": true, "expectedVersion": 1, "reasonCode": "INCIDENT"})
    );
    let op = h
        .store
        .operations()
        .into_iter()
        .find(|o| o.idempotency_key.to_string() == key)
        .unwrap();
    assert_eq!(op.target, format!("messaging:switch:SEARCH:{ID}"));

    let body = json!({"operation_id": Uuid::new_v4().to_string(), "expected_version": 3, "reason_code": "CANARY", "mode": "SHADOW"});
    let (status, v) = send(
        &h,
        &post(
            "/operations/switches/notifications:SECURITY_ALERT:EMAIL",
            "operations.switch.change",
            body,
            "staff-0402",
            true,
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let sent = fake.sent.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        sent.1,
        "/internal/v1/operator/routing/SECURITY_ALERT/EMAIL/mode"
    );
    assert_eq!(
        sent.2,
        json!({"mode": "SHADOW", "expectedVersion": 3, "reasonCode": "CANARY"})
    );

    // The field must match the owner; unknown switches name nothing.
    let before = fake.sent.lock().unwrap().len();
    let wrong = json!({"operation_id": Uuid::new_v4().to_string(), "expected_version": 3, "reason_code": "CANARY", "engaged": true});
    let (_, v) = send(
        &h,
        &post(
            "/operations/switches/notifications:SECURITY_ALERT:EMAIL",
            "operations.switch.change",
            wrong,
            "staff-0403",
            true,
        ),
    )
    .await;
    assert_eq!(code(&v), "COMMAND_BODY_INVALID");
    let unknown = json!({"operation_id": Uuid::new_v4().to_string(), "expected_version": 1, "reason_code": "CANARY", "engaged": true});
    let (_, v) = send(
        &h,
        &post(
            "/operations/switches/calling:LIMITS",
            "operations.switch.change",
            unknown,
            "staff-0404",
            true,
        ),
    )
    .await;
    assert_eq!(code(&v), "RESOURCE_NOT_FOUND");
    assert_eq!(fake.sent.lock().unwrap().len(), before);
}

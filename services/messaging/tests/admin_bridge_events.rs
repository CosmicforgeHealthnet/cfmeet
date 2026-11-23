//! Owner events to the HQ admin bridge (ADR 0068 A7, brief §2): with the bridge
//! configured, a report or incident change writes its `abuse.report.changed.v1` /
//! `incident.changed.v1` event in the same transaction (`messaging_admin_bridge_events`);
//! the relay posts each closed body at least once and settles it by the bridge's answer.
//! Without the bridge, nothing is stored. Ids, states, versions and times only.
//!
//! One test function: the on / off switch is process-wide.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use common::{flows::*, *};
use messaging::{
    bridge_events::{self, BridgeEventSink, PublishFailure},
    operations::{self, IncidentCategory},
};
use serde_json::{json, Value};
use uuid::Uuid;

const OP: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVAL: [(&str, &str); 3] = [
    ("x-cf-approver", "hq:staff-0002"),
    ("x-cf-approval-ref", "appr-0001"),
    (
        "x-cf-intent-digest",
        "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899",
    ),
];

/// A sink that records every body and answers with a scripted outcome.
struct Recorder {
    bodies: Mutex<Vec<Value>>,
    outcome: Mutex<Result<(), PublishFailure>>,
}

#[async_trait]
impl BridgeEventSink for Recorder {
    async fn publish(&self, body: &Value) -> Result<(), PublishFailure> {
        self.bodies.lock().unwrap().push(body.clone());
        *self.outcome.lock().unwrap()
    }
}

async fn events(h: &Harness) -> Vec<(String, String, i64, String)> {
    #[derive(sea_orm::FromQueryResult)]
    struct E {
        event_type: String,
        subject_state: String,
        subject_version: i64,
        delivery: String,
    }
    let rows: Vec<E> = messaging::db::all(
        h.conn(),
        "SELECT event_type, subject_state, subject_version, delivery FROM messaging_admin_bridge_events \
         ORDER BY occurred_at, subject_version",
        vec![],
    )
    .await
    .unwrap();
    rows.into_iter()
        .map(|e| (e.event_type, e.subject_state, e.subject_version, e.delivery))
        .collect()
}

#[tokio::test]
async fn report_and_incident_changes_reach_the_bridge_through_the_outbox() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED"], "defaultProfile": "MANAGED" } } }),
    )
    .await;
    let a = h.principal(&c);
    let b = h.principal(&c);
    let conv = h
        .create_conversation(
            &c,
            a,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await
        .json;
    let cid = uuid(&conv["conversationId"]);
    h.post(
        &c,
        Some(a),
        &format!("/v1/conversations/{cid}/members"),
        json!({ "principalId": b }),
    )
    .await;
    let sent = h.send_managed(&c, &conv, a, "reported").await;
    let report = |by: Uuid| {
        let path = format!("/v1/conversations/{cid}/reports");
        let body = json!({ "category": "HARASSMENT", "messageIds": [sent.json["messageId"]] });
        let h = &h;
        let c = &c;
        async move { h.post(c, Some(by), &path, body).await }
    };

    // Off (no CF_ADMIN_BRIDGE_BASE_URL): nothing is stored.
    bridge_events::install(false);
    let r = report(b).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    operations::raise(h.conn(), IncidentCategory::RetentionBacklog, "off", h.now()).await;
    assert!(events(&h).await.is_empty());

    // On: the report's creation and dismissal, an incident's raise (not its bump) and
    // resolution, each with the row version.
    bridge_events::install(true);
    let r = report(a).await;
    let rid = uuid(&r.json["reportId"]);
    let key = Uuid::new_v4().to_string();
    let peers = peers();
    let d = h
        .call_internal(
            &peers.admin_bridge,
            Method::POST,
            &format!("/internal/v1/operator/reports/{rid}/dismiss"),
            Some(json!({ "tenantId": c.context.scope.tenant_id, "expectedVersion": 1, "reasonCode": "NO_VIOLATION" })),
            None,
            &[OP, ("idempotency-key", key.as_str()), APPROVAL[0], APPROVAL[1], APPROVAL[2]],
        )
        .await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.text);
    h.clock.advance(chrono::Duration::seconds(1));
    operations::raise(h.conn(), IncidentCategory::AbuseWave, "env-1", h.now()).await;
    operations::raise(h.conn(), IncidentCategory::AbuseWave, "env-1", h.now()).await;
    #[derive(sea_orm::FromQueryResult)]
    struct Id {
        id: Uuid,
    }
    let incident = messaging::db::one::<Id, _>(
        h.conn(),
        "SELECT id FROM messaging_incidents WHERE category = 'ABUSE_WAVE'",
        vec![],
    )
    .await
    .unwrap()
    .unwrap()
    .id;
    h.clock.advance(chrono::Duration::seconds(1));
    assert!(operations::resolve(h.conn(), incident, "ops", h.now())
        .await
        .unwrap());
    bridge_events::install(false);
    assert_eq!(
        events(&h).await,
        vec![
            (
                "abuse.report.changed.v1".into(),
                "OPEN".into(),
                1,
                "PENDING".into()
            ),
            (
                "abuse.report.changed.v1".into(),
                "DISMISSED".into(),
                2,
                "PENDING".into()
            ),
            (
                "incident.changed.v1".into(),
                "OPEN".into(),
                1,
                "PENDING".into()
            ),
            (
                "incident.changed.v1".into(),
                "RESOLVED".into(),
                2,
                "PENDING".into()
            ),
        ]
    );

    // The relay: a transient failure retries later, then the bridge accepts.
    let sink = Arc::new(Recorder {
        bodies: Mutex::new(vec![]),
        outcome: Mutex::new(Err(PublishFailure::Unavailable)),
    });
    let r = bridge_events::relay_due(&h.state, sink.as_ref(), 100)
        .await
        .unwrap();
    assert_eq!(r.retried, 4);
    assert_eq!(
        bridge_events::relay_due(&h.state, sink.as_ref(), 100)
            .await
            .unwrap(),
        Default::default(),
        "not due before the backoff"
    );
    *sink.outcome.lock().unwrap() = Ok(());
    h.clock.advance(chrono::Duration::minutes(5));
    let r = bridge_events::relay_due(&h.state, sink.as_ref(), 100)
        .await
        .unwrap();
    assert_eq!(r.acked, 4);
    let bodies = sink.bodies.lock().unwrap().clone();
    let last = bodies.last().unwrap();
    let mut keys: Vec<&String> = last.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "event_id",
            "occurred_at",
            "state",
            "subject",
            "type",
            "version"
        ]
    );
    assert_eq!(
        last["subject"],
        json!({ "kind": "incident", "id": incident })
    );
    assert_eq!(bodies[4]["subject"]["kind"], "abuse_report");
    assert_eq!(bodies[4]["subject"]["id"], json!(rid));
    // A retry re-sends the same event id (the bridge dedupes on it).
    assert_eq!(bodies[0]["event_id"], bodies[4]["event_id"]);
    let text = serde_json::to_string(&bodies).unwrap();
    assert!(!text.contains("reported") && !text.contains("HARASSMENT"));
    assert!(events(&h).await.iter().all(|e| e.3 == "ACKED"));

    // A contract refusal dead-letters the event.
    bridge_events::install(true);
    operations::raise(h.conn(), IncidentCategory::ScannerUnavailable, "x", h.now()).await;
    bridge_events::install(false);
    *sink.outcome.lock().unwrap() = Err(PublishFailure::Rejected);
    let r = bridge_events::relay_due(&h.state, sink.as_ref(), 100)
        .await
        .unwrap();
    assert_eq!(r.dead, 1);
    // Settled rows are purged after the retention.
    h.clock.advance(chrono::Duration::days(8));
    assert_eq!(bridge_events::purge(&h.state, 100).await.unwrap(), 5);
}

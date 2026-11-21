//! The dataset collector against the memory store and a fake owner: owner pulls with the
//! sync operator, feed counts per producer as of each day's end, the bridge's own
//! quarantine count, null (never 0) for what no source reported, and idempotent merges.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    datasets::{collect, SYNC_OPERATOR},
    owners::{OwnerError, OwnerPort},
    store::{OwnerEvent, Store},
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::*;
use serde_json::{json, Value};

/// (owner, path, operator) of one owner call.
type Recorded = (Owner, String, String);

#[derive(Default)]
struct FakeNotifications {
    calls: Mutex<Vec<Recorded>>,
}

#[async_trait]
impl OwnerPort for FakeNotifications {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        _: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        self.calls
            .lock()
            .unwrap()
            .push((owner, path.to_string(), operator.to_string()));
        let day = path.rsplit('/').next().unwrap().to_string();
        let dataset = path.split('/').nth(5).unwrap().to_string();
        let yesterday = (Utc::now().date_naive() - Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        let y = day == yesterday;
        // Yesterday is countable; older days are null (beyond the owner's retention).
        let n = |v: i64| if y { json!(v) } else { Value::Null };
        let metrics = match (owner, dataset.as_str()) {
            (Owner::Notifications, "cfmeet.operations_health") => {
                json!({"failed_deliveries": n(7), "extra": 1})
            }
            (Owner::Messaging, "cfmeet.operations_health") => json!({"failed_messages": n(2)}),
            (Owner::Messaging, "cfmeet.messaging_daily") => json!({"messages_sent": n(42)}),
            (Owner::DeveloperPlatform, "cfmeet.partners_daily") => json!({
                "workspaces_active": n(5), "workspaces_restricted": n(0), "workspaces_suspended": n(1),
                "partners_live": n(2), "partner_mau": null}),
            _ => return Err(OwnerError::NotFound),
        };
        Ok(json!({"datasetId": dataset, "date": day, "metrics": metrics}))
    }
}

fn event(
    producer: &str,
    kind: &str,
    subject: &str,
    t: &str,
    state: &str,
    v: i64,
    at: DateTime<Utc>,
) -> OwnerEvent {
    OwnerEvent {
        producer: producer.into(),
        event_id: uuid::Uuid::new_v4(),
        event_type: t.into(),
        subject_kind: kind.into(),
        subject_id: subject.into(),
        state: state.into(),
        version: v,
        occurred_at: at,
    }
}

async fn day(h: &Harness, id: &str, d: NaiveDate) -> Option<admin_bridge::store::DatasetDay> {
    h.store
        .dataset_days(id, d, d, 1)
        .await
        .unwrap()
        .into_iter()
        .next()
}

#[tokio::test]
async fn the_collector_fills_dataset_days_from_pulls_events_and_the_bridge() {
    let fake = Arc::new(FakeNotifications::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        ..Default::default()
    })
    .await;
    let today = Utc::now().date_naive();
    let yesterday = today - Duration::days(1);
    let before = today - Duration::days(3);
    let at = |d: NaiveDate, hour: u32| d.and_hms_opt(hour, 0, 0).unwrap().and_utc();
    let s = &h.store;
    let ingest = |e: OwnerEvent, received: DateTime<Utc>| async move {
        s.ingest_event(&e, true, received).await.unwrap();
    };
    // Two Messaging reports: one opened three days ago and closed yesterday, one opened
    // yesterday. Calling never sent a report: no stream, so open_calling stays null.
    let t = "abuse.report.changed.v1";
    ingest(
        event(
            "messaging",
            "abuse_report",
            "r1",
            t,
            "OPEN",
            1,
            at(before, 9),
        ),
        at(before, 9),
    )
    .await;
    ingest(
        event(
            "messaging",
            "abuse_report",
            "r1",
            t,
            "DISMISSED",
            2,
            at(yesterday, 8),
        ),
        at(yesterday, 8),
    )
    .await;
    ingest(
        event(
            "messaging",
            "abuse_report",
            "r2",
            t,
            "OPEN",
            1,
            at(yesterday, 10),
        ),
        at(yesterday, 10),
    )
    .await;
    // A verification case, and one quarantined event yesterday.
    let v = "verification.case.changed.v1";
    ingest(
        event(
            "developer-platform",
            "verification_case",
            "cfmeet:APP_REVIEW:a",
            v,
            "CLAIMED",
            2,
            at(yesterday, 11),
        ),
        at(yesterday, 11),
    )
    .await;
    s.ingest_event(
        &event(
            "notifications",
            "incident",
            "i1",
            "incident.changed.v1",
            "OPEN",
            1,
            at(yesterday, 12),
        ),
        false,
        at(yesterday, 12),
    )
    .await
    .unwrap();

    let stats = collect(&h.bridge).await;
    assert!(stats.merged > 0);
    // Pulls: the sync operator on the owner's day route, bounded.
    let calls = fake.calls.lock().unwrap().clone();
    assert!(!calls.is_empty() && calls.len() <= admin_bridge::datasets::MAX_PULLS_PER_TICK);
    let (owner, path, operator) = &calls[0];
    assert_eq!(*owner, Owner::Notifications);
    assert_eq!(operator, SYNC_OPERATOR);
    assert_eq!(
        path,
        &format!(
            "/internal/v1/operator/datasets/cfmeet.operations_health/days/{}",
            yesterday.format("%Y-%m-%d")
        )
    );

    let tsq = day(&h, "cfmeet.trust_safety_queue", yesterday)
        .await
        .unwrap();
    assert_eq!(tsq.metrics["open_messaging"], 1, "r1 closed, r2 open");
    assert!(
        tsq.metrics.get("open_calling").is_none(),
        "no stream: null, never 0"
    );
    assert_eq!(tsq.sources_reported, ["messaging:abuse.report.changed.v1"]);
    let older = day(&h, "cfmeet.trust_safety_queue", before).await.unwrap();
    assert_eq!(
        older.metrics["open_messaging"], 1,
        "r1 was open at that day's end"
    );
    // Before the stream started: nothing reported.
    assert!(
        day(&h, "cfmeet.trust_safety_queue", before - Duration::days(1))
            .await
            .is_none()
    );

    let vq = day(&h, "cfmeet.verification_queue", yesterday)
        .await
        .unwrap();
    assert_eq!(
        (
            vq.metrics["in_review"].clone(),
            vq.metrics["pending"].clone()
        ),
        (json!(1), json!(0))
    );

    let ops = day(&h, "cfmeet.operations_health", yesterday)
        .await
        .unwrap();
    assert_eq!(ops.metrics["failed_deliveries"], 7);
    assert_eq!(ops.metrics["quarantined_events"], 1);
    assert_eq!(ops.metrics["projection_gaps"], 0);
    assert!(
        ops.metrics.get("extra").is_none(),
        "only the pull's own metrics"
    );
    assert!(
        ops.metrics.get("open_incidents").is_none(),
        "no incident stream yet"
    );
    assert!(ops
        .sources_reported
        .contains(&"notifications:PULL".to_string()));
    assert!(ops
        .sources_reported
        .contains(&"admin-bridge:PULL".to_string()));
    // A null pull answer is merged as nothing: the source has not reported that day.
    let old = day(&h, "cfmeet.operations_health", before).await.unwrap();
    assert!(old.metrics.get("failed_deliveries").is_none());
    assert!(!old
        .sources_reported
        .contains(&"notifications:PULL".to_string()));

    // A second pass changes nothing that was reported (idempotent merges).
    collect(&h.bridge).await;
    let again = day(&h, "cfmeet.trust_safety_queue", yesterday)
        .await
        .unwrap();
    assert_eq!(again.metrics, tsq.metrics);
    assert_eq!(again.sources_reported, tsq.sources_reported);
}

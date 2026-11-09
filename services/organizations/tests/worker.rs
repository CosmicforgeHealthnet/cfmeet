//! The maintenance loop and the outbox lease protocol.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use common::{s, Harness};
use organizations::{
    outbox::{self, DeliveryError, DisabledDispatcher, EventDispatcher},
    worker,
};
use serde_json::Value;
use uuid::Uuid;

struct Recording {
    sent: AtomicUsize,
    fail: bool,
}

const MEETING: &str = "cfmeet.meeting-service";

fn meeting() -> Vec<String> {
    vec![MEETING.to_string()]
}

#[async_trait]
impl EventDispatcher for Recording {
    fn destinations(&self) -> Vec<String> {
        meeting()
    }

    async fn deliver(&self, destination: &str, envelope: &Value) -> Result<(), DeliveryError> {
        assert_eq!(
            destination, MEETING,
            "the reconciliation record is never sent"
        );
        assert!(envelope["eventId"].is_string());
        self.sent.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(DeliveryError {
                class: "UPSTREAM_UNAVAILABLE".into(),
                permanent: false,
            })
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn the_maintenance_pass_expires_invitations_and_sweeps_nonces_and_idempotency() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "worker-org").await;
    let id = s(&org["id"]);
    let (inv, _) = h.invite(&id, owner, "late@example.com", "MEMBER").await;
    h.invite(&id, owner, "fresh@example.com", "MEMBER").await;
    h.exec(&format!(
        "UPDATE organization_invitations SET created_at = now() - interval '8 days', expires_at = now() - interval '1 day' WHERE id = '{}'",
        s(&inv["id"])
    ))
    .await
    .unwrap();
    h.exec("INSERT INTO organization_service_nonces (issuer, token_id, expires_at) VALUES ('cfmeet.meeting-service', 'old-1', now() - interval '1 hour')").await.unwrap();
    h.exec("INSERT INTO organization_idempotency_records (scope_key, idempotency_key, request_digest, response_status, response_body, created_at) VALUES ('s', 'k', repeat('0', 64), 201, '{}', now() - interval '2 days')").await.unwrap();

    let report = worker::run_once(&h.db.conn, &DisabledDispatcher)
        .await
        .unwrap();
    assert_eq!(report.invitations_expired, 1);
    assert_eq!(report.nonces_swept, 1);
    assert_eq!(report.idempotency_swept, 1);
    assert_eq!(report.events_delivered, 0);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_invitations WHERE id = '{}' AND status = 'EXPIRED' AND decided_at IS NOT NULL", s(&inv["id"]))).await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_invitations WHERE status = 'PENDING'")
            .await,
        1
    );
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE event_type = 'invitation.expired' AND payload->'actor'->>'kind' = 'SYSTEM'").await, 1);
    assert_eq!(h.count("SELECT count(*) FROM organization_audit_events WHERE action = 'invitation.expired' AND actor_kind = 'SYSTEM'").await, 1);
    // Live nonces stay (replay protection); a second pass finds nothing.
    assert!(
        h.count("SELECT count(*) FROM organization_service_nonces")
            .await
            >= 1
    );
    assert_eq!(
        worker::run_once(&h.db.conn, &DisabledDispatcher)
            .await
            .unwrap(),
        worker::WorkerReport::default()
    );
    // Delivery is disabled: every row (record and consumer) stays PENDING.
    assert_eq!(
        h.count("SELECT count(*) FROM organization_outbox WHERE state <> 'PENDING'")
            .await,
        0
    );
}

#[tokio::test]
async fn outbox_leases_are_exclusive_and_failures_back_off() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    h.create_org(owner, "outbox-org").await;
    // organization.created + membership.activated, each once as the reconciliation
    // record and once for the Meeting Service.
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox").await, 4);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_outbox WHERE destination = '{MEETING}'"
        ))
        .await,
        2
    );
    // A dispatcher without destinations claims nothing; the record is never claimed.
    assert!(outbox::claim_due(&h.db.conn, Uuid::new_v4(), 10, &[])
        .await
        .unwrap()
        .is_empty());
    assert!(outbox::claim_due(
        &h.db.conn,
        Uuid::new_v4(),
        10,
        &["cfmeet.reconciliation".to_string()]
    )
    .await
    .unwrap()
    .is_empty());

    // Two concurrent claimers never share a row.
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let first = outbox::claim_due(&h.db.conn, a, 1, &meeting())
        .await
        .unwrap();
    let second = outbox::claim_due(&h.db.conn, b, 10, &meeting())
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].seq, second[0].seq);
    assert!(
        outbox::claim_due(&h.db.conn, Uuid::new_v4(), 10, &meeting())
            .await
            .unwrap()
            .is_empty()
    );
    // A failure releases the lease and schedules a retry; a stale lease cannot settle.
    outbox::settle(&h.db.conn, &first[0], b, Ok(()))
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM organization_outbox WHERE state = 'DELIVERED'")
            .await,
        0
    );
    outbox::settle(
        &h.db.conn,
        &first[0],
        a,
        Err(DeliveryError {
            class: "TIMEOUT".into(),
            permanent: false,
        }),
    )
    .await
    .unwrap();
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE state = 'PENDING' AND next_attempt_at > now() AND last_error_class = 'TIMEOUT'").await, 1);
    outbox::settle(
        &h.db.conn,
        &second[0],
        b,
        Err(DeliveryError {
            class: "REJECTED".into(),
            permanent: true,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM organization_outbox WHERE state = 'DEAD'")
            .await,
        1
    );
    // An expired lease is reclaimed.
    h.exec("UPDATE organization_outbox SET next_attempt_at = now() WHERE state = 'PENDING'")
        .await
        .unwrap();
    let c = outbox::claim_due(&h.db.conn, Uuid::new_v4(), 10, &meeting())
        .await
        .unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].attempts, 2);
    h.exec("UPDATE organization_outbox SET lease_until = now() - interval '1 second' WHERE state = 'INFLIGHT'").await.unwrap();
    assert_eq!(
        outbox::claim_due(&h.db.conn, Uuid::new_v4(), 10, &meeting())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_enabled_dispatcher_delivers_each_pending_event() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.create_org(Uuid::new_v4(), "deliver-org").await;
    let failing = Recording {
        sent: AtomicUsize::new(0),
        fail: true,
    };
    worker::run_once(&h.db.conn, &failing).await.unwrap();
    assert_eq!(failing.sent.load(Ordering::SeqCst), 2);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_outbox WHERE destination = '{MEETING}' AND state = 'PENDING' AND attempts = 1"
        ))
        .await,
        2
    );
    // The reconciliation record is untouched.
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND state = 'PENDING' AND attempts = 0").await, 2);
    h.exec("UPDATE organization_outbox SET next_attempt_at = now()")
        .await
        .unwrap();
    let ok = Arc::new(Recording {
        sent: AtomicUsize::new(0),
        fail: false,
    });
    let report = worker::run_once(&h.db.conn, ok.as_ref()).await.unwrap();
    assert_eq!(report.events_delivered, 2);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE destination = '{MEETING}' AND state = 'DELIVERED' AND delivered_at IS NOT NULL")).await, 2);
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND state = 'DELIVERED'").await, 0);
    assert_eq!(
        worker::run_once(&h.db.conn, ok.as_ref())
            .await
            .unwrap()
            .events_delivered,
        0
    );
}

#[tokio::test]
async fn the_worker_loop_runs_until_shutdown() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(worker::run(
        h.db.conn.clone(),
        Arc::new(DisabledDispatcher),
        std::time::Duration::from_millis(50),
        async move {
            let _ = rx.await;
        },
    ));
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_consumer_receives_each_aggregate_in_version_order_and_replay_resumes_after_a_dead_letter(
) {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "order-org").await;
    let id = s(&org["id"]);
    let member = Uuid::new_v4();
    h.add_member(&id, owner, member, "MEMBER").await;
    // Two more versions of the same membership aggregate: role change, then removal.
    let mid = h
        .count(&format!(
            "SELECT count(*) FROM organization_outbox WHERE destination = '{MEETING}' AND event_type = 'membership.activated'"
        ))
        .await;
    assert_eq!(mid, 2);
    let membership: String = {
        use sea_orm::{ConnectionTrait, Statement};
        h.db.conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT id::text FROM organization_memberships WHERE user_id = '{member}'"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    };
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{id}/members/{membership}"),
            serde_json::json!({ "role": "ADMIN", "expectedVersion": 1 }),
        )
        .await
        .expect(200);
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{id}/members/{membership}"),
            serde_json::json!({ "status": "REMOVED", "expectedVersion": 2 }),
        )
        .await
        .expect(200);
    // The first event of the membership dead-letters (permanent refusal).
    let lease = Uuid::new_v4();
    let claimed = outbox::claim_due(&h.db.conn, lease, 50, &meeting())
        .await
        .unwrap();
    let types: Vec<String> = claimed
        .iter()
        .map(|r| r.payload["eventType"].as_str().unwrap().to_string())
        .collect();
    // Only the first version of each aggregate is claimable: the membership's later
    // versions wait for their predecessor.
    assert_eq!(
        types
            .iter()
            .filter(|t| t.starts_with("membership."))
            .count(),
        2,
        "{types:?}"
    );
    assert!(
        !types.contains(&"membership.removed".to_string()),
        "{types:?}"
    );
    for row in &claimed {
        let refused = row.payload["aggregateId"].as_str() == Some(membership.as_str());
        let outcome = if refused {
            Err(DeliveryError {
                class: "REFUSED_400".into(),
                permanent: true,
            })
        } else {
            Ok(())
        };
        outbox::settle(&h.db.conn, row, lease, outcome)
            .await
            .unwrap();
    }
    // Its successors stay held while the predecessor is dead.
    assert!(
        outbox::claim_due(&h.db.conn, Uuid::new_v4(), 50, &meeting())
            .await
            .unwrap()
            .is_empty()
    );
    let status = outbox::destination_status(&h.db.conn).await.unwrap();
    assert_eq!(
        status.len(),
        1,
        "the reconciliation record is not a consumer"
    );
    assert_eq!((status[0].dead, status[0].pending), (1, 2));
    assert_eq!(status[0].last_error_class.as_deref(), Some("REFUSED_400"));

    // Operator replay through the API (organization.administer), audited.
    let r = h
        .by(&h.peers.operator)
        .post(
            "/internal/v1/admin/event-delivery/replay",
            serde_json::json!({ "destination": MEETING }),
        )
        .await;
    assert_eq!(r.expect(200)["replayed"], 1);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_audit_events WHERE action = 'event_delivery.replayed'")
            .await,
        1
    );
    // Each pass delivers the next version of the membership, in order.
    let mut order = Vec::new();
    for _ in 0..4 {
        let before: Vec<String> = {
            let lease = Uuid::new_v4();
            let rows = outbox::claim_due(&h.db.conn, lease, 50, &meeting())
                .await
                .unwrap();
            let t = rows
                .iter()
                .map(|r| r.payload["eventType"].as_str().unwrap().to_string())
                .collect();
            for row in &rows {
                outbox::settle(&h.db.conn, row, lease, Ok(()))
                    .await
                    .unwrap();
            }
            t
        };
        order.extend(before);
    }
    assert_eq!(
        order,
        [
            "membership.activated",
            "membership.role_changed",
            "membership.removed"
        ],
        "one version per pass, in aggregate order"
    );
    let r = h
        .by(&h.peers.operator)
        .get("/internal/v1/admin/event-delivery")
        .await;
    let d = &r.expect(200)["destinations"][0];
    assert_eq!(
        (d["dead"].as_i64(), d["pending"].as_i64()),
        (Some(0), Some(0))
    );
    // A caller without organization.administer cannot read or replay.
    h.by(&h.peers.messaging)
        .get("/internal/v1/admin/event-delivery")
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    h.by(&h.peers.operator)
        .post(
            "/internal/v1/admin/event-delivery/replay",
            serde_json::json!({ "destination": "cfmeet.reconciliation" }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
}

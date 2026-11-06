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

#[async_trait]
impl EventDispatcher for Recording {
    fn enabled(&self) -> bool {
        true
    }

    async fn deliver(&self, _destination: &str, envelope: &Value) -> Result<(), DeliveryError> {
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
    // Delivery is disabled: every event stays PENDING for reconciliation.
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
    let total = h.count("SELECT count(*) FROM organization_outbox").await;
    assert_eq!(total, 2, "organization.created + membership.activated");

    // Two concurrent claimers never share a row.
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let first = outbox::claim_due(&h.db.conn, a, 1).await.unwrap();
    let second = outbox::claim_due(&h.db.conn, b, 10).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].seq, second[0].seq);
    assert!(outbox::claim_due(&h.db.conn, Uuid::new_v4(), 10)
        .await
        .unwrap()
        .is_empty());
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
    let c = outbox::claim_due(&h.db.conn, Uuid::new_v4(), 10)
        .await
        .unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].attempts, 2);
    h.exec("UPDATE organization_outbox SET lease_until = now() - interval '1 second' WHERE state = 'INFLIGHT'").await.unwrap();
    assert_eq!(
        outbox::claim_due(&h.db.conn, Uuid::new_v4(), 10)
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
        h.count(
            "SELECT count(*) FROM organization_outbox WHERE state = 'PENDING' AND attempts = 1"
        )
        .await,
        2
    );
    h.exec("UPDATE organization_outbox SET next_attempt_at = now()")
        .await
        .unwrap();
    let ok = Arc::new(Recording {
        sent: AtomicUsize::new(0),
        fail: false,
    });
    let report = worker::run_once(&h.db.conn, ok.as_ref()).await.unwrap();
    assert_eq!(report.events_delivered, 2);
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE state = 'DELIVERED' AND delivered_at IS NOT NULL").await, 2);
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

//! The Organizations maintenance loop (runs inside the API process; every step is
//! idempotent and multi-instance safe):
//!
//! 1. sweep expired service-token nonces (replay protection keeps a nonce until 60 s
//!    after its token expired);
//! 2. sweep idempotency records older than 24 hours;
//! 3. expire PENDING invitations past `expires_at` (`invitation.expired` event + audit,
//!    rows claimed with `FOR UPDATE SKIP LOCKED`);
//! 4. deliver due outbox rows when a dispatcher is enabled (none is in Section 5).

use std::{sync::Arc, time::Duration};

use sea_orm::{DatabaseConnection, DbErr, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use crate::{
    db,
    outbox::{self, EventDispatcher},
    store::{self, Event, InvitationRow, OpContext, INVITATION_COLUMNS},
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkerReport {
    pub nonces_swept: u64,
    pub idempotency_swept: u64,
    pub invitations_expired: u64,
    pub events_delivered: u64,
}

/// Expires due PENDING invitations (at most `limit`).
pub async fn expire_invitations(db: &DatabaseConnection, limit: i64) -> Result<u64, DbErr> {
    let txn = db.begin().await?;
    let now = db::now(&txn).await?;
    let due: Vec<InvitationRow> = db::all(
        &txn,
        &format!(
            "UPDATE organization_invitations SET status = 'EXPIRED', decided_at = now(), version = version + 1 \
             WHERE id IN (SELECT id FROM organization_invitations WHERE status = 'PENDING' AND expires_at <= now() ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED) \
             RETURNING {INVITATION_COLUMNS}"
        ),
        vec![limit.into()],
    )
    .await?;
    let ctx = OpContext::system(cf_service_auth::services::ORGANIZATIONS);
    for inv in &due {
        store::emit(
            &txn,
            &ctx,
            now,
            Event {
                organization_id: inv.organization_id,
                aggregate_type: "invitation",
                aggregate_id: inv.id,
                aggregate_version: inv.version,
                event_type: "invitation.expired",
                payload: json!({ "invitationId": inv.id, "role": inv.role }),
            },
        )
        .await?;
        store::audit(
            &txn,
            &ctx,
            now,
            Some(inv.organization_id),
            "invitation.expired",
            "invitation",
            Some(inv.id),
            json!({ "role": inv.role }),
        )
        .await?;
    }
    txn.commit().await?;
    Ok(due.len() as u64)
}

/// One pass of the loop.
pub async fn run_once(
    db: &DatabaseConnection,
    dispatcher: &dyn EventDispatcher,
) -> Result<WorkerReport, DbErr> {
    let nonces_swept = db::exec(
        db,
        "DELETE FROM organization_service_nonces WHERE ctid IN (SELECT ctid FROM organization_service_nonces WHERE expires_at < now() LIMIT 5000)",
        vec![],
    )
    .await?;
    let idempotency_swept = db::exec(
        db,
        "DELETE FROM organization_idempotency_records WHERE ctid IN (SELECT ctid FROM organization_idempotency_records WHERE created_at < now() - interval '24 hours' LIMIT 5000)",
        vec![],
    )
    .await?;
    let invitations_expired = expire_invitations(db, 200).await?;
    let mut events_delivered = 0;
    if dispatcher.enabled() {
        let lease = Uuid::new_v4();
        for row in outbox::claim_due(db, lease, 50).await? {
            let outcome = dispatcher.deliver(&row.destination, &row.payload).await;
            if outcome.is_ok() {
                events_delivered += 1;
            }
            outbox::settle(db, &row, lease, outcome).await?;
        }
    }
    Ok(WorkerReport {
        nonces_swept,
        idempotency_swept,
        invitations_expired,
        events_delivered,
    })
}

/// Runs [`run_once`] every `interval` until `shutdown` resolves.
pub async fn run(
    db: DatabaseConnection,
    dispatcher: Arc<dyn EventDispatcher>,
    interval: Duration,
    shutdown: impl std::future::Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                match run_once(&db, dispatcher.as_ref()).await {
                    Ok(r) if r != WorkerReport::default() => tracing::info!(
                        nonces = r.nonces_swept, idempotency = r.idempotency_swept,
                        invitations_expired = r.invitations_expired, delivered = r.events_delivered,
                        "organizations maintenance pass"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(summary = %db::summarize_db_error(&e), "organizations maintenance pass failed"),
                }
            }
        }
    }
}

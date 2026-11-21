//! The Organizations maintenance loop (runs inside the API process; every step is
//! idempotent and multi-instance safe):
//!
//! 1. sweep expired service-token nonces (replay protection keeps a nonce until 60 s
//!    after its token expired);
//! 2. sweep idempotency records older than 24 hours;
//! 3. expire PENDING invitations past `expires_at` (`invitation.expired` event + audit,
//!    rows claimed with `FOR UPDATE SKIP LOCKED`);
//! 4. deliver due outbox rows to the configured consumers ([`outbox::HttpEventDispatcher`];
//!    with no consumer configured every row stays PENDING);
//! 5. when provisioning is enabled, one pass of the Section 7 saga
//!    ([`crate::provisioning::run_once`]: enqueue, advance, tenant-status sync,
//!    reconciliation);
//! 6. when `CF_NOTIFICATIONS_BASE_URL` is set, one pass of the Notification Service
//!    publications ([`crate::notification_publications::run_once`]: expire / purge,
//!    discovery from the committed outbox record rows, relay to I1). Unset: this step
//!    does not run and touches nothing.
//! 7. owner-delivered exports ([`crate::exports::run_once`]): expiry, purge of the
//!    encrypted copies and row retention on every tick; with `CF_NOTIFICATIONS_BASE_URL`
//!    also the relay of the owners' link notices.

use std::{sync::Arc, time::Duration};

use sea_orm::{DatabaseConnection, DbErr, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use crate::{
    db, exports,
    notification_publications::{self, NotificationEventSink},
    outbox::{self, EventDispatcher},
    provisioning::{self, PlatformScopes},
    store::{self, Event, InvitationRow, OpContext, INVITATION_COLUMNS},
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkerReport {
    pub nonces_swept: u64,
    pub idempotency_swept: u64,
    pub invitations_expired: u64,
    pub events_delivered: u64,
    /// Gap audit G11: archived organizations purged this pass.
    pub organizations_purged: u64,
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

/// Gap audit G11: days after archival when an organization's personal data is purged
/// (`ORGANIZATIONS_ARCHIVE_PURGE_DAYS`; unset or 0 = never — the retention period is an
/// owner decision).
pub fn archive_purge_days() -> Option<i64> {
    std::env::var("ORGANIZATIONS_ARCHIVE_PURGE_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
}

/// Gap audit G11: purges organizations archived more than `days` ago (at most `limit` per
/// pass): their invitations (with invitee addresses) are deleted and the profile's
/// contact, address and legal details are cleared, in one transaction with an
/// `organization.purged` audit event. Identifiers, memberships, teams and the
/// append-only audit and history stay as evidence; the organization stays ARCHIVED.
pub async fn purge_archived(db: &DatabaseConnection, days: i64, limit: i64) -> Result<u64, DbErr> {
    use sea_orm::TransactionTrait;
    #[derive(Debug, sea_orm::FromQueryResult)]
    struct Due {
        id: Uuid,
    }
    let due: Vec<Due> = db::all(
        db,
        "SELECT o.id FROM organizations o WHERE o.lifecycle_status = 'ARCHIVED' \
           AND o.archived_at < now() - make_interval(days => $1::int) \
           AND NOT EXISTS (SELECT 1 FROM organization_audit_events a \
                            WHERE a.organization_id = o.id AND a.action = 'organization.purged') \
         ORDER BY o.archived_at LIMIT $2",
        vec![(days as i32).into(), limit.into()],
    )
    .await?;
    let mut purged = 0;
    for org in due {
        let txn = db.begin().await?;
        let invitations = db::exec(
            &txn,
            "DELETE FROM organization_invitations WHERE organization_id = $1",
            vec![org.id.into()],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE organization_profiles SET description = NULL, city = NULL, operating_address = NULL, \
                    logo_ref = NULL, public_contact_email = NULL, legal_registered_name = NULL, \
                    legal_registration_number = NULL, legal_incorporated_on = NULL, legal_verification = NULL \
              WHERE organization_id = $1",
            vec![org.id.into()],
        )
        .await?;
        crate::store::audit(
            &txn,
            &crate::store::OpContext::system("cfmeet.organizations"),
            chrono::Utc::now(),
            Some(org.id),
            "organization.purged",
            "ORGANIZATION",
            Some(org.id),
            serde_json::json!({ "afterDays": days, "invitationsDeleted": invitations }),
        )
        .await?;
        txn.commit().await?;
        purged += 1;
    }
    Ok(purged)
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
    let organizations_purged = match archive_purge_days() {
        Some(days) => purge_archived(db, days, 20).await?,
        None => 0,
    };
    let mut events_delivered = 0;
    if dispatcher.enabled() {
        let lease = Uuid::new_v4();
        let destinations = dispatcher.destinations();
        for row in outbox::claim_due(db, lease, 50, &destinations).await? {
            let outcome = dispatcher.deliver(&row.destination, &row.payload).await;
            match &outcome {
                Ok(()) => events_delivered += 1,
                Err(e) => tracing::warn!(
                    destination = %row.destination,
                    event_id = %row.event_id,
                    attempts = row.attempts,
                    error_class = %e.class,
                    permanent = e.permanent,
                    "organization event delivery failed"
                ),
            }
            outbox::settle(db, &row, lease, outcome).await?;
        }
    }
    Ok(WorkerReport {
        nonces_swept,
        idempotency_swept,
        invitations_expired,
        events_delivered,
        organizations_purged,
    })
}

/// Runs [`run_once`] every `interval` until `shutdown` resolves.
pub async fn run(
    db: DatabaseConnection,
    dispatcher: Arc<dyn EventDispatcher>,
    interval: Duration,
    shutdown: impl std::future::Future<Output = ()>,
) {
    run_with(db, dispatcher, None, None, interval, shutdown).await
}

/// [`run`] plus, when `provisioner` is set, one provisioning pass per tick and, when
/// `notifications` is set (`CF_NOTIFICATIONS_BASE_URL`), one notification publication
/// pass per tick.
pub async fn run_with(
    db: DatabaseConnection,
    dispatcher: Arc<dyn EventDispatcher>,
    provisioner: Option<Arc<dyn PlatformScopes>>,
    notifications: Option<Arc<dyn NotificationEventSink>>,
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
                if let Some(port) = &provisioner {
                    match provisioning::run_once(&db, port.as_ref()).await {
                        Ok(r) if r != provisioning::ProvisioningReport::default() => tracing::info!(
                            enqueued = r.enqueued, ready = r.ready, failed = r.failed, retried = r.retried,
                            waiting = r.waiting, synced = r.synced, sync_failed = r.sync_failed,
                            reconciled = r.reconciled, drifted = r.drifted,
                            "organizations provisioning pass"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(summary = %db::summarize_db_error(&e), "organizations provisioning pass failed"),
                    }
                }
                match exports::run_once(&db, notifications.as_deref()).await {
                    Ok(r) if r != exports::ExportReport::default() => tracing::info!(
                        sent = r.sent, retried = r.retried, failed = r.failed, expired = r.expired,
                        purged = r.purged, deleted = r.deleted,
                        "organizations export maintenance pass"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(summary = %db::summarize_db_error(&e), "organizations export maintenance pass failed"),
                }
                if let Some(sink) = &notifications {
                    match notification_publications::run_once(&db, sink.as_ref()).await {
                        Ok(r) if r != notification_publications::PublicationReport::default() => tracing::info!(
                            discovered = r.discovered, skipped = r.skipped, acked = r.acked,
                            retried = r.retried, dead = r.dead, expired = r.expired, purged = r.purged,
                            not_configured = r.not_configured,
                            "organizations notification publication pass"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(summary = %db::summarize_db_error(&e), "organizations notification publication pass failed"),
                    }
                }
            }
        }
    }
}

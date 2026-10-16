//! The retention worker (§80): one bounded tick per interval on every instance.
//!
//! Each class is one transaction per tick: `SET LOCAL privacy_relay.retention_worker =
//! 'on'` (the only way a DELETE passes the evidence tables' triggers), a `FOR UPDATE SKIP
//! LOCKED` claim of at most `limit` rows that aged past the class's bound and are not
//! under an active hold, the delete / erase, one retention-audit row, commit. Concurrent
//! instances take disjoint rows; a crash loses at most the uncommitted batch; a
//! re-run finds nothing left. Nothing here needs key material, and nothing here ever
//! reads a ciphertext.
//!
//! Relay evidence is purged per terminal context, children first in foreign-key order
//! (outbox → inbound resolutions → grants → audit events → enforcement jobs →
//! decisions → session children → relay assignments → sessions → context children →
//! commands → context). A context is skipped while any session is live, any grant is
//! ISSUED, any enforcement job is PENDING, any outbox row is unpublished, any relay
//! assignment is ACTIVE or still in COOLDOWN, or a hold covers the context, one of its
//! sessions or one of its protected-contact participants.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DatabaseTransaction, FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{
    audit::{self, RetentionAuditEntry},
    DataClass, RetentionAction, RetentionPolicy, RetentionSettings, DEFAULT_RETENTION_BATCH,
};
use crate::{clock::SharedClock, db, error::RelayResult};

/// What one tick did (bounded counts; the only thing logged).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RetentionReport {
    pub contacts_erased: usize,
    pub verification_events_deleted: usize,
    pub grants_deleted: usize,
    pub access_audit_deleted: usize,
    pub contexts_purged: usize,
    pub outbox_deleted: usize,
    pub number_events_deleted: usize,
}

impl RetentionReport {
    pub fn total(&self) -> usize {
        self.contacts_erased
            + self.verification_events_deleted
            + self.grants_deleted
            + self.access_audit_deleted
            + self.contexts_purged
            + self.outbox_deleted
            + self.number_events_deleted
    }
}

/// The retention worker of one instance.
#[derive(Clone)]
pub struct RetentionWorker {
    db: DatabaseConnection,
    clock: SharedClock,
    settings: RetentionSettings,
}

#[derive(Debug, FromQueryResult)]
struct ContextCandidate {
    id: Uuid,
}

/// Enables retention deletes for the rest of `txn` (never on a pooled connection).
async fn enable_retention_flag(txn: &DatabaseTransaction) -> RelayResult<()> {
    db::exec(
        txn,
        "SET LOCAL privacy_relay.retention_worker = 'on'",
        vec![],
    )
    .await
    .map(|_| ())
}

fn cutoff(now: DateTime<Utc>, policy: &RetentionPolicy) -> DateTime<Utc> {
    now - chrono::Duration::from_std(policy.retention).unwrap_or(chrono::Duration::zero())
}

impl RetentionWorker {
    pub fn new(db: DatabaseConnection, clock: SharedClock, settings: RetentionSettings) -> Self {
        Self {
            db,
            clock,
            settings,
        }
    }

    pub fn settings(&self) -> &RetentionSettings {
        &self.settings
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// One tick: every policy once, at most `limit` rows each.
    pub async fn retention_tick(&self, limit: u64) -> RelayResult<RetentionReport> {
        let limit = limit.clamp(1, DEFAULT_RETENTION_BATCH);
        let report = RetentionReport {
            contacts_erased: self.erase_contacts(limit).await?,
            verification_events_deleted: self.purge_verification_events(limit).await?,
            grants_deleted: self.purge_grants(limit).await?,
            access_audit_deleted: self.purge_access_audit(limit).await?,
            contexts_purged: self.purge_relay_evidence(limit).await?,
            outbox_deleted: self.purge_outbox(limit).await?,
            number_events_deleted: self.purge_number_events(limit).await?,
        };
        if report.total() > 0 {
            tracing::info!(
                contacts_erased = report.contacts_erased,
                verification_events_deleted = report.verification_events_deleted,
                grants_deleted = report.grants_deleted,
                access_audit_deleted = report.access_audit_deleted,
                contexts_purged = report.contexts_purged,
                outbox_deleted = report.outbox_deleted,
                number_events_deleted = report.number_events_deleted,
                "retention tick"
            );
        }
        Ok(report)
    }

    /// Runs [`retention_tick`](Self::retention_tick) forever.
    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = self.retention_tick(DEFAULT_RETENTION_BATCH).await {
                tracing::warn!(%error, "retention tick failed; will retry");
            }
        }
    }

    async fn audit_batch(
        &self,
        txn: &DatabaseTransaction,
        policy: &RetentionPolicy,
        count: usize,
        now: DateTime<Utc>,
    ) -> RelayResult<()> {
        if count == 0 {
            return Ok(());
        }
        crate::metrics::inc(
            "privacy_relay_retention_batches_total",
            &[
                ("data_class", policy.class.as_str()),
                ("action", policy.action.as_str()),
            ],
        );
        crate::metrics::add(
            "privacy_relay_retention_rows_total",
            &[
                ("data_class", policy.class.as_str()),
                ("action", policy.action.as_str()),
            ],
            count as u64,
        );
        audit::record(
            txn,
            &RetentionAuditEntry::batch(policy.class, policy.action, policy.label(), count),
            now,
        )
        .await
        .map(|_| ())
    }

    /// PROTECTED_CONTACT_VALUE: minimize (erase) contacts disabled past the delay.
    async fn erase_contacts(&self, limit: u64) -> RelayResult<usize> {
        let policy = self.settings.policy(DataClass::ProtectedContactValue);
        debug_assert_eq!(policy.action, RetentionAction::Minimize);
        let now = self.now();
        let txn = self.db.begin().await?;
        let report = crate::contact_vault::service::erase_disabled_contacts(
            &txn,
            cutoff(now, &policy),
            now,
            limit,
        )
        .await?;
        self.audit_batch(&txn, &policy, report.erased.len(), now)
            .await?;
        txn.commit().await?;
        Ok(report.erased.len())
    }

    /// A bounded DELETE of one class: `select_ids` yields the claim query (`$1` = cutoff,
    /// `$2` = class, `$3` = limit) whose ids `delete` removes.
    async fn purge_simple(
        &self,
        class: DataClass,
        claim_sql: &str,
        delete_sql: &str,
        limit: u64,
    ) -> RelayResult<usize> {
        #[derive(FromQueryResult)]
        struct Id {
            id: Uuid,
        }
        let policy = self.settings.policy(class);
        let now = self.now();
        let txn = self.db.begin().await?;
        enable_retention_flag(&txn).await?;
        let ids: Vec<Uuid> = db::all::<Id, _>(
            &txn,
            claim_sql,
            vec![
                cutoff(now, &policy).into(),
                class.as_str().into(),
                (limit as i64).into(),
            ],
        )
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();
        if ids.is_empty() {
            txn.rollback().await?;
            return Ok(0);
        }
        let deleted = db::exec(&txn, delete_sql, vec![ids.clone().into()]).await? as usize;
        self.audit_batch(&txn, &policy, deleted, now).await?;
        txn.commit().await?;
        Ok(deleted)
    }

    async fn purge_verification_events(&self, limit: u64) -> RelayResult<usize> {
        self.purge_simple(
            DataClass::ContactVerificationEvents,
            "SELECT e.id FROM contact_verification_events e WHERE e.occurred_at <= $1 \
               AND NOT privacy_relay_retention_held($2, e.contact_reference, e.tenant_id, e.application_id, e.environment_id) \
             ORDER BY e.occurred_at, e.id LIMIT $3 FOR UPDATE SKIP LOCKED",
            "DELETE FROM contact_verification_events WHERE id = ANY($1)",
            limit,
        )
        .await
    }

    async fn purge_grants(&self, limit: u64) -> RelayResult<usize> {
        self.purge_simple(
            DataClass::ContactResolutionGrants,
            "SELECT g.id FROM contact_resolution_grants g \
             WHERE g.status IN ('EXPIRED', 'CONSUMED', 'REVOKED') \
               AND COALESCE(g.consumed_at, g.revoked_at, g.expires_at) <= $1 \
               AND NOT privacy_relay_retention_held($2, g.contact_reference, g.tenant_id, g.application_id, g.environment_id) \
               AND NOT privacy_relay_retention_held($2, g.context_ref, g.tenant_id, g.application_id, g.environment_id) \
             ORDER BY g.expires_at, g.id LIMIT $3 FOR UPDATE SKIP LOCKED",
            "DELETE FROM contact_resolution_grants WHERE id = ANY($1)",
            limit,
        )
        .await
    }

    async fn purge_access_audit(&self, limit: u64) -> RelayResult<usize> {
        self.purge_simple(
            DataClass::ContactAccessAudit,
            "SELECT a.id FROM contact_access_audit a WHERE a.created_at <= $1 \
               AND NOT privacy_relay_retention_held($2, a.contact_reference, a.tenant_id, a.application_id, a.environment_id) \
             ORDER BY a.created_at, a.id LIMIT $3 FOR UPDATE SKIP LOCKED",
            "DELETE FROM contact_access_audit WHERE id = ANY($1)",
            limit,
        )
        .await
    }

    async fn purge_outbox(&self, limit: u64) -> RelayResult<usize> {
        self.purge_simple(
            DataClass::RelayOutbox,
            "SELECT o.id FROM privacy_relay_outbox o WHERE o.status = 'PUBLISHED' AND o.published_at <= $1 \
               AND NOT privacy_relay_retention_held($2, o.aggregate_id, o.tenant_id, o.application_id, o.environment_id) \
             ORDER BY o.published_at, o.id LIMIT $3 FOR UPDATE SKIP LOCKED",
            "DELETE FROM privacy_relay_outbox WHERE id = ANY($1)",
            limit,
        )
        .await
    }

    async fn purge_number_events(&self, limit: u64) -> RelayResult<usize> {
        let policy = self.settings.policy(DataClass::RelayNumberEvents);
        let now = self.now();
        let txn = self.db.begin().await?;
        enable_retention_flag(&txn).await?;
        let deleted =
            crate::relay_numbers::retention::purge_number_events(&txn, cutoff(now, &policy), limit)
                .await? as usize;
        if deleted == 0 {
            txn.rollback().await?;
            return Ok(0);
        }
        self.audit_batch(&txn, &policy, deleted, now).await?;
        txn.commit().await?;
        Ok(deleted)
    }

    /// RELAY_EVIDENCE: terminal contexts past the bound, with every child row.
    async fn purge_relay_evidence(&self, limit: u64) -> RelayResult<usize> {
        let policy = self.settings.policy(DataClass::RelayEvidence);
        let now = self.now();
        let txn = self.db.begin().await?;
        enable_retention_flag(&txn).await?;
        let class = DataClass::RelayEvidence.as_str();
        let candidates: Vec<ContextCandidate> = db::all(
            &txn,
            "SELECT c.id FROM communication_contexts c \
             WHERE c.status IN ('EXPIRED', 'REVOKED', 'COMPLETED') AND c.updated_at <= $1 \
               AND NOT privacy_relay_retention_held($2, c.id, c.tenant_id, c.application_id, c.environment_id) \
               AND NOT EXISTS (SELECT 1 FROM privacy_relay_sessions s WHERE s.context_id = c.id \
                   AND (s.status IN ('ACTIVE', 'SUSPENDED') OR s.updated_at > $1 \
                        OR privacy_relay_retention_held($2, s.id, s.tenant_id, s.application_id, s.environment_id))) \
               AND NOT EXISTS (SELECT 1 FROM communication_context_participants p WHERE p.context_id = c.id \
                   AND p.contact_ref IS NOT NULL \
                   AND privacy_relay_retention_held($2, p.contact_ref, p.tenant_id, p.application_id, p.environment_id)) \
               AND NOT EXISTS (SELECT 1 FROM contact_resolution_grants g WHERE g.context_ref = c.id AND g.status = 'ISSUED') \
               AND NOT EXISTS (SELECT 1 FROM privacy_relay_enforcement_jobs j WHERE j.context_id = c.id AND j.status <> 'APPLIED') \
               AND NOT EXISTS (SELECT 1 FROM privacy_relay_outbox o JOIN relay_audit_events e ON e.id = o.source_audit_event_id \
                   WHERE e.context_id = c.id AND o.status <> 'PUBLISHED') \
               AND NOT EXISTS (SELECT 1 FROM relay_number_assignments a WHERE a.context_id = c.id \
                   AND a.status IN ('ACTIVE', 'COOLDOWN')) \
             ORDER BY c.updated_at, c.id LIMIT $3 FOR UPDATE OF c SKIP LOCKED",
            vec![
                cutoff(now, &policy).into(),
                class.into(),
                (limit as i64).into(),
            ],
        )
        .await?;
        let ids: Vec<Uuid> = candidates.into_iter().map(|c| c.id).collect();
        if ids.is_empty() {
            txn.rollback().await?;
            return Ok(0);
        }
        let param: sea_orm::Value = ids.clone().into();
        for sql in [
            "DELETE FROM privacy_relay_outbox WHERE source_audit_event_id IN ( \
               SELECT id FROM relay_audit_events WHERE context_id = ANY($1))",
            "DELETE FROM relay_inbound_resolutions WHERE context_id = ANY($1) OR assignment_id IN ( \
               SELECT id FROM relay_number_assignments WHERE context_id = ANY($1))",
            "DELETE FROM contact_resolution_grants WHERE context_ref = ANY($1)",
            "DELETE FROM relay_audit_events WHERE context_id = ANY($1)",
            "DELETE FROM privacy_relay_enforcement_jobs WHERE context_id = ANY($1)",
            "DELETE FROM relay_authorization_decisions WHERE context_id = ANY($1)",
            "DELETE FROM privacy_relay_session_directions WHERE context_id = ANY($1)",
            "DELETE FROM privacy_relay_session_participants WHERE context_id = ANY($1)",
            "DELETE FROM privacy_relay_session_channels WHERE context_id = ANY($1)",
            "DELETE FROM privacy_relay_commands WHERE resource_id IN ( \
               SELECT id FROM privacy_relay_sessions WHERE context_id = ANY($1)) \
               OR result_resource_id IN (SELECT id FROM privacy_relay_sessions WHERE context_id = ANY($1))",
        ] {
            db::exec(&txn, sql, vec![param.clone()]).await?;
        }
        // Relay assignments reference their session (the session's binding is a plain
        // reference): assignments first, then the sessions.
        crate::relay_numbers::retention::purge_assignments_of_contexts(&txn, &ids).await?;
        for sql in [
            "DELETE FROM privacy_relay_sessions WHERE context_id = ANY($1)",
            "DELETE FROM communication_context_directions WHERE context_id = ANY($1)",
            "DELETE FROM communication_context_participants WHERE context_id = ANY($1)",
            "DELETE FROM communication_context_channels WHERE context_id = ANY($1)",
            "DELETE FROM privacy_relay_commands WHERE resource_id = ANY($1) OR result_resource_id = ANY($1)",
        ] {
            db::exec(&txn, sql, vec![param.clone()]).await?;
        }
        let deleted = db::exec(
            &txn,
            "DELETE FROM communication_contexts WHERE id = ANY($1)",
            vec![param],
        )
        .await? as usize;
        self.audit_batch(&txn, &policy, deleted, now).await?;
        txn.commit().await?;
        Ok(deleted)
    }
}

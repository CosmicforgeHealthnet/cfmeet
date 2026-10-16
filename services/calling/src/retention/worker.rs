//! The retention worker (§80): one bounded tick per interval on every instance.
//!
//! Each class with a Delete / Minimize policy is ONE transaction per tick:
//! `SET LOCAL calling.retention_worker = 'on'` (the only way the evidence tables' guard
//! triggers accept the operation), a `FOR UPDATE SKIP LOCKED` claim of at most `limit`
//! rows that aged past the class's bound and that no active hold covers, the delete /
//! minimization, one `calling_retention_audit` row, commit. Concurrent instances claim
//! disjoint rows and every action re-checks its own predicate, so a row is acted on (and
//! counted) once; a crash or a database error loses only the uncommitted batch — nothing
//! partial is ever visible — and the next tick retries it. A failed class does not stop
//! the others: it is logged (summary only), counted
//! (`calling_retention_actions_total{outcome="FAILED"}`) and audited when the database
//! still accepts the audit row.
//!
//! What a sweep never touches: non-terminal Calls; PENDING / DELIVERING /
//! RETRY_SCHEDULED deliveries (and the events they belong to); OPEN incidents; provider
//! events of live Calls; call events, usage, spend, the enterprise / recording access
//! audits (Retain); anything an active hold covers. Rows a hold keeps are recorded as
//! `SKIPPED_HOLD` evidence whenever their count changes (never once per tick).
//!
//! Recording and voicemail artifacts are deleted by their Part F workers (per-policy
//! retention), which honour the same holds.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use uuid::Uuid;

use super::{
    audit::{self, RetentionAuditEntry},
    DataClass, RetentionAction, RetentionOutcome, RetentionPolicy, RetentionSettings,
    MAX_RETENTION_BATCH,
};
use crate::{
    domain::SharedClock,
    platform::{PlatformError, PlatformResult},
};

/// What one tick did (bounded counts; the only thing logged).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub calls_minimized: usize,
    pub webhook_deliveries_deleted: usize,
    pub webhook_events_deleted: usize,
    pub incidents_deleted: usize,
    pub provider_events_deleted: usize,
    pub operator_audit_deleted: usize,
    /// Classes whose batch failed this tick (retried next tick).
    pub failed: Vec<DataClass>,
}

impl RetentionReport {
    pub fn total(&self) -> usize {
        self.calls_minimized
            + self.webhook_deliveries_deleted
            + self.webhook_events_deleted
            + self.incidents_deleted
            + self.provider_events_deleted
            + self.operator_audit_deleted
    }
}

/// What a sweep acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    CallDisplay,
    WebhookDeliveries,
    WebhookEvents,
    Incidents,
    MediaProviderEvents,
    TelephonyEvents,
    OperatorAudit,
}

/// One claimable source of a class: `SELECT {id} FROM {from} WHERE {due} AND NOT ({held})`.
/// In every fragment `$1` is the cutoff and `$2` the data class; `{held}` uses the
/// database's single hold-matching rule (`calling_retention_held`).
struct Sweep {
    target: Target,
    id: &'static str,
    from: &'static str,
    due: &'static str,
    held: &'static str,
    order: &'static str,
    lock: &'static str,
}

impl Sweep {
    fn claim_sql(&self) -> String {
        format!(
            "SELECT {id} FROM {from} WHERE {due} AND NOT ({held}) ORDER BY {order} LIMIT $3 \
             FOR UPDATE OF {lock} SKIP LOCKED",
            id = self.id,
            from = self.from,
            due = self.due,
            held = self.held,
            order = self.order,
            lock = self.lock,
        )
    }

    /// Due rows a hold keeps (bounded count).
    fn held_sql(&self) -> String {
        format!(
            "SELECT count(*) FROM (SELECT 1 FROM {from} WHERE {due} AND ({held}) LIMIT {max}) h",
            from = self.from,
            due = self.due,
            held = self.held,
            max = MAX_RETENTION_BATCH,
        )
    }
}

const CALL_RECORD: Sweep = Sweep {
    target: Target::CallDisplay,
    id: "c.id",
    from: "calls c",
    due: "c.ended_at IS NOT NULL AND c.ended_at <= $1 \
          AND (EXISTS (SELECT 1 FROM call_participants p WHERE p.call_id = c.id AND p.display_minimized_at IS NULL) \
               OR EXISTS (SELECT 1 FROM enterprise_call_contexts x WHERE x.call_id = c.id \
                          AND x.entry_label <> x.entry_extension_number))",
    held: "calling_retention_held($2, c.id, c.tenant_id, c.application_id, c.environment_id)",
    order: "c.ended_at, c.id",
    lock: "c",
};

const WEBHOOK_DELIVERIES: Sweep = Sweep {
    target: Target::WebhookDeliveries,
    id: "d.id",
    from: "webhook_deliveries d",
    due: "d.status IN ('SUCCEEDED', 'FAILED', 'CANCELLED') AND d.completed_at <= $1 \
          AND NOT EXISTS (SELECT 1 FROM webhook_deliveries r WHERE r.replay_of = d.id)",
    held: "calling_retention_held($2, d.id, d.tenant_id, d.application_id, d.environment_id) \
           OR calling_retention_held($2, d.webhook_event_id, d.tenant_id, d.application_id, d.environment_id) \
           OR calling_retention_held($2, d.subscription_id, d.tenant_id, d.application_id, d.environment_id)",
    order: "d.completed_at, d.id",
    lock: "d",
};

const WEBHOOK_EVENTS: Sweep = Sweep {
    target: Target::WebhookEvents,
    id: "e.id",
    from: "webhook_events e",
    due: "e.created_at <= $1 \
          AND NOT EXISTS (SELECT 1 FROM webhook_deliveries d WHERE d.webhook_event_id = e.id)",
    held: "calling_retention_held($2, e.id, e.tenant_id, e.application_id, e.environment_id)",
    order: "e.created_at, e.id",
    lock: "e",
};

const INCIDENTS: Sweep = Sweep {
    target: Target::Incidents,
    id: "i.id",
    from: "calling_incidents i",
    due: "i.status = 'RESOLVED' AND i.resolved_at <= $1",
    held: "calling_retention_held($2, i.id, i.tenant_id, i.application_id, i.environment_id) \
           OR (i.call_id IS NOT NULL \
               AND calling_retention_held($2, i.call_id, i.tenant_id, i.application_id, i.environment_id)) \
           OR (i.resource_id IS NOT NULL \
               AND calling_retention_held($2, i.resource_id, i.tenant_id, i.application_id, i.environment_id))",
    order: "i.resolved_at, i.id",
    lock: "i",
};

const MEDIA_PROVIDER_EVENTS: Sweep = Sweep {
    target: Target::MediaProviderEvents,
    id: "e.id",
    from: "call_media_provider_events e",
    due: "e.received_at <= $1 AND (e.call_id IS NULL \
          OR EXISTS (SELECT 1 FROM calls c WHERE c.id = e.call_id AND c.ended_at IS NOT NULL))",
    held: "calling_retention_held($2, e.id, e.tenant_id, e.application_id, e.environment_id) \
           OR (e.call_id IS NOT NULL \
               AND calling_retention_held($2, e.call_id, e.tenant_id, e.application_id, e.environment_id))",
    order: "e.received_at, e.id",
    lock: "e",
};

const TELEPHONY_EVENTS: Sweep = Sweep {
    target: Target::TelephonyEvents,
    id: "t.event_id",
    from: "call_telephony_events t JOIN calls c ON c.id = t.call_id",
    due: "t.received_at <= $1 AND c.ended_at IS NOT NULL",
    held: "calling_retention_held($2, t.event_id, c.tenant_id, c.application_id, c.environment_id) \
           OR calling_retention_held($2, t.call_id, c.tenant_id, c.application_id, c.environment_id)",
    order: "t.received_at, t.event_id",
    lock: "t",
};

const OPERATOR_AUDIT: Sweep = Sweep {
    target: Target::OperatorAudit,
    id: "a.id",
    from: "platform_operator_audit a",
    due: "a.occurred_at <= $1",
    held:
        "calling_retention_held($2, a.target_id, a.tenant_id, a.application_id, a.environment_id)",
    order: "a.occurred_at, a.id",
    lock: "a",
};

/// The classes the worker acts on, in sweep order (deliveries before their events).
pub const SWEPT_CLASSES: [DataClass; 6] = [
    DataClass::CallRecord,
    DataClass::WebhookDeliveryHistory,
    DataClass::WebhookEventHistory,
    DataClass::CallingIncident,
    DataClass::MediaProviderEvidence,
    DataClass::OperatorAudit,
];

fn sweeps(class: DataClass) -> &'static [Sweep] {
    match class {
        DataClass::CallRecord => std::slice::from_ref(&CALL_RECORD),
        DataClass::WebhookDeliveryHistory => std::slice::from_ref(&WEBHOOK_DELIVERIES),
        DataClass::WebhookEventHistory => std::slice::from_ref(&WEBHOOK_EVENTS),
        DataClass::CallingIncident => std::slice::from_ref(&INCIDENTS),
        DataClass::MediaProviderEvidence => {
            const BOTH: [Sweep; 2] = [MEDIA_PROVIDER_EVENTS, TELEPHONY_EVENTS];
            &BOTH
        }
        DataClass::OperatorAudit => std::slice::from_ref(&OPERATOR_AUDIT),
        _ => &[],
    }
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn cutoff(now: DateTime<Utc>, policy: &RetentionPolicy) -> DateTime<Utc> {
    let bound = policy.retention.unwrap_or(Duration::MAX);
    match chrono::Duration::from_std(bound) {
        Ok(d) => now
            .checked_sub_signed(d)
            .unwrap_or(DateTime::<Utc>::MIN_UTC),
        Err(_) => DateTime::<Utc>::MIN_UTC,
    }
}

fn error_summary(error: &PlatformError) -> String {
    match error {
        PlatformError::Database { summary, .. } => summary.clone(),
        other => other.to_string(),
    }
}

/// The retention worker of one instance.
#[derive(Clone)]
pub struct RetentionWorker {
    db: DatabaseConnection,
    clock: SharedClock,
    settings: RetentionSettings,
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

    /// One tick: every swept class once, at most `limit` (≤ 500) rows each.
    pub async fn retention_tick(&self, limit: u64) -> PlatformResult<RetentionReport> {
        let limit = limit.clamp(1, MAX_RETENTION_BATCH);
        let mut report = RetentionReport::default();
        for class in SWEPT_CLASSES {
            let acted = match self.run_class(class, limit).await {
                Ok(n) => n,
                Err(error) => {
                    self.record_failure(class, &error).await;
                    report.failed.push(class);
                    0
                }
            };
            match class {
                DataClass::CallRecord => report.calls_minimized = acted,
                DataClass::WebhookDeliveryHistory => report.webhook_deliveries_deleted = acted,
                DataClass::WebhookEventHistory => report.webhook_events_deleted = acted,
                DataClass::CallingIncident => report.incidents_deleted = acted,
                DataClass::MediaProviderEvidence => report.provider_events_deleted = acted,
                DataClass::OperatorAudit => report.operator_audit_deleted = acted,
                _ => {}
            }
        }
        if report.total() > 0 || !report.failed.is_empty() {
            tracing::info!(
                calls_minimized = report.calls_minimized,
                webhook_deliveries_deleted = report.webhook_deliveries_deleted,
                webhook_events_deleted = report.webhook_events_deleted,
                incidents_deleted = report.incidents_deleted,
                provider_events_deleted = report.provider_events_deleted,
                operator_audit_deleted = report.operator_audit_deleted,
                failed_classes = report.failed.len(),
                "retention tick"
            );
        }
        Ok(report)
    }

    /// Runs [`retention_tick`](Self::retention_tick) every configured interval, forever.
    pub async fn run(self) {
        let mut ticker = tokio::time::interval(self.settings.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = self.retention_tick(MAX_RETENTION_BATCH).await {
                tracing::warn!(error = %error_summary(&error), "retention tick failed; retried next tick");
            }
        }
    }

    /// One class: one transaction (flag, claim, act, audit, commit), then the hold
    /// evidence. Returns the rows acted on.
    async fn run_class(&self, class: DataClass, limit: u64) -> PlatformResult<usize> {
        let policy = self.settings.policy(class);
        let outcome = match policy.action {
            RetentionAction::Delete => RetentionOutcome::Deleted,
            RetentionAction::Minimize => RetentionOutcome::Minimized,
            RetentionAction::Retain => return Ok(0),
        };
        let now = self.clock.now();
        let cutoff = cutoff(now, &policy);
        let txn = self.db.begin().await?;
        txn.execute_unprepared("SET LOCAL calling.retention_worker = 'on'")
            .await?;
        let mut acted = 0usize;
        let mut remaining = limit;
        for sweep in sweeps(class) {
            if remaining == 0 {
                break;
            }
            let ids = claim(&txn, sweep, class, cutoff, remaining).await?;
            if ids.is_empty() {
                continue;
            }
            let n = act(&txn, sweep, &ids, now).await?;
            remaining = remaining.saturating_sub(ids.len() as u64);
            acted += n;
        }
        if acted == 0 {
            txn.rollback().await?;
        } else {
            audit::record(
                &txn,
                &RetentionAuditEntry::batch(class, policy.action, policy.label(), acted, outcome),
                now,
            )
            .await?;
            txn.commit().await?;
            audit::count(class, policy.action, outcome, acted as u32);
        }
        self.record_held(class, &policy, cutoff, now).await?;
        Ok(acted)
    }

    /// `SKIPPED_HOLD` evidence: written when the number of due rows a hold keeps differs
    /// from the last recorded number for the class (so a long hold is one row, not one
    /// per tick, and its release is visible as a return to zero).
    async fn record_held(
        &self,
        class: DataClass,
        policy: &RetentionPolicy,
        cutoff: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> PlatformResult<()> {
        let mut held = 0i64;
        for sweep in sweeps(class) {
            if let Some(row) = self
                .db
                .query_one(stmt(
                    &sweep.held_sql(),
                    vec![cutoff.into(), class.as_str().into()],
                ))
                .await?
            {
                held += row.try_get_by_index::<i64>(0)?;
            }
        }
        let last: i64 = match self
            .db
            .query_one(stmt(
                "SELECT batch_count::bigint FROM calling_retention_audit \
                 WHERE data_class = $1 AND outcome = 'SKIPPED_HOLD' \
                 ORDER BY occurred_at DESC, id DESC LIMIT 1",
                vec![class.as_str().into()],
            ))
            .await?
        {
            Some(row) => row.try_get_by_index(0)?,
            None => 0,
        };
        if held != last {
            let count = held.clamp(0, i64::from(i32::MAX)) as usize;
            audit::record(
                &self.db,
                &RetentionAuditEntry::batch(
                    class,
                    policy.action,
                    policy.label(),
                    count,
                    RetentionOutcome::SkippedHold,
                ),
                now,
            )
            .await?;
            audit::count(
                class,
                policy.action,
                RetentionOutcome::SkippedHold,
                count as u32,
            );
        }
        Ok(())
    }

    /// A failed class batch: nothing of it committed. Logged (summary only), counted and —
    /// when the database still takes it — audited; retried next tick.
    async fn record_failure(&self, class: DataClass, error: &PlatformError) {
        let policy = self.settings.policy(class);
        tracing::warn!(
            data_class = class.as_str(),
            error = %error_summary(error),
            "retention batch failed; nothing applied, retried next tick"
        );
        audit::count(class, policy.action, RetentionOutcome::Failed, 1);
        let entry = RetentionAuditEntry::batch(
            class,
            policy.action,
            policy.label(),
            0,
            RetentionOutcome::Failed,
        );
        if let Err(audit_error) = audit::record(&self.db, &entry, self.clock.now()).await {
            tracing::warn!(
                data_class = class.as_str(),
                error = %error_summary(&audit_error),
                "retention failure could not be audited"
            );
        }
    }
}

async fn claim(
    txn: &DatabaseTransaction,
    sweep: &Sweep,
    class: DataClass,
    cutoff: DateTime<Utc>,
    limit: u64,
) -> PlatformResult<Vec<Uuid>> {
    let rows = txn
        .query_all(stmt(
            &sweep.claim_sql(),
            vec![cutoff.into(), class.as_str().into(), (limit as i64).into()],
        ))
        .await?;
    rows.into_iter()
        .map(|r| r.try_get_by_index::<Uuid>(0).map_err(PlatformError::from))
        .collect()
}

/// Applies the sweep's action to claimed ids. Every statement re-checks its own
/// predicate, so a row another instance already handled is not counted twice.
async fn act(
    txn: &DatabaseTransaction,
    sweep: &Sweep,
    ids: &[Uuid],
    now: DateTime<Utc>,
) -> PlatformResult<usize> {
    let ids: sea_orm::Value = ids.to_vec().into();
    let delete = |sql: &'static str| {
        let ids = ids.clone();
        async move {
            txn.execute(stmt(sql, vec![ids]))
                .await
                .map(|r| r.rows_affected() as usize)
                .map_err(PlatformError::from)
        }
    };
    let minimized_calls = |sql: &'static str| {
        let ids = ids.clone();
        async move {
            let rows = txn.query_all(stmt(sql, vec![ids, now.into()])).await?;
            rows.into_iter()
                .map(|r| r.try_get_by_index::<Uuid>(0).map_err(PlatformError::from))
                .collect::<PlatformResult<Vec<Uuid>>>()
        }
    };
    match sweep.target {
        Target::CallDisplay => {
            // Display metadata only: ids, state, outcome, timestamps and every relation stay.
            let mut calls = minimized_calls(
                "UPDATE call_participants SET display_name = NULL, display_minimized_at = $2 \
                 WHERE call_id = ANY($1) AND display_minimized_at IS NULL RETURNING call_id",
            )
            .await?;
            calls.extend(
                minimized_calls(
                    "UPDATE enterprise_call_contexts SET entry_label = entry_extension_number, \
                       version = version + 1, updated_at = GREATEST(updated_at, $2) \
                     WHERE call_id = ANY($1) AND entry_label <> entry_extension_number RETURNING call_id",
                )
                .await?,
            );
            calls.sort_unstable();
            calls.dedup();
            Ok(calls.len())
        }
        Target::WebhookDeliveries => {
            delete("DELETE FROM webhook_delivery_attempts WHERE delivery_id = ANY($1)").await?;
            delete(
                "DELETE FROM webhook_deliveries WHERE id = ANY($1) \
                   AND status IN ('SUCCEEDED', 'FAILED', 'CANCELLED')",
            )
            .await
        }
        Target::WebhookEvents => {
            delete(
                "DELETE FROM webhook_events e WHERE e.id = ANY($1) \
                   AND NOT EXISTS (SELECT 1 FROM webhook_deliveries d WHERE d.webhook_event_id = e.id)",
            )
            .await
        }
        Target::Incidents => {
            delete("DELETE FROM calling_incidents WHERE id = ANY($1) AND status = 'RESOLVED'").await
        }
        Target::MediaProviderEvents => {
            delete("DELETE FROM call_media_provider_events WHERE id = ANY($1)").await
        }
        Target::TelephonyEvents => {
            delete("DELETE FROM call_telephony_events WHERE event_id = ANY($1)").await
        }
        Target::OperatorAudit => {
            delete("DELETE FROM platform_operator_audit WHERE id = ANY($1)").await
        }
    }
}

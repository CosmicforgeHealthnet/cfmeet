//! `calling_retention_audit`: append-only evidence of every retention batch and every
//! tombstone — data class, action, count, policy label, actor, outcome, time. Never the
//! deleted or minimized content, never a scope-revealing detail beyond an opaque id.
//! UPDATE / DELETE / TRUNCATE are refused by trigger (even for the retention worker).
//!
//! Every committed row is also counted in
//! `calling_retention_actions_total{data_class,action,outcome}` (bounded vocabularies only).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

use super::{DataClass, RetentionAction, RetentionOutcome};
use crate::platform::PlatformResult;

/// The actor of a system (worker) action.
pub const SYSTEM_ACTOR: &str = "SYSTEM";

/// One retention audit row.
#[derive(Debug, Clone)]
pub struct RetentionAuditEntry {
    pub class: DataClass,
    pub action: RetentionAction,
    /// An opaque resource id (a tombstoned principal); `None` for a worker batch.
    pub resource_ref: Option<Uuid>,
    pub batch_count: u32,
    /// Bounded policy label (`VAR=days` or a marker).
    pub policy: String,
    /// `SYSTEM` or the operator id.
    pub actor: String,
    pub outcome: RetentionOutcome,
}

impl RetentionAuditEntry {
    /// A worker batch.
    pub fn batch(
        class: DataClass,
        action: RetentionAction,
        policy: String,
        count: usize,
        outcome: RetentionOutcome,
    ) -> Self {
        Self {
            class,
            action,
            resource_ref: None,
            batch_count: count.min(i32::MAX as usize) as u32,
            policy,
            actor: SYSTEM_ACTOR.to_string(),
            outcome,
        }
    }
}

/// Appends one retention audit row with `conn` (the transaction of the action it
/// describes, so the row exists if and only if the action committed). The caller counts
/// it ([`count`]) once the transaction committed.
pub async fn record<C: ConnectionTrait>(
    conn: &C,
    entry: &RetentionAuditEntry,
    at: DateTime<Utc>,
) -> PlatformResult<Uuid> {
    let id = Uuid::new_v4();
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO calling_retention_audit (id, data_class, action, resource_ref, batch_count, policy, \
           actor, outcome, occurred_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        [
            id.into(),
            entry.class.as_str().into(),
            entry.action.as_str().into(),
            entry.resource_ref.into(),
            (entry.batch_count as i32).into(),
            entry.policy.clone().into(),
            entry.actor.clone().into(),
            entry.outcome.as_str().into(),
            at.into(),
        ],
    ))
    .await?;
    Ok(id)
}

/// `calling_retention_actions_total{data_class,action,outcome}` += `n` (rows acted on,
/// rows skipped under a hold, or 1 per failed batch).
pub fn count(class: DataClass, action: RetentionAction, outcome: RetentionOutcome, n: u32) {
    crate::metrics::add(
        "calling_retention_actions_total",
        &[
            ("data_class", class.as_str()),
            ("action", action.as_str()),
            ("outcome", outcome.as_str()),
        ],
        u64::from(n),
    );
}

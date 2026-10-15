//! `privacy_relay_retention_audit`: append-only evidence of every retention batch and
//! of every hold placed or released — class, action, count, policy, actor, outcome.
//! Never a value, never a scope-revealing detail beyond an opaque resource id.

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use super::{DataClass, RetentionAction};
use crate::{db, error::RelayResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionActor {
    System,
    Operator,
}

impl RetentionActor {
    pub fn as_str(&self) -> &'static str {
        match self {
            RetentionActor::System => "SYSTEM",
            RetentionActor::Operator => "OPERATOR",
        }
    }
}

/// What is recorded about one retention action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionAuditAction {
    Apply(RetentionAction),
    Hold,
    Release,
}

impl RetentionAuditAction {
    fn as_str(&self) -> &'static str {
        match self {
            RetentionAuditAction::Apply(a) => a.as_str(),
            RetentionAuditAction::Hold => "HOLD",
            RetentionAuditAction::Release => "RELEASE",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RetentionAuditEntry {
    /// The data class, or `*` for a hold over every class.
    pub data_class: String,
    pub action: RetentionAuditAction,
    pub resource_ref: Option<Uuid>,
    pub batch_count: u32,
    /// Bounded policy label (`VAR=days`, or a reason code for holds).
    pub policy: String,
    pub actor: RetentionActor,
    pub succeeded: bool,
}

impl RetentionAuditEntry {
    /// A worker batch under `policy` for `class`.
    pub fn batch(class: DataClass, action: RetentionAction, policy: String, count: usize) -> Self {
        Self {
            data_class: class.as_str().to_string(),
            action: RetentionAuditAction::Apply(action),
            resource_ref: None,
            batch_count: count.min(u32::MAX as usize) as u32,
            policy,
            actor: RetentionActor::System,
            succeeded: true,
        }
    }
}

/// Appends one retention audit row.
pub async fn record<C: ConnectionTrait>(
    conn: &C,
    entry: &RetentionAuditEntry,
    at: DateTime<Utc>,
) -> RelayResult<Uuid> {
    let id = Uuid::new_v4();
    db::exec(
        conn,
        "INSERT INTO privacy_relay_retention_audit (id, data_class, action, resource_ref, batch_count, policy, \
           actor, outcome, occurred_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![
            id.into(),
            entry.data_class.clone().into(),
            entry.action.as_str().into(),
            entry.resource_ref.into(),
            (entry.batch_count as i32).into(),
            entry.policy.clone().into(),
            entry.actor.as_str().into(),
            if entry.succeeded {
                "SUCCEEDED"
            } else {
                "FAILED"
            }
            .into(),
            at.into(),
        ],
    )
    .await?;
    Ok(id)
}

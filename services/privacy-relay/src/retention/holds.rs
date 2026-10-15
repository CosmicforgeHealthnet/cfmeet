//! Retention holds (§80): an operator freezes retention for one resource, one scope or
//! one data class (`*` = every class) while a policy review, a dispute or a security
//! investigation needs the data. Workers skip whatever an active hold covers; a hold
//! ends by being RELEASED (never deleted), and both steps are retention-audited.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{
    audit::{self, RetentionActor, RetentionAuditAction, RetentionAuditEntry},
    DataClass,
};
use crate::{
    db,
    error::{RelayError, RelayResult},
    scope::RelayScope,
};

/// Why a hold exists (bounded vocabulary; the database checks it too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    PolicyReview,
    Dispute,
    SecurityInvestigation,
    Other,
}

impl HoldReason {
    pub const ALL: [HoldReason; 4] = [
        HoldReason::PolicyReview,
        HoldReason::Dispute,
        HoldReason::SecurityInvestigation,
        HoldReason::Other,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            HoldReason::PolicyReview => "POLICY_REVIEW",
            HoldReason::Dispute => "DISPUTE",
            HoldReason::SecurityInvestigation => "SECURITY_INVESTIGATION",
            HoldReason::Other => "OTHER",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == value)
    }
}

/// What a hold covers: a data class (or every class), optionally one resource and / or
/// one scope (tenant, tenant + application, or the full scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldScope {
    pub data_class: Option<DataClass>,
    pub resource_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
}

impl HoldScope {
    pub fn everything() -> Self {
        Self {
            data_class: None,
            resource_id: None,
            tenant_id: None,
            application_id: None,
            environment_id: None,
        }
    }

    pub fn resource(id: Uuid) -> Self {
        Self {
            resource_id: Some(id),
            ..Self::everything()
        }
    }

    pub fn scope(scope: RelayScope) -> Self {
        Self {
            tenant_id: Some(scope.tenant_id()),
            application_id: Some(scope.application_id()),
            environment_id: Some(scope.environment_id()),
            ..Self::everything()
        }
    }

    pub fn class(mut self, class: DataClass) -> Self {
        self.data_class = Some(class);
        self
    }

    fn class_str(&self) -> &'static str {
        self.data_class.map(|c| c.as_str()).unwrap_or("*")
    }

    fn validate(&self) -> RelayResult<()> {
        if self.application_id.is_some() && self.tenant_id.is_none() {
            return Err(RelayError::Validation(
                "A hold on an application names its tenant.",
            ));
        }
        if self.environment_id.is_some() && self.application_id.is_none() {
            return Err(RelayError::Validation(
                "A hold on an environment names its application.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct HoldRow {
    pub id: Uuid,
    pub data_class: String,
    pub resource_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub reason_code: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

fn operator_ok(created_by: &str) -> bool {
    let mut bytes = created_by.bytes();
    created_by.len() <= 64
        && bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Places a hold. Returns its id.
pub async fn place<C: ConnectionTrait + TransactionTrait>(
    conn: &C,
    scope: &HoldScope,
    reason: HoldReason,
    created_by: &str,
    now: DateTime<Utc>,
) -> RelayResult<Uuid> {
    scope.validate()?;
    if !operator_ok(created_by) {
        return Err(RelayError::Validation(
            "created-by must be a bounded operator reference.",
        ));
    }
    let id = Uuid::new_v4();
    let txn = conn.begin().await?;
    db::exec(
        &txn,
        "INSERT INTO privacy_relay_retention_holds (id, data_class, resource_id, tenant_id, application_id, \
           environment_id, reason_code, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![
            id.into(),
            scope.class_str().into(),
            scope.resource_id.into(),
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            reason.as_str().into(),
            created_by.into(),
            now.into(),
        ],
    )
    .await?;
    audit::record(
        &txn,
        &RetentionAuditEntry {
            data_class: scope.class_str().to_string(),
            action: RetentionAuditAction::Hold,
            resource_ref: scope.resource_id.or(Some(id)),
            batch_count: 1,
            policy: reason.as_str().to_string(),
            actor: RetentionActor::Operator,
            succeeded: true,
        },
        now,
    )
    .await?;
    txn.commit().await?;
    Ok(id)
}

/// Releases a hold (idempotent). Returns whether it was active.
pub async fn release<C: ConnectionTrait + TransactionTrait>(
    conn: &C,
    id: Uuid,
    now: DateTime<Utc>,
) -> RelayResult<bool> {
    let txn = conn.begin().await?;
    let row: Option<HoldRow> = db::one(
        &txn,
        "SELECT id, data_class, resource_id, tenant_id, application_id, environment_id, reason_code, created_by, \
           created_at, released_at FROM privacy_relay_retention_holds WHERE id = $1 FOR UPDATE",
        vec![id.into()],
    )
    .await?;
    let Some(row) = row else {
        return Err(RelayError::Validation("The hold does not exist."));
    };
    if row.released_at.is_some() {
        return Ok(false);
    }
    db::exec(
        &txn,
        "UPDATE privacy_relay_retention_holds SET released_at = $2 WHERE id = $1",
        vec![id.into(), now.into()],
    )
    .await?;
    audit::record(
        &txn,
        &RetentionAuditEntry {
            data_class: row.data_class,
            action: RetentionAuditAction::Release,
            resource_ref: row.resource_id.or(Some(id)),
            batch_count: 1,
            policy: row.reason_code,
            actor: RetentionActor::Operator,
            succeeded: true,
        },
        now,
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Every hold (active first), newest first within each group.
pub async fn list<C: ConnectionTrait>(conn: &C, active_only: bool) -> RelayResult<Vec<HoldRow>> {
    db::all(
        conn,
        &format!(
            "SELECT id, data_class, resource_id, tenant_id, application_id, environment_id, reason_code, created_by, \
               created_at, released_at FROM privacy_relay_retention_holds{} \
             ORDER BY (released_at IS NULL) DESC, created_at DESC",
            if active_only {
                " WHERE released_at IS NULL"
            } else {
                ""
            }
        ),
        vec![],
    )
    .await
}

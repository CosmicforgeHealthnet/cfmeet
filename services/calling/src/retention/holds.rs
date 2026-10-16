//! Retention holds (§80): an operator freezes every retention action for one resource,
//! one scope's data class (or every class, `*`), or — platform-wide, `*` only — for
//! everything, while a policy review, a dispute or a security investigation needs the
//! data. Workers (and the recording / voicemail retention passes) skip whatever an
//! active hold covers; a hold ends by being RELEASED (never deleted).
//!
//! A scope hold names tenant, application AND environment: a hold in one scope never
//! protects another (the database's `calling_retention_held` requires all three ids to
//! match). Placing and releasing a hold are operator-audited (`RETENTION_HOLD_PLACED` /
//! `RETENTION_HOLD_RELEASED`) in the same transaction as the change.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::DataClass;
use crate::platform::{
    operator::{self, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    PlatformError, PlatformResult,
};

text_enum!(
    /// Why a hold exists (neutral, bounded vocabulary; the database checks it too).
    HoldReason {
        PolicyReview = "POLICY_REVIEW",
        Dispute = "DISPUTE",
        SecurityInvestigation = "SECURITY_INVESTIGATION",
        Other = "OTHER",
    }
);

/// The owning scope of a hold (all three ids, never a partial scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldOwner {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

/// What a hold covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldScope {
    /// One data class, or every class (`None` = `*`).
    pub data_class: Option<DataClass>,
    /// One resource (a Call, a delivery, an incident, …) or every resource of the scope.
    pub resource_id: Option<Uuid>,
    /// The scope; `None` only for a platform-wide hold over every class.
    pub owner: Option<HoldOwner>,
}

impl HoldScope {
    fn class_str(&self) -> &'static str {
        self.data_class.map(|c| c.as_str()).unwrap_or("*")
    }

    fn validate(&self) -> PlatformResult<()> {
        if self.owner.is_none() && (self.data_class.is_some() || self.resource_id.is_some()) {
            return Err(PlatformError::Validation(
                "a hold without a tenant / application / environment scope is platform-wide and \
                 covers every class (`*`) and every resource"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

/// A hold as stored (opaque ids and bounded codes only).
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldView {
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

const HOLD_COLUMNS: &str =
    "id, data_class, resource_id, tenant_id, application_id, environment_id, \
    reason_code, created_by, created_at, released_at";

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn entry(action: OperatorAction, hold: &HoldView, detail: serde_json::Value) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::RetentionHold, hold.id)
        .scope(hold.tenant_id, hold.application_id, hold.environment_id)
        .detail(detail)
}

/// Places a hold (operator-audited, one transaction). The scope's environment must exist.
pub async fn operator_place(
    db: &DatabaseConnection,
    op: &OperatorContext,
    scope: HoldScope,
    reason: HoldReason,
    now: DateTime<Utc>,
) -> PlatformResult<HoldView> {
    scope.validate()?;
    let created_by = op.operator().to_string();
    operator::audited(db, op, now, move |txn| {
        Box::pin(async move {
            if let Some(owner) = scope.owner {
                txn.query_one(stmt(
                    "SELECT 1 FROM platform_environments WHERE tenant_id = $1 AND application_id = $2 AND id = $3",
                    vec![
                        owner.tenant_id.into(),
                        owner.application_id.into(),
                        owner.environment_id.into(),
                    ],
                ))
                .await?
                .ok_or(PlatformError::NotFound("environment"))?;
            }
            let hold = HoldView::find_by_statement(stmt(
                &format!(
                    "INSERT INTO calling_retention_holds (id, data_class, resource_id, tenant_id, application_id, \
                       environment_id, reason_code, created_by, created_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {HOLD_COLUMNS}"
                ),
                vec![
                    Uuid::new_v4().into(),
                    scope.class_str().into(),
                    scope.resource_id.into(),
                    scope.owner.map(|o| o.tenant_id).into(),
                    scope.owner.map(|o| o.application_id).into(),
                    scope.owner.map(|o| o.environment_id).into(),
                    reason.as_str().into(),
                    created_by.into(),
                    now.into(),
                ],
            ))
            .one(txn)
            .await?
            .ok_or(PlatformError::NotFound("retention hold"))?;
            let audit = entry(
                OperatorAction::RetentionHoldPlaced,
                &hold,
                json!({
                    "dataClass": hold.data_class,
                    "reasonCode": hold.reason_code,
                    "resourceId": hold.resource_id,
                    "platformWide": scope.owner.is_none(),
                }),
            );
            Ok((hold, audit))
        })
    })
    .await
}

/// Releases a hold (operator-audited; idempotent — an already released hold is not
/// touched, and the audit row says so).
pub async fn operator_release(
    db: &DatabaseConnection,
    op: &OperatorContext,
    id: Uuid,
    now: DateTime<Utc>,
) -> PlatformResult<HoldView> {
    operator::audited(db, op, now, move |txn| {
        Box::pin(async move {
            let current = HoldView::find_by_statement(stmt(
                &format!(
                    "SELECT {HOLD_COLUMNS} FROM calling_retention_holds WHERE id = $1 FOR UPDATE"
                ),
                vec![id.into()],
            ))
            .one(txn)
            .await?
            .ok_or(PlatformError::NotFound("retention hold"))?;
            let was_active = current.released_at.is_none();
            let hold = if was_active {
                HoldView::find_by_statement(stmt(
                    &format!(
                        "UPDATE calling_retention_holds SET released_at = GREATEST($2, created_at) \
                         WHERE id = $1 AND released_at IS NULL RETURNING {HOLD_COLUMNS}"
                    ),
                    vec![id.into(), now.into()],
                ))
                .one(txn)
                .await?
                .ok_or(PlatformError::NotFound("retention hold"))?
            } else {
                current
            };
            let audit = entry(
                OperatorAction::RetentionHoldReleased,
                &hold,
                json!({
                    "dataClass": hold.data_class,
                    "reasonCode": hold.reason_code,
                    "wasActive": was_active,
                }),
            );
            Ok((hold, audit))
        })
    })
    .await
}

/// Holds, active first then newest first (at most 500).
pub async fn list<C: ConnectionTrait>(
    conn: &C,
    active_only: bool,
) -> PlatformResult<Vec<HoldView>> {
    Ok(HoldView::find_by_statement(stmt(
        &format!(
            "SELECT {HOLD_COLUMNS} FROM calling_retention_holds{} \
             ORDER BY (released_at IS NULL) DESC, created_at DESC, id LIMIT 500",
            if active_only {
                " WHERE released_at IS NULL"
            } else {
                ""
            }
        ),
        vec![],
    ))
    .all(conn)
    .await?)
}

/// Whether an active hold covers `class` for `resource` in the given scope (the same rule
/// the workers apply in SQL).
pub async fn is_held<C: ConnectionTrait>(
    conn: &C,
    class: DataClass,
    resource: Option<Uuid>,
    owner: Option<HoldOwner>,
) -> PlatformResult<bool> {
    let row = conn
        .query_one(stmt(
            "SELECT calling_retention_held($1, $2, $3, $4, $5)",
            vec![
                class.as_str().into(),
                resource.into(),
                owner.map(|o| o.tenant_id).into(),
                owner.map(|o| o.application_id).into(),
                owner.map(|o| o.environment_id).into(),
            ],
        ))
        .await?
        .ok_or(PlatformError::NotFound("retention hold"))?;
    Ok(row.try_get_by_index::<bool>(0)?)
}

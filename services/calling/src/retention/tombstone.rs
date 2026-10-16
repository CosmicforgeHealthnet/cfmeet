//! Principal tombstone (§80.42): the minimization of a communication principal that must
//! no longer be identifiable, without breaking any evidence.
//!
//! The principal itself belongs to Platform Access (ADR 0032): Calling checks its own
//! retention holds, asks Platform Access to tombstone the principal (named operator,
//! audited there too), then — in ONE operator-audited Calling transaction
//! (`PRINCIPAL_TOMBSTONED`, plus a `PRINCIPAL / MINIMIZE` retention audit row) — applies
//! the tombstoned snapshot to its projection and minimizes its own participations:
//!
//! * the principal becomes DEACTIVATED with `tombstoned_at` set (the database then refuses
//!   any other status, so it can never be reactivated or resolve again);
//! * its `external_reference` (the only identifying value Calling holds for it) is
//!   replaced by a non-identifying value derived from its own id — `tombstoned:<id>`, or
//!   the id itself for a TELEPHONE_ENDPOINT (whose references are UUIDs) — the one change
//!   the immutable-reference guard allows;
//! * the display names of its participations are erased (`display_minimized_at` set, so
//!   none can reappear).
//!
//! Nothing is deleted: Calls, participants, events, usage and spend keep referencing the
//! same opaque principal id, so every foreign key stays intact and call history still
//! lists the Calls (with that opaque id). A retention hold on the principal refuses the
//! tombstone. SERVICE_ACCOUNT principals follow their service account's lifecycle and are
//! refused. Re-running it is idempotent.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    audit::{self, RetentionAuditEntry},
    holds::{self, HoldOwner},
    DataClass, RetentionAction, RetentionOutcome,
};
use crate::platform::{
    access::IdentityAuthority,
    operator::{self, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    projection::upsert_principal,
    PlatformError, PlatformResult,
};

/// Reserved prefix of tombstone references (refused as a registration reference).
pub use crate::platform::principals::TOMBSTONE_REFERENCE_PREFIX as TOMBSTONE_PREFIX;

/// The policy label of a tombstone in the retention audit.
pub const TOMBSTONE_POLICY: &str = "OPERATOR_TOMBSTONE";

/// The result of a tombstone (opaque ids and counts only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TombstoneView {
    pub principal_id: Uuid,
    pub status: &'static str,
    pub tombstoned_at: DateTime<Utc>,
    pub participations_minimized: u64,
    pub already_tombstoned: bool,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Tombstones `principal_id` (see the module documentation).
pub async fn operator_tombstone(
    db: &DatabaseConnection,
    authority: &dyn IdentityAuthority,
    op: &OperatorContext,
    principal_id: Uuid,
    now: DateTime<Utc>,
) -> PlatformResult<TombstoneView> {
    let row = db
        .query_one(stmt(
            "SELECT tenant_id, application_id, environment_id, kind, tombstoned_at \
             FROM platform_principals WHERE id = $1",
            vec![principal_id.into()],
        ))
        .await?
        .ok_or(PlatformError::NotFound("principal"))?;
    let owner = HoldOwner {
        tenant_id: row.try_get_by_index(0)?,
        application_id: row.try_get_by_index(1)?,
        environment_id: row.try_get_by_index(2)?,
    };
    let kind: String = row.try_get_by_index(3)?;
    let tombstoned_at: Option<DateTime<Utc>> = row.try_get_by_index(4)?;
    let is_service_account = kind == "SERVICE_ACCOUNT";
    let audit_entry = move |n: u64, already: bool| {
        AuditEntry::new(
            OperatorAction::PrincipalTombstoned,
            AuditTargetKind::Principal,
            principal_id,
        )
        .scope(
            Some(owner.tenant_id),
            Some(owner.application_id),
            Some(owner.environment_id),
        )
        .detail(json!({
            "kind": kind.clone(),
            "participationsMinimized": n,
            "alreadyTombstoned": already,
        }))
    };
    if let Some(at) = tombstoned_at {
        let view = TombstoneView {
            principal_id,
            status: "DEACTIVATED",
            tombstoned_at: at,
            participations_minimized: 0,
            already_tombstoned: true,
        };
        let entry = audit_entry(0, true);
        return operator::audited(db, op, now, move |_| {
            Box::pin(async move { Ok((view, entry)) })
        })
        .await;
    }
    if is_service_account {
        return Err(PlatformError::PolicyViolation(
            "SERVICE_ACCOUNT principals follow their service account's lifecycle and are not tombstoned"
                .to_string(),
        ));
    }
    if holds::is_held(db, DataClass::Principal, Some(principal_id), Some(owner)).await? {
        return Err(PlatformError::InvalidState(
            "a retention hold covers this principal; release it before tombstoning".to_string(),
        ));
    }
    // The identity authority tombstones the principal (idempotent there).
    let outcome = authority
        .tombstone_principal(principal_id, op.operator())
        .await
        .map_err(|e| PlatformError::InvalidState(format!("platform access could not tombstone the principal: {e}")))?;
    let snapshot = outcome.principal;
    let at = snapshot.tombstoned_at.unwrap_or(now);
    let actor = op.operator().to_string();
    let view = operator::audited(db, op, now, move |txn| {
        Box::pin(async move {
            let minimized = upsert_principal(txn, &snapshot, now)
                .await
                .map_err(|e| PlatformError::Database {
                    summary: e.to_string(),
                    connectivity: false,
                })?;
            audit::record(
                txn,
                &RetentionAuditEntry {
                    class: DataClass::Principal,
                    action: RetentionAction::Minimize,
                    resource_ref: Some(principal_id),
                    batch_count: 1,
                    policy: TOMBSTONE_POLICY.to_string(),
                    actor,
                    outcome: RetentionOutcome::Minimized,
                },
                now,
            )
            .await?;
            let view = TombstoneView {
                principal_id,
                status: "DEACTIVATED",
                tombstoned_at: at,
                participations_minimized: minimized,
                already_tombstoned: false,
            };
            let entry = audit_entry(minimized, false);
            Ok((view, entry))
        })
    })
    .await?;
    audit::count(
        DataClass::Principal,
        RetentionAction::Minimize,
        RetentionOutcome::Minimized,
        1,
    );
    tracing::info!(
        principal_id = %view.principal_id,
        participations_minimized = view.participations_minimized,
        "principal tombstoned"
    );
    Ok(view)
}

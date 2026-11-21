//! ADR 0068, phase A3 — decisions relayed by the HQ admin bridge.
//!
//! The admin bridge (`cfmeet.admin-bridge`, verified `hq:<staff>` attribution) replaces the
//! named-reviewer lists for the decisions HQ governs:
//!
//! * every command carries HQ's `Idempotency-Key` and the case's `expectedVersion`; the
//!   change happens only at that version (else `VERSION_CONFLICT`, nothing done);
//! * two-person decisions (approve / reject / suspend / publish) carry the approver
//!   (`X-CF-Approver`, a different person, verified by the shared attribution parser);
//! * no self-review: when the staff member's CF Meet account is linked
//!   (`x-cf-operator-user-id`) and belongs to the partner's workspace, the decision is
//!   refused; without a link the decision is allowed and recorded `UNLINKED` (agreed with
//!   HQ: two people decide every R4 action);
//! * the applied command is recorded in `devplat_admin_operations` in the SAME transaction
//!   as the change, so the bridge can settle an unknown outcome from it, and a repeated key
//!   never applies twice; the bridge's answer is the case in HQ's shape
//!   ([`super::verification`]), and the record reports the same state.

use axum::http::HeaderMap;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{json, Value};
use uuid::Uuid;

use super::stmt;
use crate::http::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
};

/// What the record's subject is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    ProductionAccess,
    App,
}

impl Subject {
    pub const fn as_str(self) -> &'static str {
        match self {
            Subject::ProductionAccess => "PRODUCTION_ACCESS",
            Subject::App => "APP",
        }
    }
    const fn table(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Subject::ProductionAccess => ("devplat_production_access_requests", "id", "state"),
            Subject::App => ("devplat_apps", "app_id", "status"),
        }
    }
}

/// One verified bridge command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCommand {
    pub key: Uuid,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)` for two-person decisions.
    pub approval: Option<(String, String)>,
    pub expected_version: i64,
    /// The staff member's linked CF Meet account, if any.
    pub linked_user: Option<Uuid>,
}

impl BridgeCommand {
    /// `Ok(None)` for the operator console; for the bridge, the command (the key and the
    /// expected version are required, and the approval when `two_person`).
    pub fn of(
        caller: &InternalCaller,
        headers: &HeaderMap,
        expected_version: Option<i64>,
        two_person: bool,
    ) -> Result<Option<Self>, ApiError> {
        let Some(attribution) = caller.bridge_operator() else {
            return Ok(None);
        };
        let key = headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v.trim()).ok())
            .ok_or(ApiError(ErrorCode::ValidationError))?;
        let expected_version = expected_version
            .filter(|v| *v >= 1)
            .ok_or(ApiError(ErrorCode::ValidationError))?;
        let approval = attribution
            .approval
            .as_ref()
            .map(|a| (a.approver.as_str().to_string(), a.approval_ref.clone()));
        if two_person && approval.is_none() {
            // R4: the bridge never sends a decision without its second person.
            return Err(ApiError(ErrorCode::PolicyViolation));
        }
        let linked_user = headers
            .get("x-cf-operator-user-id")
            .and_then(|v| v.to_str().ok())
            .map(|v| Uuid::parse_str(v.trim()).map_err(|_| ApiError(ErrorCode::OperatorRequired)))
            .transpose()?;
        Ok(Some(BridgeCommand {
            key,
            operator: attribution.operator.as_str().to_string(),
            approval,
            expected_version,
            linked_user,
        }))
    }

    pub fn staff_link(&self) -> &'static str {
        if self.linked_user.is_some() {
            "LINKED"
        } else {
            "UNLINKED"
        }
    }
}

/// A key already applied: the same command on the same subject is answered with the
/// current resource (never applied twice); anything else is a conflict.
pub async fn replayed<C: ConnectionTrait>(
    db: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Uuid,
) -> Result<bool, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT command, subject_kind, subject_id FROM devplat_admin_operations WHERE idempotency_key = $1",
            vec![cmd.key.into()],
        ))
        .await?;
    let Some(row) = row else { return Ok(false) };
    let (c, k, id): (String, String, Uuid) = (
        row.try_get("", "command")?,
        row.try_get("", "subject_kind")?,
        row.try_get("", "subject_id")?,
    );
    if c == command && k == subject.as_str() && id == subject_id {
        Ok(true)
    } else {
        Err(ApiError(ErrorCode::VersionConflict))
    }
}

/// Locks the subject and checks the expected version (in the decision's transaction).
pub async fn lock_version<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    subject: Subject,
    subject_id: Uuid,
) -> Result<(), ApiError> {
    let (table, id_col, _) = subject.table();
    let row = txn
        .query_one(stmt(
            &format!("SELECT version FROM {table} WHERE {id_col} = $1 FOR UPDATE"),
            vec![subject_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let version: i64 = row.try_get("", "version")?;
    if version != cmd.expected_version {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
    Ok(())
}

/// Records the applied command (same transaction as the change).
pub async fn record<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Uuid,
    tenant_id: Uuid,
) -> Result<(), ApiError> {
    let (table, id_col, state_col) = subject.table();
    let row = txn
        .query_one(stmt(
            &format!("SELECT {state_col} AS state, version FROM {table} WHERE {id_col} = $1"),
            vec![subject_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    let (state, version): (String, i64) = (row.try_get("", "state")?, row.try_get("", "version")?);
    let (approver, approval_ref) = match &cmd.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    txn.execute(stmt(
        "INSERT INTO devplat_admin_operations (idempotency_key, command, subject_kind, subject_id, tenant_id, \
           operator_ref, approver_ref, approval_ref, staff_link, resulting_state, resulting_version, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            cmd.key.into(),
            command.into(),
            subject.as_str().into(),
            subject_id.into(),
            tenant_id.into(),
            cmd.operator.clone().into(),
            approver.into(),
            approval_ref.into(),
            cmd.staff_link().into(),
            state.into(),
            version.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// `GET /internal/v1/operator/operations/{key}`: the bridge's settlement read.
pub async fn operation_record(db: &DatabaseConnection, key: Uuid) -> Result<Value, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT subject_kind, resulting_state, resulting_version FROM devplat_admin_operations              WHERE idempotency_key = $1",
            vec![key.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let (subject, state, version): (String, String, i64) = (
        row.try_get("", "subject_kind")?,
        row.try_get("", "resulting_state")?,
        row.try_get("", "resulting_version")?,
    );
    // The case state as HQ sees it, as in the command's own answer.
    let kind = if subject == Subject::ProductionAccess.as_str() {
        super::verification::PARTNER_GO_LIVE
    } else {
        super::verification::APP_REVIEW
    };
    let hq =
        super::verification::hq_state(kind, &state).ok_or(ApiError(ErrorCode::InternalError))?;
    Ok(json!({
        "idempotency_key": key,
        "outcome": "APPLIED",
        "source_status": hq,
        "source_version": version.to_string(),
    }))
}

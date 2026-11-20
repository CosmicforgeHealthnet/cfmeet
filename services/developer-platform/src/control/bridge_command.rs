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
//!
//! A4 (partners, apps) and A6 (pricing) commands run through the same core: they answer
//! the standard owner shape `{"status", "version", "resource"}` ([`answer`]) and their
//! record reports the RAW resulting state (an upper-snake code), not a verification
//! state. Unversioned subjects (a webhook delivery retry, a usage reprocess) carry no
//! `expectedVersion`; their record stores version 0 and reports `null`.

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
    /// A4: a partner workspace (`devplat_organizations`).
    Organization,
    /// A4: a workspace's support profile (absent row: version 0).
    SupportProfile,
    /// A4: a push credential's metadata (`devplat_push_providers`).
    PushKey,
    /// A4: a webhook delivery (unversioned; the retry is a NEW delivery).
    WebhookDelivery,
    /// A4: the usage rollups (unversioned; the subject id is the environment or nil).
    UsageRollups,
}

impl Subject {
    pub const fn as_str(self) -> &'static str {
        match self {
            Subject::ProductionAccess => "PRODUCTION_ACCESS",
            Subject::App => "APP",
            Subject::Organization => "ORGANIZATION",
            Subject::SupportProfile => "SUPPORT_PROFILE",
            Subject::PushKey => "PUSH_KEY",
            Subject::WebhookDelivery => "WEBHOOK_DELIVERY",
            Subject::UsageRollups => "USAGE_ROLLUPS",
        }
    }

    /// `(table, id column, state column)` of a versioned subject.
    const fn table(self) -> Option<(&'static str, &'static str, &'static str)> {
        Some(match self {
            Subject::ProductionAccess => ("devplat_production_access_requests", "id", "state"),
            Subject::App => ("devplat_apps", "app_id", "status"),
            Subject::Organization => ("devplat_organizations", "tenant_id", "state"),
            Subject::SupportProfile => ("devplat_support_profiles", "tenant_id", "support_tier"),
            Subject::PushKey => ("devplat_push_providers", "id", "state"),
            Subject::WebhookDelivery | Subject::UsageRollups => return None,
        })
    }

    /// Whether the subject has a row version (an `expectedVersion`).
    pub const fn versioned(self) -> bool {
        self.table().is_some()
    }

    fn parse(s: &str) -> Option<Self> {
        [
            Subject::ProductionAccess,
            Subject::App,
            Subject::Organization,
            Subject::SupportProfile,
            Subject::PushKey,
            Subject::WebhookDelivery,
            Subject::UsageRollups,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// The commands answered with HQ's verification case (A3); every other command answers
/// the standard shape and records the raw state.
fn verification_kind(subject: Subject, command: &str) -> Option<&'static str> {
    match subject {
        Subject::ProductionAccess => Some(super::verification::PARTNER_GO_LIVE),
        Subject::App if !matches!(command, "app.suspend" | "app.lift-suspension") => {
            Some(super::verification::APP_REVIEW)
        }
        _ => None,
    }
}

/// The standard answer of an A4+ bridge command: `status` (the resulting state, upper
/// snake), `version` (the resulting row version as a string; `null` when unversioned)
/// and a small closed `resource` (ids, states, timestamps).
pub fn answer(state: &str, version: Option<i64>, resource: Value) -> Value {
    json!({
        "status": state,
        "version": version.map(|v| v.to_string()),
        "resource": resource,
    })
}

/// What the bridge expects of the subject's version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// `expectedVersion` ≥ 1 (the row exists).
    Version,
    /// `expectedVersion` ≥ 0: 0 means "no row yet" (e.g. a support profile never set).
    VersionOrAbsent,
    /// The subject is unversioned: no `expectedVersion` is accepted.
    Nothing,
}

/// One verified bridge command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCommand {
    pub key: Uuid,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)` for two-person decisions.
    pub approval: Option<(String, String)>,
    /// `None` for an unversioned subject.
    pub expected_version: Option<i64>,
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
        Self::of_with(
            caller,
            headers,
            expected_version,
            two_person,
            Expect::Version,
        )
    }

    /// [`BridgeCommand::of`] with an explicit version rule.
    pub fn of_with(
        caller: &InternalCaller,
        headers: &HeaderMap,
        expected_version: Option<i64>,
        two_person: bool,
        expect: Expect,
    ) -> Result<Option<Self>, ApiError> {
        let Some(attribution) = caller.bridge_operator() else {
            return Ok(None);
        };
        let key = headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v.trim()).ok())
            .ok_or(ApiError(ErrorCode::ValidationError))?;
        let expected_version = match expect {
            Expect::Version => Some(
                expected_version
                    .filter(|v| *v >= 1)
                    .ok_or(ApiError(ErrorCode::ValidationError))?,
            ),
            Expect::VersionOrAbsent => Some(
                expected_version
                    .filter(|v| *v >= 0)
                    .ok_or(ApiError(ErrorCode::ValidationError))?,
            ),
            Expect::Nothing if expected_version.is_some() => {
                return Err(ApiError(ErrorCode::ValidationError))
            }
            Expect::Nothing => None,
        };
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

    /// The bridge command of a route served to the admin bridge ONLY: the operator
    /// console and every other caller are refused (INTERNAL_PERMISSION_DENIED).
    pub fn required(
        caller: &InternalCaller,
        headers: &HeaderMap,
        expected_version: Option<i64>,
        two_person: bool,
        expect: Expect,
    ) -> Result<Self, ApiError> {
        Self::of_with(caller, headers, expected_version, two_person, expect)?
            .ok_or(ApiError(ErrorCode::InternalPermissionDenied))
    }

    /// The expected version (versioned subjects only).
    pub fn version(&self) -> Result<i64, ApiError> {
        self.expected_version
            .ok_or(ApiError(ErrorCode::ValidationError))
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
    Ok(prior(db, cmd, command, subject, subject_id)
        .await?
        .is_some())
}

/// The recorded outcome `(state, version)` of a key already applied to the same command
/// and subject (`None`: never applied; another command or subject: VERSION_CONFLICT).
pub async fn prior<C: ConnectionTrait>(
    db: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Uuid,
) -> Result<Option<(String, Option<i64>)>, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT command, subject_kind, subject_id, resulting_state, resulting_version \
             FROM devplat_admin_operations WHERE idempotency_key = $1",
            vec![cmd.key.into()],
        ))
        .await?;
    let Some(row) = row else { return Ok(None) };
    let (c, k, id): (String, String, Uuid) = (
        row.try_get("", "command")?,
        row.try_get("", "subject_kind")?,
        row.try_get("", "subject_id")?,
    );
    if c == command && k == subject.as_str() && id == subject_id {
        let version: i64 = row.try_get("", "resulting_version")?;
        Ok(Some((
            row.try_get("", "resulting_state")?,
            subject.versioned().then_some(version),
        )))
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
    if !lock_version_or_absent(txn, cmd, subject, subject_id).await? {
        return Err(ApiError(ErrorCode::NotFound));
    }
    Ok(())
}

/// Locks the subject's row (`FOR UPDATE`) and checks the expected version. `Ok(false)`:
/// no row and the bridge expected version 0 ([`Expect::VersionOrAbsent`]); a missing row
/// at any other expected version is NOT_FOUND, a row at another version
/// VERSION_CONFLICT.
pub async fn lock_version_or_absent<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    subject: Subject,
    subject_id: Uuid,
) -> Result<bool, ApiError> {
    let (table, id_col, _) = subject.table().ok_or(ApiError(ErrorCode::InternalError))?;
    let expected = cmd.version()?;
    let row = txn
        .query_one(stmt(
            &format!("SELECT version FROM {table} WHERE {id_col} = $1 FOR UPDATE"),
            vec![subject_id.into()],
        ))
        .await?;
    let Some(row) = row else {
        return if expected == 0 {
            Ok(false)
        } else {
            Err(ApiError(ErrorCode::NotFound))
        };
    };
    let version: i64 = row.try_get("", "version")?;
    if version != expected {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
    Ok(true)
}

/// A non-locking version check, for commands that must call another service BEFORE
/// their transaction (the check is repeated under the row lock).
pub async fn check_version<C: ConnectionTrait>(
    db: &C,
    cmd: &BridgeCommand,
    subject: Subject,
    subject_id: Uuid,
) -> Result<(), ApiError> {
    let (table, id_col, _) = subject.table().ok_or(ApiError(ErrorCode::InternalError))?;
    let row = db
        .query_one(stmt(
            &format!("SELECT version FROM {table} WHERE {id_col} = $1"),
            vec![subject_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    if row.try_get::<i64>("", "version")? != cmd.version()? {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
    Ok(())
}

/// Records the applied command (same transaction as the change), reading the resulting
/// state and version from the subject's row.
pub async fn record<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Uuid,
    tenant_id: Uuid,
) -> Result<(), ApiError> {
    let (table, id_col, state_col) = subject.table().ok_or(ApiError(ErrorCode::InternalError))?;
    let row = txn
        .query_one(stmt(
            &format!("SELECT {state_col} AS state, version FROM {table} WHERE {id_col} = $1"),
            vec![subject_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    let (state, version): (String, i64) = (row.try_get("", "state")?, row.try_get("", "version")?);
    record_result(
        txn,
        cmd,
        command,
        subject,
        subject_id,
        tenant_id,
        &state,
        Some(version),
    )
    .await
}

/// Records the applied command with an explicit resulting state (upper snake) and
/// version (`None`: unversioned, stored as 0).
#[allow(clippy::too_many_arguments)]
pub async fn record_result<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Uuid,
    tenant_id: Uuid,
    state: &str,
    version: Option<i64>,
) -> Result<(), ApiError> {
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
            version.unwrap_or(0).into(),
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
            "SELECT command, subject_kind, resulting_state, resulting_version \
             FROM devplat_admin_operations WHERE idempotency_key = $1",
            vec![key.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let (command, subject, state, version): (String, String, String, i64) = (
        row.try_get("", "command")?,
        row.try_get("", "subject_kind")?,
        row.try_get("", "resulting_state")?,
        row.try_get("", "resulting_version")?,
    );
    let subject = Subject::parse(&subject).ok_or(ApiError(ErrorCode::InternalError))?;
    // As in the command's own answer: the case state as HQ sees it for the A3 verification
    // decisions, the raw resulting state for every other command.
    let status = match verification_kind(subject, &command) {
        Some(kind) => super::verification::hq_state(kind, &state)
            .ok_or(ApiError(ErrorCode::InternalError))?
            .to_string(),
        None => state,
    };
    Ok(json!({
        "idempotency_key": key,
        "outcome": "APPLIED",
        "source_status": status,
        "source_version": subject.versioned().then(|| version.to_string()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_round_trip_and_only_a3_decisions_map_to_case_states() {
        for k in [
            Subject::ProductionAccess,
            Subject::App,
            Subject::Organization,
            Subject::SupportProfile,
            Subject::PushKey,
            Subject::WebhookDelivery,
            Subject::UsageRollups,
        ] {
            assert_eq!(Subject::parse(k.as_str()), Some(k));
            assert!(k.as_str().len() <= 24);
        }
        assert!(verification_kind(Subject::App, "app.approve").is_some());
        assert!(verification_kind(Subject::App, "app.suspend").is_none());
        assert!(verification_kind(Subject::App, "app.lift-suspension").is_none());
        assert!(
            verification_kind(Subject::ProductionAccess, "production-access.suspend").is_some()
        );
        assert!(verification_kind(Subject::Organization, "partners.workspace.state").is_none());
        assert!(!Subject::WebhookDelivery.versioned() && Subject::PushKey.versioned());
    }

    #[test]
    fn the_standard_answer_carries_the_version_as_a_string() {
        let a = answer("SUSPENDED", Some(7), json!({ "id": 1 }));
        assert_eq!(a["status"], "SUSPENDED");
        assert_eq!(a["version"], "7");
        assert!(answer("RETRIED", None, json!({}))["version"].is_null());
    }
}

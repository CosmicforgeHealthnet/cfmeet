//! ADR 0068, phase A4 — commands relayed by the HQ admin bridge.
//!
//! The admin bridge (`cfmeet.admin-bridge`, verified `hq:<staff>` attribution, see
//! [`super::middleware`]) runs operator commands with `organization.administer`. For the
//! bridge, and only for it, every command:
//!
//! * carries HQ's `Idempotency-Key` (a UUID; 400 `VALIDATION_ERROR` without it);
//! * carries `expectedVersion` where the subject is versioned: the change happens only at
//!   that version, checked under the subject's row lock (409 `VERSION_CONFLICT`, nothing
//!   done);
//! * of risk R3 carries the second person (`X-CF-Approver`, `X-CF-Approval-Ref`,
//!   `X-CF-Intent-Digest`, a different person; 403 `APPROVAL_REQUIRED` without them);
//! * is recorded in `organization_admin_operations` in the SAME transaction as the change,
//!   so the bridge settles an unknown outcome from that record
//!   (`GET /internal/v1/operator/operations/{key}`) and a repeated key never applies twice:
//!   the same command on the same subject answers the current state, anything else is 409
//!   `IDEMPOTENCY_CONFLICT`;
//! * answers `{"status": "<STATE>", "version": "<n>" | null, "resource": {…}}` — ids,
//!   states and timestamps only (never a name, email or credential value).
//!
//! Every other caller (the operator console, the Meeting Service platform-admin facade)
//! keeps the route's existing behaviour and answer.

use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{middleware::Caller, respond};
use crate::{
    db,
    error::{ApiError, ErrorCode},
};

/// HQ's command key (also the settlement key).
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// The capability's risk: R3 needs HQ's second person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    R2,
    R3,
}

/// What a recorded command changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Organization,
    Membership,
    Team,
    Policy,
    Provisioning,
    CallingBinding,
    MessagingBinding,
    EventDelivery,
}

impl Subject {
    pub const fn as_str(self) -> &'static str {
        match self {
            Subject::Organization => "ORGANIZATION",
            Subject::Membership => "MEMBERSHIP",
            Subject::Team => "TEAM",
            Subject::Policy => "POLICY",
            Subject::Provisioning => "PROVISIONING",
            Subject::CallingBinding => "CALLING_BINDING",
            Subject::MessagingBinding => "MESSAGING_BINDING",
            Subject::EventDelivery => "EVENT_DELIVERY",
        }
    }
}

/// Command names (the record's `command`).
pub mod command {
    pub const CREATE: &str = "organization.create";
    pub const SUSPEND: &str = "organization.suspend";
    pub const REACTIVATE: &str = "organization.reactivate";
    pub const ARCHIVE: &str = "organization.archive";
    pub const OWNERSHIP_RECOVER: &str = "ownership.recover";
    pub const BINDING_CHANGE: &str = "binding.change";
    pub const PROVISIONING_RETRY: &str = "provisioning.retry";
    pub const MEMBER_CHANGE: &str = "member.change";
    pub const TEAM_CHANGE: &str = "team.change";
    pub const POLICIES_CHANGE: &str = "policies.change";
    pub const EVENTS_REPLAY: &str = "events.replay";
}

/// One verified bridge command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCommand {
    pub key: Uuid,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)` of an R3 command.
    pub approval: Option<(String, String)>,
}

impl BridgeCommand {
    /// `Ok(None)` for every caller but the admin bridge; for the bridge, the command (the
    /// key is required, and the approval for R3).
    pub fn of(caller: &Caller, headers: &HeaderMap, risk: Risk) -> Result<Option<Self>, ApiError> {
        let Some(attribution) = caller.operator.as_ref() else {
            return Ok(None);
        };
        let key = headers
            .get(IDEMPOTENCY_KEY)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::try_parse(v.trim()).ok())
            .ok_or_else(|| {
                ApiError::validation(
                    "Idempotency-Key (a UUID) is required for admin bridge commands",
                )
            })?;
        let approval = attribution
            .approval
            .as_ref()
            .map(|a| (a.approver.as_str().to_string(), a.approval_ref.clone()));
        if risk == Risk::R3 && approval.is_none() {
            return Err(ApiError::new(ErrorCode::ApprovalRequired));
        }
        Ok(Some(Self {
            key,
            operator: attribution.operator.as_str().to_string(),
            approval,
        }))
    }

    /// The command of a bridge-only route: any other caller is refused (403
    /// `ACTOR_NOT_ALLOWED`, the route needs `X-CF-Operator`).
    pub fn required(caller: &Caller, headers: &HeaderMap, risk: Risk) -> Result<Self, ApiError> {
        Self::of(caller, headers, risk)?.ok_or_else(|| {
            ApiError::with_message(
                ErrorCode::ActorNotAllowed,
                "X-CF-Operator (the admin bridge) is required for this operation",
            )
        })
    }
}

/// The bridge's `expectedVersion`: required, at least `min` (1 for an existing row; 0
/// where 0 names "none yet").
pub fn expected_version(value: Option<i64>, min: i64) -> Result<i64, ApiError> {
    match value {
        Some(v) if v >= min => Ok(v),
        _ => Err(ApiError::validation(format!(
            "expectedVersion (an integer of at least {min}) is required for admin bridge commands"
        ))),
    }
}

/// `VERSION_CONFLICT` unless the subject is at the expected version.
pub fn check_version(current: i64, expected: i64) -> Result<(), ApiError> {
    if current == expected {
        Ok(())
    } else {
        Err(ApiError::new(ErrorCode::VersionConflict))
    }
}

/// The bridge's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// UPPER_SNAKE resulting state.
    pub state: String,
    pub version: Option<i64>,
    pub resource: Value,
}

impl Outcome {
    pub fn new(state: impl Into<String>, version: Option<i64>, resource: Value) -> Self {
        Self {
            state: state.into(),
            version,
            resource,
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "status": self.state,
            "version": self.version.map(|v| v.to_string()),
            "resource": self.resource,
        })
    }

    pub fn response(&self) -> Response {
        respond(StatusCode::OK, self.json())
    }
}

#[derive(Debug, FromQueryResult)]
struct Recorded {
    command: String,
    subject_kind: String,
    subject_id: Uuid,
    resulting_state: String,
}

/// A key already applied to the same command and subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    pub subject_id: Uuid,
    /// The state the command answered when it was applied.
    pub state: String,
}

/// Serializes concurrent uses of the key (transaction-scoped advisory lock), then looks it
/// up: `Ok(Some(_))` when the key was already applied to the same command (and subject,
/// when the caller names one): answer the current state and apply nothing. `Ok(None)`: a
/// new key. A key used for anything else: 409 `IDEMPOTENCY_CONFLICT`.
pub async fn replayed<C: ConnectionTrait>(
    c: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Option<Uuid>,
) -> Result<Option<Replay>, ApiError> {
    db::exec(
        c,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("organization-admin-operation:{}", cmd.key).into()],
    )
    .await?;
    let row: Option<Recorded> = db::one(
        c,
        "SELECT command, subject_kind, subject_id, resulting_state FROM organization_admin_operations WHERE idempotency_key = $1",
        vec![cmd.key.into()],
    )
    .await?;
    let Some(row) = row else { return Ok(None) };
    let same = row.command == command
        && row.subject_kind == subject.as_str()
        && subject_id.is_none_or(|id| id == row.subject_id);
    if same {
        Ok(Some(Replay {
            subject_id: row.subject_id,
            state: row.resulting_state,
        }))
    } else {
        Err(ApiError::new(ErrorCode::IdempotencyConflict))
    }
}

/// Records the applied command (in the change's own transaction).
#[allow(clippy::too_many_arguments)]
pub async fn record<C: ConnectionTrait>(
    c: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: Uuid,
    organization: Option<Uuid>,
    outcome: &Outcome,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let (approver, approval_ref) = match &cmd.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    db::exec(
        c,
        "INSERT INTO organization_admin_operations (idempotency_key, command, subject_kind, subject_id, organization_id, \
           operator_ref, approver_ref, approval_ref, resulting_state, resulting_version, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        vec![
            cmd.key.into(),
            command.into(),
            subject.as_str().into(),
            subject_id.into(),
            organization.into(),
            cmd.operator.clone().into(),
            approver.into(),
            approval_ref.into(),
            outcome.state.clone().into(),
            outcome.version.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

#[derive(Debug, FromQueryResult)]
struct Settlement {
    resulting_state: String,
    resulting_version: Option<i64>,
}

/// `GET /internal/v1/operator/operations/{key}`: the bridge's settlement read (404 for an
/// unknown key).
pub async fn operation_record<C: ConnectionTrait>(c: &C, key: Uuid) -> Result<Value, ApiError> {
    let row: Settlement = db::one(
        c,
        "SELECT resulting_state, resulting_version FROM organization_admin_operations WHERE idempotency_key = $1",
        vec![key.into()],
    )
    .await?
    .ok_or_else(|| ApiError::with_message(ErrorCode::NotFound, "No operation is recorded for this key."))?;
    Ok(json!({
        "idempotency_key": key,
        "outcome": "APPLIED",
        "source_status": row.resulting_state,
        "source_version": row.resulting_version.map(|v| v.to_string()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_carries_the_version_as_a_string_or_null() {
        let o = Outcome::new(
            "SUSPENDED",
            Some(7),
            json!({ "organizationId": Uuid::nil() }),
        );
        assert_eq!(
            o.json(),
            json!({ "status": "SUSPENDED", "version": "7", "resource": { "organizationId": Uuid::nil() } })
        );
        let unversioned = Outcome::new("REPLAYED", None, json!({}));
        assert_eq!(unversioned.json()["version"], Value::Null);
    }

    #[test]
    fn expected_versions_are_required_and_bounded() {
        assert_eq!(expected_version(Some(3), 1).unwrap(), 3);
        assert_eq!(expected_version(Some(0), 0).unwrap(), 0);
        assert!(expected_version(Some(0), 1).is_err());
        assert!(expected_version(None, 0).is_err());
        assert!(expected_version(Some(-1), 0).is_err());
        assert!(check_version(2, 2).is_ok());
        assert_eq!(
            check_version(3, 2).unwrap_err().code,
            ErrorCode::VersionConflict
        );
    }

    #[test]
    fn subjects_fit_the_record_constraint() {
        for s in [
            Subject::Organization,
            Subject::Membership,
            Subject::Team,
            Subject::Policy,
            Subject::Provisioning,
            Subject::CallingBinding,
            Subject::MessagingBinding,
            Subject::EventDelivery,
        ] {
            let v = s.as_str();
            assert!(v.len() <= 24 && v.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'));
        }
        for c in [
            command::CREATE,
            command::SUSPEND,
            command::REACTIVATE,
            command::ARCHIVE,
            command::OWNERSHIP_RECOVER,
            command::BINDING_CHANGE,
            command::PROVISIONING_RETRY,
            command::MEMBER_CHANGE,
            command::TEAM_CHANGE,
            command::POLICIES_CHANGE,
            command::EVENTS_REPLAY,
        ] {
            assert!(
                c.len() <= 48
                    && c.bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'-')
            );
        }
    }
}

//! ADR 0068, phases A4–A7 — the operations ledger of commands relayed by the HQ admin
//! bridge (design §17.6; table `meeting_admin_operations`).
//!
//! Every bridge command carries HQ's `Idempotency-Key`. The service that applies it:
//!
//! 1. [`claim`]s the key inside its own transaction (an advisory lock on the key, then the
//!    record must not exist yet; a concurrent duplicate waits and then sees the record);
//! 2. locks the subject and checks the expected version ([`check_version`]) where the
//!    subject is versioned;
//! 3. applies the change and [`record`]s the command, its subject, the operator, the
//!    approver and the resulting state and version, in the SAME transaction.
//!
//! The bridge settles an unknown outcome from the record ([`find`]); a repeated key for the
//! same command and subject answers the current resource without applying again, and the
//! same key for anything else is a conflict. Ids, codes and references only: never an
//! address, a name or a reason text.

use chrono::Utc;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult, ErrorCode};

/// The record's subject kinds (`ck_meeting_admin_operations_subject`).
pub mod subject {
    pub const USER: &str = "USER";
    pub const ACCOUNT_DEVICE: &str = "ACCOUNT_DEVICE";
    pub const IDENTITY_UNIQUENESS: &str = "IDENTITY_UNIQUENESS";
    pub const CONFERENCE: &str = "CONFERENCE";
    pub const MEETING_SPACE: &str = "MEETING_SPACE";
    pub const ARTIFACT: &str = "ARTIFACT";
    pub const CHAT_MIGRATION: &str = "CHAT_MIGRATION";
    pub const CRITICAL_DECISION: &str = "CRITICAL_DECISION";
}

/// One verified bridge command (built by the operator router from the verified
/// `cfmeet.admin-bridge` attribution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCommand {
    /// HQ's `Idempotency-Key`.
    pub key: Uuid,
    /// The capability's command name (e.g. `accounts.user.status.change`).
    pub command: &'static str,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)` for R3 / R4 commands.
    pub approval: Option<(String, String)>,
    /// The subject's row version HQ looked at (versioned subjects only).
    pub expected_version: Option<i64>,
    /// The staff member's linked CF Meet account (`x-cf-operator-user-id`), if any.
    pub linked_user: Option<Uuid>,
}

impl BridgeCommand {
    /// `hq:<staff>` as written to audit detail.
    pub fn operator(&self) -> &str {
        &self.operator
    }

    pub fn approver(&self) -> Option<&str> {
        self.approval.as_ref().map(|(a, _)| a.as_str())
    }
}

/// A stored record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub command: String,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub resulting_state: String,
    pub resulting_version: Option<i64>,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// The record of `key`, if the command was applied.
pub async fn find<C: ConnectionTrait>(db: &C, key: Uuid) -> ApiResult<Option<Record>> {
    let row = db
        .query_one(stmt(
            "SELECT command, subject_kind, subject_id, resulting_state, resulting_version \
               FROM meeting_admin_operations WHERE idempotency_key = $1",
            vec![key.into()],
        ))
        .await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(Record {
        command: row.try_get("", "command")?,
        subject_kind: row.try_get("", "subject_kind")?,
        subject_id: row.try_get("", "subject_id")?,
        resulting_state: row.try_get("", "resulting_state")?,
        resulting_version: row.try_get("", "resulting_version")?,
    }))
}

/// Claims the key in the command's transaction: serialises concurrent uses of the same key
/// and refuses a key that is already recorded (`IDEMPOTENCY_CONFLICT`; the router then
/// answers a replay of the same command and subject from the record).
pub async fn claim<C: ConnectionTrait>(txn: &C, cmd: &BridgeCommand) -> ApiResult<()> {
    txn.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtextextended('cfmeet-admin-operation:' || $1::text, 0))",
        vec![cmd.key.into()],
    ))
    .await?;
    if find(txn, cmd.key).await?.is_some() {
        return Err(conflict_key());
    }
    Ok(())
}

pub fn conflict_key() -> ApiError {
    ApiError::domain(
        ErrorCode::IdempotencyConflict,
        "This Idempotency-Key was already used",
    )
}

/// The change happens only at the version HQ looked at (when the command carries one).
pub fn check_version(cmd: &BridgeCommand, current: i64) -> ApiResult<()> {
    match cmd.expected_version {
        Some(expected) if expected != current => Err(ApiError::domain(
            ErrorCode::VersionConflict,
            "The resource changed; re-read it and retry",
        )),
        _ => Ok(()),
    }
}

/// Records the applied command (same transaction as the change).
pub async fn record<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    subject_kind: &'static str,
    subject_id: Uuid,
    resulting_state: &str,
    resulting_version: Option<i64>,
) -> ApiResult<()> {
    let (approver, approval_ref) = match &cmd.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    txn.execute(stmt(
        "INSERT INTO meeting_admin_operations (idempotency_key, command, subject_kind, subject_id, \
           operator_ref, approver_ref, approval_ref, resulting_state, resulting_version, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        vec![
            cmd.key.into(),
            cmd.command.into(),
            subject_kind.into(),
            subject_id.into(),
            cmd.operator.clone().into(),
            approver.into(),
            approval_ref.into(),
            resulting_state.to_string().into(),
            resulting_version.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// The account's administration version (`users.account_version`), locked `FOR UPDATE`
/// when `lock`; `None` when there is no such account.
pub async fn account_version<C: ConnectionTrait>(
    conn: &C,
    user_id: Uuid,
    lock: bool,
) -> ApiResult<Option<i64>> {
    let sql = if lock {
        "SELECT account_version::bigint AS v FROM users WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT account_version::bigint AS v FROM users WHERE id = $1"
    };
    conn.query_one(stmt(sql, vec![user_id.into()]))
        .await?
        .map(|r| r.try_get::<i64>("", "v"))
        .transpose()
        .map_err(Into::into)
}

/// UPPER_SNAKE state code of a lower-case database value (`pending_verification` →
/// `PENDING_VERIFICATION`).
pub fn state_code(value: &str) -> String {
    value.trim().to_ascii_uppercase().replace([' ', '-'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(expected: Option<i64>) -> BridgeCommand {
        BridgeCommand {
            key: Uuid::nil(),
            command: "accounts.user.status.change",
            operator: "hq:staff-1".into(),
            approval: None,
            expected_version: expected,
            linked_user: None,
        }
    }

    #[test]
    fn versions_must_match_when_given() {
        assert!(check_version(&cmd(Some(3)), 3).is_ok());
        assert_eq!(
            check_version(&cmd(Some(2)), 3).unwrap_err().code(),
            Some(ErrorCode::VersionConflict)
        );
        assert!(check_version(&cmd(None), 9).is_ok());
    }

    #[test]
    fn state_codes_are_upper_snake() {
        assert_eq!(state_code("pending_verification"), "PENDING_VERIFICATION");
        assert_eq!(state_code("active"), "ACTIVE");
    }
}

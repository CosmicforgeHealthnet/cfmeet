//! ADR 0068, phase A4 — identity commands relayed by the HQ admin bridge.
//!
//! The admin bridge (`cfmeet.admin-bridge`, verified `hq:<staff>` attribution) runs the
//! same audited `operator_*` operations as the operator console, with three additions
//! that make a command settle exactly once:
//!
//! * every command carries HQ's `Idempotency-Key`; a repeated key for the same command
//!   and subject answers the current resource without applying again, the same key for
//!   anything else is refused (`CONFLICT`);
//! * a versioned subject changes only at the caller's `expectedVersion`: the subject row
//!   is locked `FOR UPDATE` in the mutation's transaction and a different version is
//!   `VERSION_CONFLICT` (nothing done);
//! * the applied command is recorded in `platform_access_admin_operations` in the SAME
//!   transaction as the change (and its audit row), with the resulting state and version
//!   the command answers; the bridge settles an unknown outcome from that record.
//!
//! The command travels inside the [`super::operator::OperatorContext`]
//! ([`super::operator::OperatorContext::for_bridge`]): the audited operations lock before
//! and record after their mutation whenever the context carries one. The audit row's
//! actor is the bridge identity and its correlation id `hq_<key>` joins it to the record
//! here, which holds the staff member and the approver.
//!
//! Versions: the subject row's `revision`, except a credential, whose version is
//! `revision + suspension changes` (a suspension lives outside the credential row). Scope
//! grants, principal registration and partner-user sign-out have no version.
//!
//! Answers and records carry ids, codes, states, counts and timestamps only — never a
//! secret, verifier, external reference, display name or free text.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement, Value as DbValue};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{PlatformError, PlatformResult};

/// What a bridge command is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Tenant,
    Application,
    Environment,
    ServiceAccount,
    Credential,
    Principal,
    AppGrant,
    /// An environment's allowed websites (`subject_detail`: the origin).
    EnvironmentOrigins,
    /// An application's grant of one scope (`subject_detail`: the scope name).
    ApplicationScope,
    /// A principal registered in an environment (subject id: the environment; the
    /// registered principal is the record's `resulting_id`).
    PrincipalRegistration,
    /// A partner user's sessions (subject id: the principal).
    PrincipalSessions,
}

impl Subject {
    pub const ALL: &'static [Subject] = &[
        Subject::Tenant,
        Subject::Application,
        Subject::Environment,
        Subject::ServiceAccount,
        Subject::Credential,
        Subject::Principal,
        Subject::AppGrant,
        Subject::EnvironmentOrigins,
        Subject::ApplicationScope,
        Subject::PrincipalRegistration,
        Subject::PrincipalSessions,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Subject::Tenant => "TENANT",
            Subject::Application => "APPLICATION",
            Subject::Environment => "ENVIRONMENT",
            Subject::ServiceAccount => "SERVICE_ACCOUNT",
            Subject::Credential => "CREDENTIAL",
            Subject::Principal => "PRINCIPAL",
            Subject::AppGrant => "APP_GRANT",
            Subject::EnvironmentOrigins => "ENVIRONMENT_ORIGINS",
            Subject::ApplicationScope => "APPLICATION_SCOPE",
            Subject::PrincipalRegistration => "PRINCIPAL_REGISTRATION",
            Subject::PrincipalSessions => "PRINCIPAL_SESSIONS",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == value)
    }

    /// `SELECT version … FOR UPDATE` of a versioned subject (`None`: unversioned).
    const fn lock_sql(self) -> Option<&'static str> {
        Some(match self {
            Subject::Tenant => "SELECT revision AS version FROM platform_tenants WHERE id = $1 FOR UPDATE",
            Subject::Application => {
                "SELECT revision AS version FROM platform_applications WHERE id = $1 FOR UPDATE"
            }
            Subject::Environment => {
                "SELECT revision AS version FROM platform_environments WHERE id = $1 FOR UPDATE"
            }
            Subject::ServiceAccount => {
                "SELECT revision AS version FROM platform_service_accounts WHERE id = $1 FOR UPDATE"
            }
            Subject::Credential => {
                "SELECT c.revision + COALESCE((SELECT x.changes FROM platform_credential_suspension_changes x \
                   WHERE x.credential_id = c.id), 0) AS version \
                 FROM platform_api_credentials c WHERE c.id = $1 FOR UPDATE"
            }
            Subject::Principal => {
                "SELECT revision AS version FROM platform_principals WHERE id = $1 FOR UPDATE"
            }
            Subject::AppGrant => {
                "SELECT revision AS version FROM platform_oauth_grants WHERE id = $1 FOR UPDATE"
            }
            Subject::EnvironmentOrigins => {
                "SELECT revision AS version FROM platform_environment_origins \
                 WHERE environment_id = $1 FOR UPDATE"
            }
            Subject::ApplicationScope
            | Subject::PrincipalRegistration
            | Subject::PrincipalSessions => return None,
        })
    }

    /// The subject's `state`, `version` (NULL: unversioned), `tenant_id` and closed
    /// resource `v`; `$1` = the subject id, `$2` = the detail (scope subjects only).
    const fn current_sql(self) -> &'static str {
        match self {
            Subject::Tenant => {
                "SELECT status AS state, revision AS version, id AS tenant_id, \
                   json_build_object('id', id, 'classification', classification, 'status', status, \
                     'revision', revision, 'updatedAt', updated_at) AS v \
                 FROM platform_tenants WHERE id = $1"
            }
            Subject::Application => {
                "SELECT status AS state, revision AS version, tenant_id, \
                   json_build_object('id', id, 'tenantId', tenant_id, 'status', status, \
                     'revision', revision, 'updatedAt', updated_at) AS v \
                 FROM platform_applications WHERE id = $1"
            }
            Subject::Environment => {
                "SELECT status AS state, revision AS version, tenant_id, \
                   json_build_object('id', id, 'tenantId', tenant_id, 'applicationId', application_id, \
                     'kind', kind, 'status', status, 'revision', revision, 'updatedAt', updated_at) AS v \
                 FROM platform_environments WHERE id = $1"
            }
            Subject::ServiceAccount => {
                "SELECT status AS state, revision AS version, tenant_id, \
                   json_build_object('id', id, 'tenantId', tenant_id, 'applicationId', application_id, \
                     'environmentId', environment_id, 'status', status, 'revokedAt', revoked_at, \
                     'revision', revision, 'updatedAt', updated_at) AS v \
                 FROM platform_service_accounts WHERE id = $1"
            }
            Subject::Credential => {
                "SELECT CASE WHEN c.status = 'REVOKED' THEN 'REVOKED' \
                             WHEN s.credential_id IS NOT NULL THEN 'SUSPENDED' ELSE c.status END AS state, \
                   c.revision + COALESCE(x.changes, 0) AS version, c.tenant_id, \
                   json_build_object('id', c.id, 'publicId', c.public_id, 'tenantId', c.tenant_id, \
                     'applicationId', c.application_id, 'environmentId', c.environment_id, \
                     'serviceAccountId', c.service_account_id, 'status', c.status, \
                     'suspendedAt', s.suspended_at, 'suspensionReason', s.reason_code, \
                     'revokedAt', c.revoked_at, 'revision', c.revision, \
                     'version', c.revision + COALESCE(x.changes, 0), 'updatedAt', c.updated_at) AS v \
                 FROM platform_api_credentials c \
                 LEFT JOIN platform_credential_suspensions s ON s.credential_id = c.id \
                 LEFT JOIN platform_credential_suspension_changes x ON x.credential_id = c.id \
                 WHERE c.id = $1"
            }
            Subject::Principal => {
                "SELECT CASE WHEN tombstoned_at IS NOT NULL THEN 'TOMBSTONED' ELSE status END AS state, \
                   revision AS version, tenant_id, \
                   json_build_object('id', id, 'tenantId', tenant_id, 'applicationId', application_id, \
                     'environmentId', environment_id, 'kind', kind, 'status', status, \
                     'tombstonedAt', tombstoned_at, 'revision', revision, 'updatedAt', updated_at) AS v \
                 FROM platform_principals WHERE id = $1"
            }
            Subject::AppGrant => {
                "SELECT status AS state, revision AS version, tenant_id, \
                   json_build_object('id', id, 'appId', app_id, 'target', target, 'tenantId', tenant_id, \
                     'environmentId', environment_id, 'principalId', principal_id, 'status', status, \
                     'revokedAt', revoked_at, 'revokeReason', revoke_reason, 'revision', revision, \
                     'updatedAt', updated_at) AS v \
                 FROM platform_oauth_grants WHERE id = $1"
            }
            Subject::EnvironmentOrigins => {
                "SELECT CASE WHEN cardinality(o.origins) = 0 THEN 'EMPTY' ELSE 'SET' END AS state, \
                   o.revision AS version, e.tenant_id, \
                   json_build_object('environmentId', e.id, 'tenantId', e.tenant_id, \
                     'applicationId', e.application_id, 'environmentKind', e.kind, \
                     'originCount', cardinality(o.origins), 'revision', o.revision, \
                     'updatedAt', o.updated_at) AS v \
                 FROM platform_environment_origins o \
                 JOIN platform_environments e ON e.id = o.environment_id \
                 WHERE o.environment_id = $1"
            }
            Subject::ApplicationScope => {
                "SELECT CASE WHEN s.scope IS NULL THEN 'REVOKED' ELSE 'GRANTED' END AS state, \
                   NULL::bigint AS version, a.tenant_id, \
                   json_build_object('applicationId', a.id, 'tenantId', a.tenant_id, 'scope', $2::text, \
                     'granted', s.scope IS NOT NULL, 'allowedEnvironmentKinds', s.allowed_environment_kinds, \
                     'grantBasis', s.grant_basis, 'grantedAt', s.granted_at) AS v \
                 FROM platform_applications a \
                 LEFT JOIN platform_application_scopes s ON s.application_id = a.id AND s.scope = $2::text \
                 WHERE a.id = $1"
            }
            Subject::PrincipalRegistration => {
                "SELECT 'REGISTERED' AS state, NULL::bigint AS version, tenant_id, \
                   json_build_object('environmentId', id, 'tenantId', tenant_id) AS v \
                 FROM platform_environments WHERE id = $1"
            }
            Subject::PrincipalSessions => {
                "SELECT CASE WHEN f.tokens_not_before IS NULL THEN 'ACTIVE' ELSE 'SIGNED_OUT' END AS state, \
                   NULL::bigint AS version, p.tenant_id, \
                   json_build_object('principalId', p.id, 'tenantId', p.tenant_id, \
                     'applicationId', p.application_id, 'environmentId', p.environment_id, \
                     'kind', p.kind, 'tokensNotBefore', f.tokens_not_before) AS v \
                 FROM platform_principals p \
                 LEFT JOIN platform_end_user_profiles f ON f.principal_id = p.id \
                 WHERE p.id = $1"
            }
        }
    }

    const fn takes_detail(self) -> bool {
        matches!(self, Subject::ApplicationScope)
    }
}

/// The bridge identity written as the audit actor of a bridge command.
pub const AUDIT_ACTOR: &str = "cfmeet.admin-bridge";

/// One verified bridge command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCommand {
    pub key: Uuid,
    /// The capability id (e.g. `identity.key.revoke`).
    pub command: String,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)`.
    pub approval: Option<(String, String)>,
    pub subject: Subject,
    pub subject_id: Uuid,
    /// The scope name or origin of a scope / website command.
    pub subject_detail: Option<String>,
    /// Required for a versioned subject.
    pub expected_version: Option<i64>,
}

/// A subject as it is now.
#[derive(Debug, Clone, PartialEq)]
pub struct Current {
    pub state: String,
    pub version: Option<i64>,
    pub tenant_id: Uuid,
    pub resource: Value,
}

fn stmt(sql: &str, values: Vec<DbValue>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// The subject's current state, version and resource (`None`: unknown).
pub async fn current<C: ConnectionTrait>(
    db: &C,
    subject: Subject,
    id: Uuid,
    detail: Option<&str>,
) -> PlatformResult<Option<Current>> {
    let mut values: Vec<DbValue> = vec![id.into()];
    if subject.takes_detail() {
        values.push(detail.map(str::to_string).into());
    }
    let Some(row) = db.query_one(stmt(subject.current_sql(), values)).await? else {
        return Ok(None);
    };
    Ok(Some(Current {
        state: row.try_get("", "state")?,
        version: row.try_get("", "version")?,
        tenant_id: row.try_get("", "tenant_id")?,
        resource: row.try_get("", "v")?,
    }))
}

/// Locks a versioned subject and checks the expected version, in the mutation's
/// transaction (unversioned subjects: nothing to lock).
pub async fn lock<C: ConnectionTrait>(txn: &C, cmd: &BridgeCommand) -> PlatformResult<()> {
    let Some(sql) = cmd.subject.lock_sql() else {
        return Ok(());
    };
    let row = txn
        .query_one(stmt(sql, vec![cmd.subject_id.into()]))
        .await?
        .ok_or(PlatformError::NotFound(subject_noun(cmd.subject)))?;
    let version: i64 = row.try_get("", "version")?;
    if Some(version) != cmd.expected_version {
        return Err(PlatformError::VersionConflict);
    }
    Ok(())
}

fn subject_noun(subject: Subject) -> &'static str {
    match subject {
        Subject::Tenant => "tenant",
        Subject::Application | Subject::ApplicationScope => "application",
        Subject::Environment | Subject::PrincipalRegistration => "environment",
        Subject::ServiceAccount => "service account",
        Subject::Credential => "credential",
        Subject::Principal | Subject::PrincipalSessions => "principal",
        Subject::AppGrant => "grant",
        Subject::EnvironmentOrigins => "allowed websites",
    }
}

/// Records the applied command (same transaction as the change). `resulting_id` is the
/// registered principal of a registration; `state` overrides the subject's own state
/// (a website removal answers `REMOVED`).
pub async fn record<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    resulting_id: Option<Uuid>,
    state: Option<&str>,
    now: DateTime<Utc>,
) -> PlatformResult<()> {
    let (resulting_id, now_state) = match cmd.subject {
        Subject::PrincipalRegistration => {
            let id = resulting_id.ok_or_else(|| internal("registration without a principal"))?;
            let p = current(txn, Subject::Principal, id, None)
                .await?
                .ok_or_else(|| internal("registered principal missing"))?;
            (
                Some(id),
                Current {
                    state: "REGISTERED".to_string(),
                    ..p
                },
            )
        }
        subject => (
            None,
            current(txn, subject, cmd.subject_id, cmd.subject_detail.as_deref())
                .await?
                .ok_or(PlatformError::NotFound(subject_noun(subject)))?,
        ),
    };
    let resulting_state = state.map(str::to_string).unwrap_or(now_state.state);
    let (approver, approval_ref) = match &cmd.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    txn.execute(stmt(
        "INSERT INTO platform_access_admin_operations (idempotency_key, command, subject_kind, \
           subject_id, subject_detail, tenant_id, resulting_id, operator_ref, approver_ref, \
           approval_ref, resulting_state, resulting_version, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        vec![
            cmd.key.into(),
            cmd.command.clone().into(),
            cmd.subject.as_str().into(),
            cmd.subject_id.into(),
            cmd.subject_detail.clone().into(),
            now_state.tenant_id.into(),
            resulting_id.into(),
            cmd.operator.clone().into(),
            approver.into(),
            approval_ref.into(),
            resulting_state.into(),
            now_state.version.into(),
            now.into(),
        ],
    ))
    .await?;
    crate::metrics::inc(
        "platform_access_bridge_commands_total",
        &[("subject", cmd.subject.as_str())],
    );
    Ok(())
}

fn internal(what: &str) -> PlatformError {
    PlatformError::Database {
        summary: what.to_string(),
        connectivity: false,
    }
}

/// One recorded command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub command: String,
    pub subject: Subject,
    pub subject_id: Uuid,
    pub subject_detail: Option<String>,
    pub resulting_id: Option<Uuid>,
    pub state: String,
    pub version: Option<i64>,
}

fn recorded_from(row: &QueryResult) -> PlatformResult<Recorded> {
    let kind: String = row.try_get("", "subject_kind")?;
    Ok(Recorded {
        command: row.try_get("", "command")?,
        subject: Subject::parse(&kind).ok_or_else(|| internal("unknown recorded subject"))?,
        subject_id: row.try_get("", "subject_id")?,
        subject_detail: row.try_get("", "subject_detail")?,
        resulting_id: row.try_get("", "resulting_id")?,
        state: row.try_get("", "resulting_state")?,
        version: row.try_get("", "resulting_version")?,
    })
}

/// The record of `key`, if it was applied.
pub async fn recorded<C: ConnectionTrait>(db: &C, key: Uuid) -> PlatformResult<Option<Recorded>> {
    db.query_one(stmt(
        "SELECT command, subject_kind, subject_id, subject_detail, resulting_id, resulting_state, \
           resulting_version FROM platform_access_admin_operations WHERE idempotency_key = $1",
        vec![key.into()],
    ))
    .await?
    .as_ref()
    .map(recorded_from)
    .transpose()
}

/// `true` when the key was already applied to this same command and subject; the same
/// key for anything else is a `CONFLICT`.
pub async fn replayed<C: ConnectionTrait>(db: &C, cmd: &BridgeCommand) -> PlatformResult<bool> {
    let Some(r) = recorded(db, cmd.key).await? else {
        return Ok(false);
    };
    if r.command == cmd.command
        && r.subject == cmd.subject
        && r.subject_id == cmd.subject_id
        && r.subject_detail == cmd.subject_detail
    {
        Ok(true)
    } else {
        Err(PlatformError::Conflict(
            "the Idempotency-Key was already used for another command".to_string(),
        ))
    }
}

/// The command's answer: `{status, version, resource}` — the recorded state and version
/// and the subject as it is now.
pub async fn answer<C: ConnectionTrait>(db: &C, key: Uuid) -> PlatformResult<Option<Value>> {
    let Some(r) = recorded(db, key).await? else {
        return Ok(None);
    };
    let (subject, id) = match (r.subject, r.resulting_id) {
        (Subject::PrincipalRegistration, Some(p)) => (Subject::Principal, p),
        (s, _) => (s, r.subject_id),
    };
    let resource = current(db, subject, id, r.subject_detail.as_deref())
        .await?
        .map(|c| c.resource)
        .unwrap_or(Value::Null);
    Ok(Some(json!({
        "status": r.state,
        "version": r.version.map(|v| v.to_string()),
        "resource": resource,
    })))
}

/// `GET /internal/v1/operator/operations/{key}`: the bridge's settlement read.
pub async fn operation_record<C: ConnectionTrait>(
    db: &C,
    key: Uuid,
) -> PlatformResult<Option<Value>> {
    Ok(recorded(db, key).await?.map(|r| {
        json!({
            "idempotency_key": key,
            "outcome": "APPLIED",
            "source_status": r.state,
            "source_version": r.version.map(|v| v.to_string()),
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_round_trip_and_only_versioned_ones_lock() {
        for s in Subject::ALL {
            assert_eq!(Subject::parse(s.as_str()), Some(*s));
            assert!(s
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b == b'_'));
        }
        assert_eq!(Subject::ALL.len(), 11);
        assert!(Subject::parse("tenant").is_none());
        for unversioned in [
            Subject::ApplicationScope,
            Subject::PrincipalRegistration,
            Subject::PrincipalSessions,
        ] {
            assert!(unversioned.lock_sql().is_none());
        }
        for s in Subject::ALL {
            if let Some(sql) = s.lock_sql() {
                assert!(sql.ends_with("FOR UPDATE"), "{sql}");
            }
            // No resource ever carries a secret, verifier, external reference or name.
            let sql = s.current_sql();
            for banned in [
                "verifier",
                "external_reference",
                "display_name",
                "label",
                "description",
                "secret",
            ] {
                assert!(!sql.contains(banned), "{s:?} selects {banned}");
            }
        }
    }
}

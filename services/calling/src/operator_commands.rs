//! The admin bridge's operator COMMANDS (ADR 0068 phase A4, design §6.4 / §9 / §17.6):
//! `POST /internal/v1/operator/*`, and the bridge's settlement read
//! `GET /internal/v1/operator/operations/{idempotencyKey}`.
//!
//! The HQ admin bridge (`cfmeet.admin-bridge`, the only holder of `calling.administer`)
//! relays an HQ staff member's command with verified attribution (`X-CF-Operator`,
//! parsed once by the internal-auth layer). Every command:
//!
//! * names its Platform Access tenant (`tenantId`, required) and never reaches another
//!   tenant's resource (404, as the operator reads);
//! * carries HQ's `Idempotency-Key` (a UUID; 400 otherwise);
//! * carries `expectedVersion` when its subject is versioned: the change happens only at
//!   that version, under the subject's row lock (409 `VERSION_CONFLICT`, nothing done).
//!   For a configuration a command creates or replaces (limits, routing / spend policy,
//!   the enterprise baseline), `0` means "not configured yet";
//! * R3 / R4 commands require the second person (`X-CF-Approver`, `X-CF-Approval-Ref`,
//!   `X-CF-Intent-Digest`; a different person, checked by the shared parser): without it
//!   403 `INTERNAL_PERMISSION_DENIED`, nothing done;
//! * is applied through the SAME domain logic as `platform_admin` (which stays for
//!   emergencies), with its operator audit row (actor `cfmeet.admin-bridge`, correlation
//!   `op_<key>`) and a `calling_admin_operations` row (who acted, who approved, the
//!   resulting state and version) in the SAME transaction as the change;
//! * a repeated key for the same command and subject answers with the current resource
//!   and applies nothing; the same key for anything else is 409 `IDEMPOTENCY_CONFLICT`;
//! * answers `{"status", "version", "resource"}`: the resulting state code, the resulting
//!   row version as a string (null when the subject is unversioned) and a small closed
//!   object of ids, codes, numbers and times. Never a telephone number, SIP URI, media
//!   URL, object key, key material, display name or free text.
//!
//! Refusals are definite 4xx and change nothing; a 4xx never follows a committed change.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::future::BoxFuture;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, FromQueryResult, SqlErr, Statement,
    TransactionTrait,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    calls::{
        app::{abuse_reports, blocks, incidents},
        command::CallScope,
    },
    domain::{ApplicationId, EnvironmentId, TenantId},
    enterprise::{policy::AdminCapability, EnterpriseError},
    error::{CallingError, ErrorCode},
    internal_auth::middleware::InternalCaller,
    operator_reads::{gate, OperatorExt},
    platform::{
        error::is_connectivity_error,
        limits::{LimitDefaults, LimitValues},
        operator::{self, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
        PlatformError, PlatformService,
    },
    retention::{
        holds::{self, HoldOwner, HoldReason, HoldScope},
        DataClass,
    },
    state::AppState,
};

/// Subscriptions re-sealed per batch by the bridge's tenant reseal.
const RESEAL_BATCH: u64 = 100;

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/operator/abuse-reports/:abuse_report_id/resolve",
            post(resolve_abuse_report),
        )
        .route(
            "/operator/blocks/:block_rule_id/release",
            post(release_block),
        )
        .route("/operator/limits", post(change_limits))
        .route(
            "/operator/routing-policies/:environment_id",
            post(change_routing_policy),
        )
        .route(
            "/operator/enterprise-policies/:environment_id/capabilities",
            post(change_enterprise_features),
        )
        .route(
            "/operator/spend-policies/:environment_id",
            post(change_spend_policy),
        )
        .route(
            "/operator/recordings/:recording_id/delete",
            post(delete_recording),
        )
        .route("/operator/retention-holds", post(place_retention_hold))
        .route(
            "/operator/retention-holds/:hold_id/release",
            post(release_retention_hold),
        )
        .route(
            "/operator/incidents/:incident_id/resolve",
            post(record_incident),
        )
        .route("/operator/webhook-keys/reseal", post(reseal_webhook_keys))
        .route(
            "/operator/operations/:idempotency_key",
            get(operation_record),
        )
}

// ---- Capabilities ----------------------------------------------------------------------

/// The capability ids HQ's catalog names (recorded as `command`).
pub mod capability {
    pub const ABUSE_REPORT_RESOLVE: &str = "calling.abuse-report.resolve";
    pub const BLOCK_RELEASE: &str = "calling.block.release";
    pub const LIMITS_CHANGE: &str = "calling.limits.change";
    pub const ROUTING_CHANGE: &str = "calling.routing.change";
    pub const ENTERPRISE_FEATURES_CHANGE: &str = "calling.enterprise-features.change";
    pub const SPEND_LIMITS_CHANGE: &str = "calling.spend-limits.change";
    pub const RECORDING_DELETE: &str = "calling.recording.delete";
    pub const RETENTION_HOLD_PLACE: &str = "calling.retention-hold.place";
    pub const RETENTION_HOLD_RELEASE: &str = "calling.retention-hold.release";
    pub const INCIDENT_RECORD: &str = "calling.incident.record";
    pub const WEBHOOK_KEY_RESEAL: &str = "calling.webhook-key.reseal";
}

/// Two-person (R3 / R4) or not (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Risk {
    R2,
    TwoPerson,
}

// ---- Subjects --------------------------------------------------------------------------

/// What a command changes (the record's `subject_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    AbuseReport,
    BlockRule,
    TenantLimits,
    ApplicationLimits,
    RoutingPolicy,
    EnterprisePolicy,
    SpendPolicy,
    Recording,
    RetentionHold,
    Incident,
    WebhookKeyRing,
}

impl Subject {
    const fn as_str(self) -> &'static str {
        match self {
            Subject::AbuseReport => "ABUSE_REPORT",
            Subject::BlockRule => "BLOCK_RULE",
            Subject::TenantLimits => "TENANT_LIMITS",
            Subject::ApplicationLimits => "APPLICATION_LIMITS",
            Subject::RoutingPolicy => "ROUTING_POLICY",
            Subject::EnterprisePolicy => "ENTERPRISE_POLICY",
            Subject::SpendPolicy => "SPEND_POLICY",
            Subject::Recording => "RECORDING",
            Subject::RetentionHold => "RETENTION_HOLD",
            Subject::Incident => "INCIDENT",
            Subject::WebhookKeyRing => "WEBHOOK_KEY_RING",
        }
    }

    /// A configuration the command creates or replaces: version 0 = not configured, and
    /// the change moves it by exactly one (a concurrent change is a conflict).
    const fn replaces(self) -> bool {
        matches!(
            self,
            Subject::TenantLimits
                | Subject::ApplicationLimits
                | Subject::RoutingPolicy
                | Subject::EnterprisePolicy
                | Subject::SpendPolicy
        )
    }

    /// The row lock taken before the version check (`$1` = the subject id).
    const fn lock_sql(self) -> Option<&'static str> {
        match self {
            Subject::AbuseReport => {
                Some("SELECT 1 FROM calling_abuse_reports WHERE id = $1 FOR UPDATE")
            }
            Subject::BlockRule => Some("SELECT 1 FROM calling_block_rules WHERE id = $1 FOR UPDATE"),
            Subject::TenantLimits => {
                Some("SELECT 1 FROM platform_tenant_limits WHERE tenant_id = $1 FOR UPDATE")
            }
            Subject::ApplicationLimits => Some(
                "SELECT 1 FROM platform_application_limits WHERE application_id = $1 FOR UPDATE",
            ),
            Subject::RoutingPolicy => {
                Some("SELECT 1 FROM call_routing_policies WHERE environment_id = $1 FOR UPDATE")
            }
            // The baseline's own lock (the same one the enterprise service takes).
            Subject::EnterprisePolicy => Some(
                "SELECT pg_advisory_xact_lock(hashtextextended('enterprise-policy-default:' || $1::text, 0))",
            ),
            Subject::SpendPolicy => {
                Some("SELECT 1 FROM call_spend_policies WHERE environment_id = $1 FOR UPDATE")
            }
            Subject::Recording => Some("SELECT 1 FROM call_recordings WHERE id = $1 FOR UPDATE"),
            Subject::RetentionHold => {
                Some("SELECT 1 FROM calling_retention_holds WHERE id = $1 FOR UPDATE")
            }
            Subject::Incident => Some("SELECT 1 FROM calling_incidents WHERE id = $1 FOR UPDATE"),
            Subject::WebhookKeyRing => None,
        }
    }

    /// The subject as HQ sees it (`$1` = the subject id): its tenant, state code, version
    /// (NULL when unversioned) and the closed resource document.
    const fn snapshot_sql(self) -> &'static str {
        match self {
            Subject::AbuseReport => {
                "SELECT r.tenant_id, r.status AS state, r.version::bigint AS version, \
                   json_build_object('abuseReportId', r.id, 'applicationId', r.application_id, \
                     'environmentId', r.environment_id, 'callId', r.call_id, 'category', r.category, \
                     'status', r.status, 'reviewedAt', r.reviewed_at, 'version', r.version) AS doc \
                 FROM calling_abuse_reports r WHERE r.id = $1"
            }
            Subject::BlockRule => {
                "SELECT b.tenant_id, b.status AS state, b.version::bigint AS version, \
                   json_build_object('blockRuleId', b.id, 'applicationId', b.application_id, \
                     'environmentId', b.environment_id, 'status', b.status, \
                     'releasedAt', b.released_at, 'version', b.version) AS doc \
                 FROM calling_block_rules b WHERE b.id = $1"
            }
            Subject::TenantLimits => {
                "SELECT t.id AS tenant_id, \
                   CASE WHEN l.tenant_id IS NULL THEN 'NOT_CONFIGURED' ELSE 'CONFIGURED' END AS state, \
                   COALESCE(l.version, 0)::bigint AS version, \
                   json_build_object('tenantId', t.id, 'applicationId', NULL::uuid, \
                     'requestsPerMinute', l.requests_per_minute, \
                     'maxConcurrentCommunications', l.max_concurrent_communications, \
                     'updatedAt', l.updated_at, 'version', COALESCE(l.version, 0)) AS doc \
                 FROM platform_tenants t LEFT JOIN platform_tenant_limits l ON l.tenant_id = t.id \
                 WHERE t.id = $1"
            }
            Subject::ApplicationLimits => {
                "SELECT a.tenant_id, \
                   CASE WHEN l.application_id IS NULL THEN 'NOT_CONFIGURED' ELSE 'CONFIGURED' END AS state, \
                   COALESCE(l.version, 0)::bigint AS version, \
                   json_build_object('tenantId', a.tenant_id, 'applicationId', a.id, \
                     'requestsPerMinute', l.requests_per_minute, \
                     'maxConcurrentCommunications', l.max_concurrent_communications, \
                     'updatedAt', l.updated_at, 'version', COALESCE(l.version, 0)) AS doc \
                 FROM platform_applications a \
                 LEFT JOIN platform_application_limits l ON l.application_id = a.id \
                 WHERE a.id = $1"
            }
            Subject::RoutingPolicy => {
                "SELECT e.tenant_id, \
                   CASE WHEN r.environment_id IS NULL THEN 'NOT_CONFIGURED' ELSE 'CONFIGURED' END AS state, \
                   COALESCE(r.revision, 0)::bigint AS version, \
                   json_build_object('environmentId', e.id, 'applicationId', e.application_id, \
                     'revision', COALESCE(r.revision, 0), 'updatedAt', r.updated_at) AS doc \
                 FROM platform_environments e \
                 LEFT JOIN call_routing_policies r ON r.environment_id = e.id \
                 WHERE e.id = $1"
            }
            Subject::EnterprisePolicy => {
                "SELECT e.tenant_id, \
                   CASE WHEN p.id IS NULL THEN 'NOT_CONFIGURED' ELSE 'CONFIGURED' END AS state, \
                   COALESCE(p.version, 0)::bigint AS version, \
                   json_build_object('environmentId', e.id, 'applicationId', e.application_id, \
                     'policyId', p.id, \
                     'adminCapabilities', COALESCE(to_json(p.admin_capabilities), '[]'::json), \
                     'version', COALESCE(p.version, 0), 'updatedAt', p.updated_at) AS doc \
                 FROM platform_environments e \
                 LEFT JOIN enterprise_call_policies p ON p.tenant_id = e.tenant_id \
                   AND p.application_id = e.application_id AND p.environment_id = e.id \
                   AND p.is_default AND p.status = 'ACTIVE' \
                 WHERE e.id = $1"
            }
            Subject::SpendPolicy => {
                "SELECT e.tenant_id, \
                   CASE WHEN s.environment_id IS NULL THEN 'NOT_CONFIGURED' \
                        WHEN s.enabled THEN 'ENABLED' ELSE 'DISABLED' END AS state, \
                   COALESCE(s.revision, 0)::bigint AS version, \
                   json_build_object('environmentId', e.id, 'applicationId', e.application_id, \
                     'currency', s.currency, 'enabled', s.enabled, \
                     'dailyLimitMinor', s.daily_limit_minor, 'monthlyLimitMinor', s.monthly_limit_minor, \
                     'revision', COALESCE(s.revision, 0), 'updatedAt', s.updated_at) AS doc \
                 FROM platform_environments e \
                 LEFT JOIN call_spend_policies s ON s.environment_id = e.id \
                 WHERE e.id = $1"
            }
            Subject::Recording => {
                "SELECT r.tenant_id, r.state, r.version::bigint AS version, \
                   json_build_object('recordingId', r.id, 'callId', r.call_id, \
                     'environmentId', r.environment_id, 'state', r.state, \
                     'deletedAt', r.deleted_at, 'version', r.version) AS doc \
                 FROM call_recordings r WHERE r.id = $1"
            }
            Subject::RetentionHold => {
                "SELECT h.tenant_id, \
                   CASE WHEN h.released_at IS NULL THEN 'ACTIVE' ELSE 'RELEASED' END AS state, \
                   NULL::bigint AS version, \
                   json_build_object('holdId', h.id, 'dataClass', h.data_class, \
                     'resourceId', h.resource_id, 'applicationId', h.application_id, \
                     'environmentId', h.environment_id, 'reasonCode', h.reason_code, \
                     'active', h.released_at IS NULL, 'createdAt', h.created_at, \
                     'releasedAt', h.released_at) AS doc \
                 FROM calling_retention_holds h WHERE h.id = $1"
            }
            Subject::Incident => {
                "SELECT i.tenant_id, i.status AS state, NULL::bigint AS version, \
                   json_build_object('incidentId', i.id, 'applicationId', i.application_id, \
                     'environmentId', i.environment_id, 'category', i.category, \
                     'severity', i.severity, 'status', i.status, 'resolvedAt', i.resolved_at) AS doc \
                 FROM calling_incidents i WHERE i.id = $1"
            }
            Subject::WebhookKeyRing => {
                "SELECT t.id AS tenant_id, 'RESEALED' AS state, NULL::bigint AS version, \
                   json_build_object('tenantId', t.id, 'keys', COALESCE((\
                     SELECT json_agg(json_build_object('keyId', k.secret_key_id, \
                       'subscriptions', k.n) ORDER BY k.secret_key_id NULLS FIRST) \
                     FROM (SELECT s.secret_key_id, count(*) AS n FROM webhook_subscriptions s \
                           WHERE s.tenant_id = t.id GROUP BY s.secret_key_id) k), '[]'::json)) AS doc \
                 FROM platform_tenants t WHERE t.id = $1"
            }
        }
    }
}

/// What must hold before the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The subject is at this version (0 = not configured, for a replaced configuration).
    Version(i64),
    /// The subject exists in the tenant (unversioned subjects).
    Exists,
    /// The command creates the subject (its id is the idempotency key).
    Creates,
}

/// One command on one subject.
#[derive(Debug, Clone, Copy)]
struct Op {
    command: &'static str,
    subject: Subject,
    subject_id: Uuid,
    tenant: Uuid,
    expect: Expect,
}

/// A verified bridge command (who, the key, the approval).
#[derive(Debug, Clone)]
struct BridgeCommand {
    key: Uuid,
    /// `hq:<staff>`.
    operator: String,
    /// `(hq:<approver>, approval ref)`.
    approval: Option<(String, String)>,
}

/// The gate, the key and the approval of a bridge command. The operator console and
/// every other caller are refused by the gate (`calling.administer` is the bridge's).
fn bridge_command(
    caller: &InternalCaller,
    operator: &OperatorExt,
    headers: &HeaderMap,
    risk: Risk,
) -> Result<BridgeCommand, CallingError> {
    gate(caller, operator)?;
    let attribution = &operator
        .as_ref()
        .ok_or(CallingError::InternalPermissionDenied)?
        .0
         .0;
    let mut keys = headers.get_all("idempotency-key").iter();
    let key = match (keys.next(), keys.next()) {
        (Some(v), None) => v.to_str().ok().and_then(|v| Uuid::parse_str(v.trim()).ok()),
        _ => None,
    }
    .ok_or(CallingError::Validation(
        "Idempotency-Key must be one UUID.",
    ))?;
    let approval = attribution
        .approval
        .as_ref()
        .map(|a| (a.approver.as_str().to_string(), a.approval_ref.clone()));
    if risk == Risk::TwoPerson && approval.is_none() {
        tracing::warn!(
            operator = %attribution.operator,
            "two-person operator command without an approval"
        );
        return Err(CallingError::InternalPermissionDenied);
    }
    Ok(BridgeCommand {
        key,
        operator: attribution.operator.as_str().to_string(),
        approval,
    })
}

fn body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, CallingError> {
    body.map(|Json(b)| b)
        .map_err(|_| CallingError::Validation("The request body is invalid."))
}

fn path_id(value: &str) -> Result<Uuid, CallingError> {
    Uuid::parse_str(value).map_err(|_| CallingError::NotFound)
}

fn row_version(v: i64) -> Result<i64, CallingError> {
    if v >= 1 {
        Ok(v)
    } else {
        Err(CallingError::Validation(
            "expectedVersion must be an integer of at least 1.",
        ))
    }
}

fn config_version(v: i64) -> Result<i64, CallingError> {
    if v >= 0 {
        Ok(v)
    } else {
        Err(CallingError::Validation(
            "expectedVersion must be a non-negative integer (0 = not configured).",
        ))
    }
}

// ---- Errors ----------------------------------------------------------------------------

fn conflict(code: ErrorCode) -> CallingError {
    CallingError::Call {
        code,
        status: StatusCode::CONFLICT,
    }
}

fn db_error(e: DbErr) -> CallingError {
    if is_connectivity_error(&e) {
        tracing::warn!(error = %e, "operator command: database unavailable");
        CallingError::ServiceUnavailable
    } else {
        tracing::error!(error = %e, "operator command");
        CallingError::Internal
    }
}

fn platform_error(e: PlatformError) -> CallingError {
    match e {
        PlatformError::NotFound(_) => CallingError::NotFound,
        PlatformError::Validation(m) => {
            tracing::info!(reason = %m, "operator command refused");
            CallingError::Validation("The request is invalid.")
        }
        PlatformError::Conflict(_)
        | PlatformError::InvalidState(_)
        | PlatformError::PolicyViolation(_) => {
            tracing::info!(error = %e, "operator command refused");
            conflict(ErrorCode::Conflict)
        }
        e @ PlatformError::Database { .. } => CallingError::from(e),
    }
}

fn enterprise_error(e: EnterpriseError) -> CallingError {
    match e {
        EnterpriseError::NotFound => CallingError::NotFound,
        EnterpriseError::Validation(m) => CallingError::Validation(m),
        EnterpriseError::VersionConflict { .. } => conflict(ErrorCode::VersionConflict),
        EnterpriseError::Conflict(m) | EnterpriseError::Unavailable(m) => {
            tracing::info!(reason = m, "operator command refused");
            conflict(ErrorCode::Conflict)
        }
        EnterpriseError::PolicyDenied(_) => CallingError::Call {
            code: ErrorCode::EnterprisePolicyDenied,
            status: StatusCode::FORBIDDEN,
        },
        EnterpriseError::StorageUnavailable => CallingError::ServiceUnavailable,
        EnterpriseError::Database {
            summary,
            connectivity,
        } => {
            tracing::error!(%summary, "operator command");
            if connectivity {
                CallingError::ServiceUnavailable
            } else {
                CallingError::Internal
            }
        }
    }
}

// ---- The runner ------------------------------------------------------------------------

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

#[derive(Debug, FromQueryResult)]
struct Snapshot {
    tenant_id: Option<Uuid>,
    state: String,
    version: Option<i64>,
    doc: Value,
}

/// The subject as HQ sees it, if it exists in the op's tenant.
async fn snapshot<C: ConnectionTrait>(db: &C, op: &Op) -> Result<Option<Snapshot>, CallingError> {
    let found =
        Snapshot::find_by_statement(stmt(op.subject.snapshot_sql(), vec![op.subject_id.into()]))
            .one(db)
            .await
            .map_err(db_error)?;
    Ok(found.filter(|s| s.tenant_id == Some(op.tenant)))
}

fn answer(s: Snapshot) -> Response {
    let mut r = Json(json!({
        "status": s.state,
        "version": s.version.map(|v| v.to_string()),
        "resource": s.doc,
    }))
    .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// A key already applied: the same command on the same subject (and tenant) answers with
/// the current resource; anything else is `IDEMPOTENCY_CONFLICT`.
async fn replayed(
    state: &AppState,
    cmd: &BridgeCommand,
    op: &Op,
) -> Result<Option<Response>, CallingError> {
    let row = state
        .db
        .query_one(stmt(
            "SELECT command, subject_kind, subject_id, tenant_id FROM calling_admin_operations \
             WHERE idempotency_key = $1",
            vec![cmd.key.into()],
        ))
        .await
        .map_err(db_error)?;
    let Some(row) = row else { return Ok(None) };
    let (command, kind, id, tenant): (String, String, Uuid, Uuid) = (
        row.try_get_by_index(0).map_err(db_error)?,
        row.try_get_by_index(1).map_err(db_error)?,
        row.try_get_by_index(2).map_err(db_error)?,
        row.try_get_by_index(3).map_err(db_error)?,
    );
    if command != op.command
        || kind != op.subject.as_str()
        || id != op.subject_id
        || tenant != op.tenant
    {
        return Err(conflict(ErrorCode::IdempotencyConflict));
    }
    let current = snapshot(&state.db, op)
        .await?
        .ok_or(CallingError::NotFound)?;
    tracing::info!(command = op.command, key = %cmd.key, "operator command replayed");
    Ok(Some(answer(current)))
}

type Mutation = Box<
    dyn for<'t> FnOnce(
            &'t DatabaseTransaction,
        ) -> BoxFuture<'t, Result<Option<AuditEntry>, CallingError>>
        + Send,
>;

/// Replays a key already applied, else applies the command.
async fn execute(
    state: &AppState,
    cmd: &BridgeCommand,
    op: Op,
    mutation: Mutation,
) -> Result<Response, CallingError> {
    if let Some(r) = replayed(state, cmd, &op).await? {
        return Ok(r);
    }
    apply(state, cmd, op, mutation).await
}

/// One transaction: lock, check the expected version, change (through the domain
/// logic), audit, record the operation, commit.
async fn apply(
    state: &AppState,
    cmd: &BridgeCommand,
    op: Op,
    mutation: Mutation,
) -> Result<Response, CallingError> {
    let now = state.clock.now();
    let txn = state.db.begin().await.map_err(db_error)?;
    if let Some(lock) = op.subject.lock_sql() {
        txn.execute(stmt(lock, vec![op.subject_id.into()]))
            .await
            .map_err(db_error)?;
    }
    match op.expect {
        Expect::Creates => {}
        Expect::Exists | Expect::Version(_) => {
            let before = snapshot(&txn, &op).await?.ok_or(CallingError::NotFound)?;
            if let Expect::Version(v) = op.expect {
                if before.version != Some(v) {
                    return Err(conflict(ErrorCode::VersionConflict));
                }
            }
        }
    }
    let entry = mutation(&txn).await?;
    if let Some(entry) = entry {
        operator::record(&txn, &OperatorContext::bridge(cmd.key), &entry, now)
            .await
            .map_err(platform_error)?;
    }
    let after = snapshot(&txn, &op).await?.ok_or(CallingError::Internal)?;
    if let (Expect::Version(v), true) = (op.expect, op.subject.replaces()) {
        // A replaced configuration moves by exactly one: anything else means a concurrent
        // writer slipped in (the row did not exist to be locked).
        if after.version != Some(v + 1) {
            return Err(conflict(ErrorCode::VersionConflict));
        }
    }
    let (approver, approval_ref) = match &cmd.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    txn.execute(stmt(
        "INSERT INTO calling_admin_operations (idempotency_key, command, subject_kind, subject_id, \
           tenant_id, operator_ref, approver_ref, approval_ref, resulting_state, resulting_version, \
           recorded_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        vec![
            cmd.key.into(),
            op.command.into(),
            op.subject.as_str().into(),
            op.subject_id.into(),
            op.tenant.into(),
            cmd.operator.clone().into(),
            approver.into(),
            approval_ref.into(),
            after.state.clone().into(),
            after.version.into(),
            now.into(),
        ],
    ))
    .await
    .map_err(|e| match e.sql_err() {
        // The same key, concurrently: nothing of this transaction commits.
        Some(SqlErr::UniqueConstraintViolation(_)) => conflict(ErrorCode::IdempotencyConflict),
        _ => db_error(e),
    })?;
    txn.commit().await.map_err(db_error)?;
    tracing::info!(
        command = op.command,
        key = %cmd.key,
        operator = %cmd.operator,
        approved = cmd.approval.is_some(),
        subject_kind = op.subject.as_str(),
        subject_id = %op.subject_id,
        resulting_state = %after.state,
        "operator command applied"
    );
    Ok(answer(after))
}

/// The application and environment of `environment_id` in `tenant` (404 otherwise).
async fn environment(
    state: &AppState,
    tenant: Uuid,
    environment_id: Uuid,
) -> Result<CallScope, CallingError> {
    let row = state
        .db
        .query_one(stmt(
            "SELECT application_id FROM platform_environments WHERE id = $1 AND tenant_id = $2",
            vec![environment_id.into(), tenant.into()],
        ))
        .await
        .map_err(db_error)?
        .ok_or(CallingError::NotFound)?;
    let application: Uuid = row.try_get_by_index(0).map_err(db_error)?;
    Ok(CallScope::from_parts(
        TenantId::from_uuid(tenant),
        ApplicationId::from_uuid(application),
        EnvironmentId::from_uuid(environment_id),
    ))
}

fn environment_entry(action: OperatorAction, scope: CallScope) -> AuditEntry {
    let (t, a, e) = (
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
    );
    AuditEntry::new(action, AuditTargetKind::Environment, e).scope(Some(t), Some(a), Some(e))
}

// ---- Commands --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveAbuseReportBody {
    tenant_id: Uuid,
    expected_version: i64,
    status: abuse_reports::AbuseReportStatus,
}

/// `calling.abuse-report.resolve` (R2): the same audited transition as the operator
/// console's resolve and `platform_admin abuse-report set-status`.
async fn resolve_abuse_report(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(id): Path<String>,
    payload: Result<Json<ResolveAbuseReportBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::R2)?;
    let b = body(payload)?;
    let id = path_id(&id)?;
    let op = Op {
        command: capability::ABUSE_REPORT_RESOLVE,
        subject: Subject::AbuseReport,
        subject_id: id,
        tenant: b.tenant_id,
        expect: Expect::Version(row_version(b.expected_version)?),
    };
    let (status, now) = (b.status, state.clock.now());
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let (_, entry) = abuse_reports::operator_set_status_in(txn, id, status, now)
                    .await
                    .map_err(platform_error)?;
                Ok(Some(entry))
            })
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionedBody {
    tenant_id: Uuid,
    expected_version: i64,
}

/// `calling.block.release` (R3): `platform_admin block release`.
async fn release_block(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(id): Path<String>,
    payload: Result<Json<VersionedBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let id = path_id(&id)?;
    let op = Op {
        command: capability::BLOCK_RELEASE,
        subject: Subject::BlockRule,
        subject_id: id,
        tenant: b.tenant_id,
        expect: Expect::Version(row_version(b.expected_version)?),
    };
    let now = state.clock.now();
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let (_, entry) = blocks::operator_release_in(txn, id, now)
                    .await
                    .map_err(platform_error)?;
                Ok(Some(entry))
            })
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LimitsBody {
    tenant_id: Uuid,
    /// The application level; absent / null = the tenant level.
    #[serde(default)]
    application_id: Option<Uuid>,
    expected_version: i64,
    /// null = not configured.
    #[serde(default)]
    requests_per_minute: Option<i32>,
    /// null = not configured.
    #[serde(default)]
    max_concurrent_communications: Option<i32>,
}

/// `calling.limits.change` (R3): replaces the request-rate and concurrency limits of the
/// tenant or one of its applications (`platform_admin limits set-tenant` /
/// `set-application`). The telecom spend ceilings of the same row are kept as they are
/// (spend is not an R3 change).
async fn change_limits(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    payload: Result<Json<LimitsBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let (subject, subject_id) = match b.application_id {
        None => (Subject::TenantLimits, b.tenant_id),
        Some(a) => (Subject::ApplicationLimits, a),
    };
    let op = Op {
        command: capability::LIMITS_CHANGE,
        subject,
        subject_id,
        tenant: b.tenant_id,
        expect: Expect::Version(config_version(b.expected_version)?),
    };
    let platform = PlatformService::new(
        state.db.clone(),
        state.clock.clone(),
        LimitDefaults::default(),
    );
    let (rpm, concurrent, tenant) = (
        b.requests_per_minute,
        b.max_concurrent_communications,
        b.tenant_id,
    );
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let (table, key) = match subject {
                    Subject::TenantLimits => ("platform_tenant_limits", "tenant_id"),
                    _ => ("platform_application_limits", "application_id"),
                };
                let current = txn
                    .query_one(stmt(
                        &format!(
                            "SELECT telecom_spend_daily_limit_minor, telecom_spend_monthly_limit_minor \
                             FROM {table} WHERE {key} = $1"
                        ),
                        vec![subject_id.into()],
                    ))
                    .await
                    .map_err(db_error)?;
                let (daily, monthly): (Option<i64>, Option<i64>) = match current {
                    Some(r) => (
                        r.try_get_by_index(0).map_err(db_error)?,
                        r.try_get_by_index(1).map_err(db_error)?,
                    ),
                    None => (None, None),
                };
                let values = LimitValues {
                    requests_per_minute: rpm,
                    max_concurrent_communications: concurrent,
                    telecom_spend_daily_limit_minor: daily,
                    telecom_spend_monthly_limit_minor: monthly,
                };
                let entry = if subject == Subject::TenantLimits {
                    let v = platform
                        .set_tenant_limits_in(txn, TenantId::from_uuid(tenant), values)
                        .await
                        .map_err(platform_error)?;
                    AuditEntry::new(OperatorAction::LimitsSet, AuditTargetKind::Tenant, tenant)
                        .scope(Some(tenant), None, None)
                        .detail(operator::limit_detail("TENANT", &v))
                } else {
                    let v = platform
                        .set_application_limits_in(txn, ApplicationId::from_uuid(subject_id), values)
                        .await
                        .map_err(platform_error)?;
                    AuditEntry::new(
                        OperatorAction::LimitsSet,
                        AuditTargetKind::Application,
                        subject_id,
                    )
                    .scope(Some(tenant), Some(subject_id), None)
                    .detail(operator::limit_detail("APPLICATION", &v))
                };
                Ok(Some(entry))
            })
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RoutingPolicyBody {
    tenant_id: Uuid,
    expected_version: i64,
    /// The whole typed routing policy (camelCase RoutingPolicy; unknown fields refused).
    policy: Value,
}

/// `calling.routing.change` (R3): replaces an environment's routing policy with a
/// validated document (`platform_admin routing-policy set`). Version = the revision.
async fn change_routing_policy(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
    payload: Result<Json<RoutingPolicyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    use crate::routing::{policy::RoutingPolicy, store};
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let environment_id = path_id(&environment_id)?;
    let policy = RoutingPolicy::parse(&b.policy).map_err(CallingError::Validation)?;
    let op = Op {
        command: capability::ROUTING_CHANGE,
        subject: Subject::RoutingPolicy,
        subject_id: environment_id,
        tenant: b.tenant_id,
        expect: Expect::Version(config_version(b.expected_version)?),
    };
    let scope = environment(&state, b.tenant_id, environment_id).await?;
    let now = state.clock.now();
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let revision = store::store_policy(
                    txn,
                    scope.tenant_id().as_uuid(),
                    scope.application_id().as_uuid(),
                    scope.environment_id().as_uuid(),
                    &policy,
                    now,
                )
                .await
                .map_err(|e| match e {
                    store::PolicyStoreError::Invalid(m) => CallingError::Validation(m),
                    store::PolicyStoreError::Database(e) => db_error(e),
                })?;
                Ok(Some(
                    environment_entry(OperatorAction::RoutingPolicySet, scope)
                        .detail(json!({ "revision": revision })),
                ))
            })
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnterpriseFeaturesBody {
    tenant_id: Uuid,
    expected_version: i64,
    /// The whole capability set of the baseline (distinct; empty = none).
    capabilities: Vec<AdminCapability>,
}

/// `calling.enterprise-features.change` (R3): replaces the administrative capabilities
/// of the environment's enterprise baseline policy (`platform_admin
/// enterprise-capabilities set`; the baseline is created at version 1 when none exists).
async fn change_enterprise_features(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
    payload: Result<Json<EnterpriseFeaturesBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let environment_id = path_id(&environment_id)?;
    let op = Op {
        command: capability::ENTERPRISE_FEATURES_CHANGE,
        subject: Subject::EnterprisePolicy,
        subject_id: environment_id,
        tenant: b.tenant_id,
        expect: Expect::Version(config_version(b.expected_version)?),
    };
    let scope = environment(&state, b.tenant_id, environment_id).await?;
    let (enterprise, capabilities) = (state.enterprise.clone(), b.capabilities);
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let view = enterprise
                    .set_capabilities_in(txn, scope, capabilities)
                    .await
                    .map_err(enterprise_error)?;
                Ok(Some(crate::enterprise::policy::capabilities_audit_entry(
                    scope, &view,
                )))
            })
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SpendPolicyBody {
    tenant_id: Uuid,
    expected_version: i64,
    currency: String,
    leg_allowance_minor: i64,
    max_leg_seconds: i32,
    daily_limit_minor: i64,
    monthly_limit_minor: i64,
    max_concurrent_legs: i32,
    max_originations_per_minute: i32,
    max_per_party_per_hour: i32,
    #[serde(default)]
    max_per_principal_per_hour: Option<i32>,
    #[serde(default)]
    max_per_service_account_per_hour: Option<i32>,
    #[serde(default)]
    max_per_context_per_hour: Option<i32>,
    enabled: bool,
}

/// `calling.spend-limits.change` (R4): replaces an environment's telecom spend /
/// velocity policy (`platform_admin spend-policy set`). Version = the revision.
async fn change_spend_policy(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
    payload: Result<Json<SpendPolicyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    use crate::telephony::spend;
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let environment_id = path_id(&environment_id)?;
    let currency = b.currency.trim().to_string();
    if currency.len() != 3 || !currency.bytes().all(|c| c.is_ascii_uppercase()) {
        return Err(CallingError::Validation(
            "currency must be an ISO 4217 code.",
        ));
    }
    let op = Op {
        command: capability::SPEND_LIMITS_CHANGE,
        subject: Subject::SpendPolicy,
        subject_id: environment_id,
        tenant: b.tenant_id,
        expect: Expect::Version(config_version(b.expected_version)?),
    };
    let scope = environment(&state, b.tenant_id, environment_id).await?;
    let policy = spend::SpendPolicy {
        currency,
        leg_allowance_minor: b.leg_allowance_minor,
        max_leg_seconds: b.max_leg_seconds,
        daily_limit_minor: b.daily_limit_minor,
        monthly_limit_minor: b.monthly_limit_minor,
        max_concurrent_legs: b.max_concurrent_legs,
        max_originations_per_minute: b.max_originations_per_minute,
        max_per_party_per_hour: b.max_per_party_per_hour,
        max_per_principal_per_hour: b.max_per_principal_per_hour,
        max_per_service_account_per_hour: b.max_per_service_account_per_hour,
        max_per_context_per_hour: b.max_per_context_per_hour,
        enabled: b.enabled,
    };
    let now = state.clock.now();
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let revision = spend::store_policy(
                    txn,
                    spend::SpendScope {
                        tenant_id: scope.tenant_id().as_uuid(),
                        application_id: scope.application_id().as_uuid(),
                        environment_id: scope.environment_id().as_uuid(),
                    },
                    &policy,
                    now,
                )
                .await
                .map_err(|e| {
                    if is_connectivity_error(&e) {
                        CallingError::ServiceUnavailable
                    } else {
                        // The policy's CHECK constraints (as `platform_admin`).
                        tracing::info!(error = %e, "spend policy refused");
                        CallingError::Validation(
                            "The spend policy was refused (limits must be positive and consistent).",
                        )
                    }
                })?;
                Ok(Some(
                    environment_entry(OperatorAction::SpendPolicySet, scope).detail(json!({
                        "revision": revision,
                        "enabled": policy.enabled,
                        "currency": policy.currency,
                        "legAllowanceMinor": policy.leg_allowance_minor,
                        "maxLegSeconds": policy.max_leg_seconds,
                    })),
                ))
            })
        }),
    )
    .await
}

/// `calling.recording.delete` (R4): the administrator's safe deletion path (READY /
/// FAILED → DELETING; the recording worker removes the object). Refused (409 CONFLICT,
/// nothing done) under a retention hold on the recording or its Call, and for a
/// recording still being captured or already deleted. Access-audited; no media is read.
async fn delete_recording(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(id): Path<String>,
    payload: Result<Json<VersionedBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let id = path_id(&id)?;
    let op = Op {
        command: capability::RECORDING_DELETE,
        subject: Subject::Recording,
        subject_id: id,
        tenant: b.tenant_id,
        expect: Expect::Version(row_version(b.expected_version)?),
    };
    let (tenant, now, correlation) = (
        b.tenant_id,
        state.clock.now(),
        format!("op_{}", cmd.key.simple()),
    );
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let row = txn
                    .query_one(stmt(
                        "SELECT application_id, environment_id FROM call_recordings \
                         WHERE id = $1 AND tenant_id = $2",
                        vec![id.into(), tenant.into()],
                    ))
                    .await
                    .map_err(db_error)?
                    .ok_or(CallingError::NotFound)?;
                let scope = CallScope::from_parts(
                    TenantId::from_uuid(tenant),
                    ApplicationId::from_uuid(row.try_get_by_index(0).map_err(db_error)?),
                    EnvironmentId::from_uuid(row.try_get_by_index(1).map_err(db_error)?),
                );
                crate::enterprise::recording::operator_delete_in(txn, scope, id, &correlation, now)
                    .await
                    .map_err(enterprise_error)?;
                // The recording access audit is this command's audit row.
                Ok(None)
            })
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlaceHoldBody {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    /// A data class, or `*` for every class.
    data_class: String,
    /// One resource of the scope; absent / null = every resource.
    #[serde(default)]
    resource_id: Option<Uuid>,
    reason_code: HoldReason,
}

/// `calling.retention-hold.place` (R4): `platform_admin retention hold` for one scope of
/// the tenant (never platform-wide). The hold's id is the command's `Idempotency-Key`.
async fn place_retention_hold(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    payload: Result<Json<PlaceHoldBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let data_class = match b.data_class.trim() {
        "*" => None,
        other => Some(DataClass::parse(other).ok_or(CallingError::Validation(
            "dataClass must be `*` or a retention data class.",
        ))?),
    };
    let op = Op {
        command: capability::RETENTION_HOLD_PLACE,
        subject: Subject::RetentionHold,
        subject_id: cmd.key,
        tenant: b.tenant_id,
        expect: Expect::Creates,
    };
    let scope = HoldScope {
        data_class,
        resource_id: b.resource_id,
        owner: Some(HoldOwner {
            tenant_id: b.tenant_id,
            application_id: b.application_id,
            environment_id: b.environment_id,
        }),
    };
    let (reason, now, id) = (b.reason_code, state.clock.now(), cmd.key);
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let (_, entry) = holds::place_in(
                    txn,
                    cf_service_auth::services::ADMIN_BRIDGE,
                    id,
                    scope,
                    reason,
                    now,
                )
                .await
                .map_err(platform_error)?;
                Ok(Some(entry))
            })
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TenantBody {
    tenant_id: Uuid,
}

/// `calling.retention-hold.release` (R4): `platform_admin retention release` for a hold
/// of the tenant (a platform-wide hold is not the tenant's: 404). Unversioned;
/// idempotent.
async fn release_retention_hold(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(id): Path<String>,
    payload: Result<Json<TenantBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let id = path_id(&id)?;
    let op = Op {
        command: capability::RETENTION_HOLD_RELEASE,
        subject: Subject::RetentionHold,
        subject_id: id,
        tenant: b.tenant_id,
        expect: Expect::Exists,
    };
    let now = state.clock.now();
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let (_, entry) = holds::release_in(txn, id, now)
                    .await
                    .map_err(platform_error)?;
                Ok(Some(entry))
            })
        }),
    )
    .await
}

/// `calling.incident.record` (R2): records the resolution of one of the tenant's
/// incidents (`platform_admin incidents resolve`; the audit rows are the resolution
/// history). A platform-wide incident is not the tenant's: 404. Unversioned; idempotent.
async fn record_incident(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    Path(id): Path<String>,
    payload: Result<Json<TenantBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::R2)?;
    let b = body(payload)?;
    let id = path_id(&id)?;
    let op = Op {
        command: capability::INCIDENT_RECORD,
        subject: Subject::Incident,
        subject_id: id,
        tenant: b.tenant_id,
        expect: Expect::Exists,
    };
    let now = state.clock.now();
    execute(
        &state,
        &cmd,
        op,
        Box::new(move |txn| {
            Box::pin(async move {
                let (_, entry) = incidents::operator_resolve_in(txn, id, now)
                    .await
                    .map_err(platform_error)?;
                Ok(Some(entry))
            })
        }),
    )
    .await
}

/// `calling.webhook-key.reseal` (R3): `platform_admin webhooks reseal` for the tenant's
/// subscriptions only. The re-seal runs in bounded, idempotent batches (each its own
/// transaction, as the CLI's); the audit row and the operation record commit together
/// once every batch is done, so a failure part-way records nothing and a new command
/// finishes the rest. Never a secret, a seal or key material: counts and key ids only.
async fn reseal_webhook_keys(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    headers: HeaderMap,
    payload: Result<Json<TenantBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let cmd = bridge_command(&caller, &operator, &headers, Risk::TwoPerson)?;
    let b = body(payload)?;
    let op = Op {
        command: capability::WEBHOOK_KEY_RESEAL,
        subject: Subject::WebhookKeyRing,
        subject_id: b.tenant_id,
        tenant: b.tenant_id,
        expect: Expect::Exists,
    };
    if let Some(r) = replayed(&state, &cmd, &op).await? {
        return Ok(r);
    }
    snapshot(&state.db, &op)
        .await?
        .ok_or(CallingError::NotFound)?;
    if !state.webhooks.key_ring_configured() {
        return Err(conflict(ErrorCode::Conflict));
    }
    let (mut resealed, mut unopenable, mut batches, mut after) = (0u64, 0u64, 0u64, None);
    loop {
        let report = state
            .webhooks
            .reseal_batch_of(Some(b.tenant_id), after, RESEAL_BATCH)
            .await?;
        resealed += report.resealed;
        unopenable += report.unopenable;
        batches += 1;
        match report.last_id {
            Some(last) => after = Some(last),
            None => break,
        }
    }
    let entry = AuditEntry {
        action: OperatorAction::WebhookKeysResealed,
        target_kind: AuditTargetKind::WebhookKeyRing,
        target_id: None,
        tenant_id: Some(b.tenant_id),
        application_id: None,
        environment_id: None,
        detail: json!({
            "resealed": resealed,
            "unopenable": unopenable,
            "batches": batches,
            "batchSize": RESEAL_BATCH,
        }),
    };
    apply(
        &state,
        &cmd,
        op,
        Box::new(move |_txn| Box::pin(async move { Ok(Some(entry)) })),
    )
    .await
}

// ---- Settlement ------------------------------------------------------------------------

/// `GET /operator/operations/{idempotencyKey}`: the bridge's readback of an applied
/// command (`APPLIED`, with the status and version the command answered). Unknown key,
/// or any caller but the bridge with a named operator: 404.
async fn operation_record(
    State(state): State<AppState>,
    caller: InternalCaller,
    operator: OperatorExt,
    Path(key): Path<String>,
) -> Result<Response, CallingError> {
    if gate(&caller, &operator).is_err() {
        return Err(CallingError::NotFound);
    }
    let key = path_id(&key)?;
    let row = state
        .db
        .query_one(stmt(
            "SELECT resulting_state, resulting_version FROM calling_admin_operations \
             WHERE idempotency_key = $1",
            vec![key.into()],
        ))
        .await
        .map_err(db_error)?
        .ok_or(CallingError::NotFound)?;
    let (status, version): (String, Option<i64>) = (
        row.try_get_by_index(0).map_err(db_error)?,
        row.try_get_by_index(1).map_err(db_error)?,
    );
    let mut r = Json(json!({
        "idempotency_key": key,
        "outcome": "APPLIED",
        "source_status": status,
        "source_version": version.map(|v| v.to_string()),
    }))
    .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBJECTS: [Subject; 11] = [
        Subject::AbuseReport,
        Subject::BlockRule,
        Subject::TenantLimits,
        Subject::ApplicationLimits,
        Subject::RoutingPolicy,
        Subject::EnterprisePolicy,
        Subject::SpendPolicy,
        Subject::Recording,
        Subject::RetentionHold,
        Subject::Incident,
        Subject::WebhookKeyRing,
    ];

    #[test]
    fn subjects_are_the_tables_vocabulary_and_never_relay_contact_or_media_data() {
        let migration = include_str!(
            "../migration/src/admin_operations/m20261210_000058_calling_admin_operations.rs"
        );
        for s in SUBJECTS {
            assert!(migration.contains(&format!("'{}'", s.as_str())), "{s:?}");
            let sql = s.snapshot_sql().to_lowercase();
            for banned in [
                "object_key",
                "secret_sealed",
                "contact",
                "display_name",
                "e164",
                "sip_uri",
                "url",
                "detail",
                "restricted_evidence",
            ] {
                assert!(!sql.contains(banned), "{s:?} relays {banned}");
            }
            assert!(sql.contains("where") && sql.contains("$1"), "{s:?}");
        }
    }

    #[test]
    fn replaced_configurations_are_versioned_from_zero() {
        for s in SUBJECTS {
            let sql = s.snapshot_sql();
            if s.replaces() {
                assert!(
                    sql.contains("COALESCE("),
                    "{s:?} answers version 0 when absent"
                );
            }
        }
        assert!(Subject::WebhookKeyRing.lock_sql().is_none());
    }

    #[test]
    fn capability_ids_are_hq_catalog_ids() {
        for c in [
            capability::ABUSE_REPORT_RESOLVE,
            capability::BLOCK_RELEASE,
            capability::LIMITS_CHANGE,
            capability::ROUTING_CHANGE,
            capability::ENTERPRISE_FEATURES_CHANGE,
            capability::SPEND_LIMITS_CHANGE,
            capability::RECORDING_DELETE,
            capability::RETENTION_HOLD_PLACE,
            capability::RETENTION_HOLD_RELEASE,
            capability::INCIDENT_RECORD,
            capability::WEBHOOK_KEY_RESEAL,
        ] {
            let rest = c.strip_prefix("calling.").unwrap();
            assert!(rest.len() <= 56 && rest.starts_with(|ch: char| ch.is_ascii_lowercase()));
            assert!(rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.".contains(&b)));
        }
    }

    #[test]
    fn expected_versions_are_bounded() {
        assert!(row_version(0).is_err() && row_version(1).is_ok());
        assert!(config_version(-1).is_err() && config_version(0).is_ok());
    }
}

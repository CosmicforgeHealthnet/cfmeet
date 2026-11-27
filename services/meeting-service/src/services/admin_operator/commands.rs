//! Admin bridge COMMANDS (ADR 0068, phases A4–A7; design §6.1–6.2, §9, §17.6).
//!
//! Every command route follows the owner contract agreed with HQ:
//!
//! * caller `cfmeet.admin-bridge` holding the route's permission, naming the HQ staff
//!   member (`X-CF-Operator`) and — every command here being R3 or R4 — the second person
//!   (`X-CF-Approver`, `X-CF-Approval-Ref`, `X-CF-Intent-Digest`; `422 APPROVAL_REQUIRED`
//!   otherwise);
//! * `Idempotency-Key` (UUID) required (`400`); a camelCase body with no unknown fields,
//!   carrying `expectedVersion` (integer ≥ 1) wherever the subject is versioned; the
//!   change happens only at that version (`409 VERSION_CONFLICT`, nothing done);
//! * the change and its record in `meeting_admin_operations` commit together; a repeated
//!   key for the same command and subject answers the current resource without applying
//!   again, the same key for anything else is `409 IDEMPOTENCY_CONFLICT`;
//! * `200 {"status": <UPPER_SNAKE>, "version": "<n>" | null, "resource": {...}}`, where
//!   `status` and `version` are the record's (the readback answers the same) and
//!   `resource` is a small closed object of ids, states and times — never an address, a
//!   name or a reason text;
//! * refusals are definite 4xx with nothing done; nothing after the commit answers 4xx.
//!
//! The new address of `accounts.user.email.change` travels only in the request body: it
//! is never answered, logged or recorded here.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{request::Parts, StatusCode},
    response::Response,
};
use cf_service_auth::{InternalPermission, OperatorAttribution};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, Set, Statement, TransactionTrait,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{authenticate, error, ok, opt_rfc, path_uuid, OperatorState};
use crate::{
    dto::v2::OwnerTransferRequest,
    error::{ApiError, ApiResult, ErrorCode},
    models::users::{self, UserRole, UserStatus},
    services::{
        account_security::{self as security, ActorKind, Outcome, RevokeReason, SecurityEvent},
        actor::ActorContext,
        admin_operations::{self as ledger, subject, BridgeCommand, Record},
        artifacts::ArtifactType,
        unified_messaging::migration::{ChatMigrationState, TransitionRequest},
    },
};

pub const USER_STATUS: &str = "/internal/v1/operator/accounts/users/:user_id/status";
pub const USER_ROLE: &str = "/internal/v1/operator/accounts/users/:user_id/role";
pub const USER_EMAIL: &str = "/internal/v1/operator/accounts/users/:user_id/email";
pub const USER_SESSIONS_REVOKE: &str =
    "/internal/v1/operator/accounts/users/:user_id/sessions/revoke-all";
pub const USER_DEVICE_REVOKE: &str =
    "/internal/v1/operator/accounts/users/:user_id/devices/:device_id/revoke";
pub const USER_MFA_RESET: &str = "/internal/v1/operator/accounts/users/:user_id/mfa/reset";
pub const USER_DELETE: &str = "/internal/v1/operator/accounts/users/:user_id/delete";
pub const USER_EXPORT: &str = "/internal/v1/operator/accounts/users/:user_id/export";
pub const DUPLICATES_ENFORCE: &str = "/internal/v1/operator/accounts/duplicates/enforce";
pub const CONFERENCE_END: &str = "/internal/v1/operator/meetings/conferences/:conference_id/end";
pub const SPACE_ARCHIVE: &str = "/internal/v1/operator/meetings/spaces/:space_id/archive";
pub const SPACE_TRANSFER: &str = "/internal/v1/operator/meetings/spaces/:space_id/owner-transfer";
pub const RECORDING_DELETE: &str =
    "/internal/v1/operator/meetings/conferences/:conference_id/recordings/:artifact_id/delete";
pub const TRANSCRIPT_DELETE: &str =
    "/internal/v1/operator/meetings/conferences/:conference_id/transcripts/:artifact_id/delete";
pub const CHAT_MIGRATION_TRANSITION: &str =
    "/internal/v1/operator/meetings/chat-migration/transitions";
pub const CRITICAL_REROUTE: &str =
    "/internal/v1/operator/meetings/critical-lane/decisions/:decision_id/reroute";

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

trait CommandBody: DeserializeOwned {
    fn expected_version(&self) -> Option<i64> {
        None
    }
    fn reason_code(&self) -> Option<&str>;
}

/// Commands whose subject has no version column.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReasonBody {
    #[serde(default)]
    reason_code: Option<String>,
}

impl CommandBody for ReasonBody {
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
}

/// Commands on a versioned subject without further parameters.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionedBody {
    #[serde(default)]
    expected_version: Option<i64>,
    #[serde(default)]
    reason_code: Option<String>,
}

impl CommandBody for VersionedBody {
    fn expected_version(&self) -> Option<i64> {
        self.expected_version
    }
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatusBody {
    #[serde(default)]
    expected_version: Option<i64>,
    status: String,
    #[serde(default)]
    reason_code: Option<String>,
}

impl CommandBody for StatusBody {
    fn expected_version(&self) -> Option<i64> {
        self.expected_version
    }
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RoleBody {
    #[serde(default)]
    expected_version: Option<i64>,
    role: String,
    #[serde(default)]
    reason_code: Option<String>,
}

impl CommandBody for RoleBody {
    fn expected_version(&self) -> Option<i64> {
        self.expected_version
    }
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
}

/// No `Debug`: the new address is never printed.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EmailBody {
    #[serde(default)]
    expected_version: Option<i64>,
    new_email: String,
    #[serde(default)]
    reason_code: Option<String>,
}

impl CommandBody for EmailBody {
    fn expected_version(&self) -> Option<i64> {
        self.expected_version
    }
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransferBody {
    #[serde(default)]
    expected_version: Option<i64>,
    new_owner_user_id: Uuid,
    #[serde(default = "default_true")]
    retain_previous_owner: bool,
    #[serde(default)]
    reason_code: Option<String>,
}

impl CommandBody for TransferBody {
    fn expected_version(&self) -> Option<i64> {
        self.expected_version
    }
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatBody {
    #[serde(default)]
    expected_version: Option<i64>,
    target_state: ChatMigrationState,
    #[serde(default)]
    reason_code: Option<String>,
}

impl CommandBody for ChatBody {
    fn expected_version(&self) -> Option<i64> {
        self.expected_version
    }
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
}

/// `reasonCode`: UPPER_SNAKE, 2–64 characters (audited; free text is never accepted).
pub fn valid_reason_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=64).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

// ---------------------------------------------------------------------------
// The command envelope
// ---------------------------------------------------------------------------

/// A parsed, authorized bridge command.
struct Accepted<B> {
    rid: String,
    cmd: BridgeCommand,
    body: B,
}

impl<B: CommandBody> Accepted<B> {
    fn reason(&self) -> &str {
        self.body.reason_code().unwrap_or("OPERATOR_REQUEST")
    }
}

fn refuse(status: StatusCode, code: &str, message: &str, rid: &str) -> Box<Response> {
    Box::new(error(status, code, message, rid))
}

/// Authentication, the key, the body, the expected version and the second person.
async fn accept<B: CommandBody>(
    s: &OperatorState,
    req: &Parts,
    raw: &Bytes,
    permission: InternalPermission,
    command: &'static str,
    versioned: bool,
) -> Result<Accepted<B>, Box<Response>> {
    let ctx = authenticate(s, req, permission).await?;
    let rid = ctx.request_id().as_str().to_string();
    let Ok(Some(attribution)) =
        OperatorAttribution::from_headers(ctx.authenticated_service(), &req.headers)
    else {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "OPERATOR_REQUIRED",
            "A valid X-CF-Operator from the admin bridge is required.",
            &rid,
        ));
    };
    let key = req
        .headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v.trim()).ok())
        .ok_or_else(|| {
            refuse(
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                "Idempotency-Key (a UUID) is required.",
                &rid,
            )
        })?;
    let raw: &[u8] = if raw.iter().all(u8::is_ascii_whitespace) {
        &b"{}"[..]
    } else {
        &raw[..]
    };
    let body: B = serde_json::from_slice(raw).map_err(|_| {
        refuse(
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "The request body is invalid.",
            &rid,
        )
    })?;
    if body.reason_code().is_some_and(|c| !valid_reason_code(c)) {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "reasonCode must be UPPER_SNAKE (2-64 characters).",
            &rid,
        ));
    }
    let expected_version = body.expected_version();
    if versioned && expected_version.is_none_or(|v| v < 1) {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "expectedVersion (an integer of at least 1) is required.",
            &rid,
        ));
    }
    // Every command served here is R3 or R4: two people decide it.
    let Some(approval) = attribution.approval.as_ref() else {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "APPROVAL_REQUIRED",
            "This command needs a second person's approval (X-CF-Approver).",
            &rid,
        ));
    };
    let linked_user = match req.headers.get("x-cf-operator-user-id") {
        None => None,
        Some(v) => Some(
            v.to_str()
                .ok()
                .and_then(|v| Uuid::parse_str(v.trim()).ok())
                .ok_or_else(|| {
                    refuse(
                        StatusCode::BAD_REQUEST,
                        "VALIDATION_ERROR",
                        "x-cf-operator-user-id must be a UUID.",
                        &rid,
                    )
                })?,
        ),
    };
    Ok(Accepted {
        rid,
        cmd: BridgeCommand {
            key,
            command,
            operator: attribution.operator.as_str().to_string(),
            approval: Some((
                approval.approver.as_str().to_string(),
                approval.approval_ref.clone(),
            )),
            expected_version: if versioned { expected_version } else { None },
            linked_user,
        },
        body,
    })
}

/// What a command acts on: its record subject and its current resource.
#[derive(Debug, Clone, Copy)]
enum Subject {
    User(Uuid),
    Device {
        user: Uuid,
        device: Uuid,
    },
    Identity,
    Conference(Uuid),
    Space(Uuid),
    Artifact {
        conference: Uuid,
        artifact: Uuid,
    },
    ChatMigration,
    Decision(Uuid),
    /// `accounts.user.export`: recorded on the account; the resource is the export recorded
    /// under the command's key.
    Export {
        user: Uuid,
        key: Uuid,
    },
}

impl Subject {
    fn kind(self) -> &'static str {
        match self {
            Subject::User(_) | Subject::Export { .. } => subject::USER,
            Subject::Device { .. } => subject::ACCOUNT_DEVICE,
            Subject::Identity => subject::IDENTITY_UNIQUENESS,
            Subject::Conference(_) => subject::CONFERENCE,
            Subject::Space(_) => subject::MEETING_SPACE,
            Subject::Artifact { .. } => subject::ARTIFACT,
            Subject::ChatMigration => subject::CHAT_MIGRATION,
            Subject::Decision(_) => subject::CRITICAL_DECISION,
        }
    }

    fn id(self) -> Uuid {
        match self {
            Subject::User(id)
            | Subject::Conference(id)
            | Subject::Space(id)
            | Subject::Decision(id) => id,
            Subject::Device { device, .. } => device,
            Subject::Export { user, .. } => user,
            Subject::Artifact { artifact, .. } => artifact,
            Subject::Identity | Subject::ChatMigration => Uuid::nil(),
        }
    }

    /// The current resource (ids, states and times only).
    async fn resource(self, s: &OperatorState) -> ApiResult<Value> {
        let db = &s.app.db;
        let one = |sql: &str, values: Vec<sea_orm::Value>| {
            Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
        };
        Ok(match self {
            Subject::User(id) => {
                let r = db
                    .query_one(one(
                        "SELECT status::text AS status, role::text AS role, \
                                (email_verified_at IS NOT NULL OR auth_type::text = 'google') AS verified, \
                                (SELECT count(*) FROM account_sessions s WHERE s.user_id = u.id \
                                   AND s.revoked_at IS NULL AND s.absolute_expires_at > now()) AS open_sessions, \
                                EXISTS (SELECT 1 FROM account_mfa_totp t WHERE t.user_id = u.id \
                                   AND t.confirmed_at IS NOT NULL) AS mfa \
                           FROM users u WHERE u.id = $1",
                        vec![id.into()],
                    ))
                    .await?
                    .ok_or_else(|| ApiError::InternalError("account vanished".into()))?;
                json!({
                    "userId": id,
                    "accountStatus": ledger::state_code(&r.try_get::<String>("", "status")?),
                    "role": ledger::state_code(&r.try_get::<String>("", "role")?),
                    "emailVerified": r.try_get::<bool>("", "verified")?,
                    "openSessions": r.try_get::<i64>("", "open_sessions")?,
                    "mfaEnabled": r.try_get::<bool>("", "mfa")?,
                })
            }
            Subject::Device { user, device } => {
                let d = crate::services::account_devices::operator_device_state(db, user, device)
                    .await?
                    .ok_or_else(|| ApiError::InternalError("device vanished".into()))?;
                json!({
                    "userId": user,
                    "deviceId": d.device_id,
                    "state": d.state,
                    "revokedAt": opt_rfc(d.revoked_at),
                    "revokeReason": d.revoke_reason,
                })
            }
            Subject::Identity => {
                serde_json::to_value(security::identity_report(db).await?).unwrap_or(Value::Null)
            }
            Subject::Conference(id) => {
                let r = db
                    .query_one(one(
                        "SELECT meeting_space_id, status::text AS status, \
                                termination_reason::text AS reason, ending_requested_at, ended_at \
                           FROM conference_sessions WHERE id = $1",
                        vec![id.into()],
                    ))
                    .await?
                    .ok_or_else(|| ApiError::InternalError("conference vanished".into()))?;
                json!({
                    "conferenceId": id,
                    "spaceId": r.try_get::<Uuid>("", "meeting_space_id")?,
                    "conferenceStatus": r.try_get::<String>("", "status")?,
                    "terminationReason": r.try_get::<Option<String>>("", "reason")?,
                    "endingRequestedAt": opt_rfc(r.try_get("", "ending_requested_at")?),
                    "endedAt": opt_rfc(r.try_get("", "ended_at")?),
                })
            }
            Subject::Space(id) => {
                let r = db
                    .query_one(one(
                        "SELECT status::text AS status, owner_user_id, archived_at \
                           FROM meeting_spaces WHERE id = $1",
                        vec![id.into()],
                    ))
                    .await?
                    .ok_or_else(|| ApiError::InternalError("space vanished".into()))?;
                json!({
                    "spaceId": id,
                    "spaceStatus": r.try_get::<String>("", "status")?,
                    "ownerUserId": r.try_get::<Option<Uuid>>("", "owner_user_id")?,
                    "archivedAt": opt_rfc(r.try_get("", "archived_at")?),
                })
            }
            Subject::Artifact {
                conference,
                artifact,
            } => {
                let r = db
                    .query_one(one(
                        "SELECT artifact_type, status, delete_requested_at, deleted_at \
                           FROM meeting_artifacts WHERE id = $1 AND conference_id = $2",
                        vec![artifact.into(), conference.into()],
                    ))
                    .await?
                    .ok_or_else(|| ApiError::InternalError("artifact vanished".into()))?;
                json!({
                    "artifactId": artifact,
                    "conferenceId": conference,
                    "artifactType": r.try_get::<String>("", "artifact_type")?,
                    "artifactStatus": r.try_get::<String>("", "status")?,
                    "deleteRequestedAt": opt_rfc(r.try_get("", "delete_requested_at")?),
                    "deletedAt": opt_rfc(r.try_get("", "deleted_at")?),
                })
            }
            Subject::ChatMigration => {
                let r = db
                    .query_one(one(
                        "SELECT state, version::bigint AS version, changed_at \
                           FROM conference_chat_migration WHERE id = 1",
                        vec![],
                    ))
                    .await?
                    .ok_or_else(|| {
                        ApiError::InternalError("chat migration state missing".into())
                    })?;
                json!({
                    "migrationState": r.try_get::<String>("", "state")?,
                    "migrationVersion": r.try_get::<i64>("", "version")?,
                    "changedAt": opt_rfc(r.try_get("", "changed_at")?),
                })
            }
            Subject::Export { key, .. } => {
                crate::services::account_export::resource_by_key(db, key)
                    .await?
                    .ok_or_else(|| ApiError::InternalError("export vanished".into()))?
            }
            Subject::Decision(id) => {
                let r = db
                    .query_one(one(
                        "SELECT category, route, reason, decision_version::bigint AS version \
                           FROM notification_route_decisions WHERE decision_id = $1",
                        vec![id.into()],
                    ))
                    .await?
                    .ok_or_else(|| ApiError::InternalError("decision vanished".into()))?;
                json!({
                    "decisionId": id,
                    "category": r.try_get::<String>("", "category")?,
                    "route": r.try_get::<String>("", "route")?,
                    "reason": r.try_get::<String>("", "reason")?,
                })
            }
        })
    }
}

/// The definite refusal of a command that changed nothing. 409 is reserved for
/// `VERSION_CONFLICT` and `IDEMPOTENCY_CONFLICT`; every other conflict of the domain is a
/// 422 with its own code. Messages are fixed or server-authored: never a database error
/// text (which could quote a value such as an address).
pub fn refusal(e: &ApiError, rid: &str) -> Response {
    let (status, code, message): (StatusCode, &str, String) = match e {
        ApiError::Domain { code, message } => match code {
            ErrorCode::VersionConflict | ErrorCode::MeetingVersionConflict => (
                StatusCode::CONFLICT,
                "VERSION_CONFLICT",
                "The resource changed; re-read it and retry.".into(),
            ),
            ErrorCode::IdempotencyConflict => (
                StatusCode::CONFLICT,
                "IDEMPOTENCY_CONFLICT",
                "This Idempotency-Key was already used for another command.".into(),
            ),
            ErrorCode::FeatureDisabled => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "FEATURE_DISABLED",
                "The feature is not enabled in this deployment.".into(),
            ),
            ErrorCode::ServiceUnavailable | ErrorCode::DatabaseError => (
                StatusCode::SERVICE_UNAVAILABLE,
                "UNAVAILABLE",
                "The service is temporarily unavailable.".into(),
            ),
            c => {
                let st = c.status();
                let st = if st == StatusCode::CONFLICT {
                    StatusCode::UNPROCESSABLE_ENTITY
                } else if st.is_client_error() || st.is_server_error() {
                    st
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (st, c.as_str(), message.chars().take(256).collect())
            }
        },
        ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "NOT_FOUND", "Not found.".into()),
        ApiError::BadRequest(_) | ApiError::ValidationError(_) | ApiError::InvalidFields { .. } => {
            (
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                "The request is invalid.".into(),
            )
        }
        ApiError::Conflict(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "STATE_CONFLICT",
            "The resource's state does not allow this command.".into(),
        ),
        ApiError::Forbidden(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "NOT_ALLOWED",
            "This command is not allowed on this resource.".into(),
        ),
        ApiError::DatabaseError(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "UNAVAILABLE",
            "The service is temporarily unavailable.".into(),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Internal error.".into(),
        ),
    };
    tracing::warn!(
        code,
        status = status.as_u16(),
        "admin bridge command refused"
    );
    error(status, code, &message, rid)
}

/// The answer of an applied command, from its record.
fn answer(record: &Record, resource: Value) -> Response {
    ok(json!({
        "status": record.resulting_state,
        "version": record.resulting_version.map(|v| v.to_string()),
        "resource": resource,
    }))
}

/// Settles a command: applied now, or earlier under the same key (the record answers),
/// or refused.
async fn settle(
    s: &OperatorState,
    rid: &str,
    cmd: &BridgeCommand,
    subject: Subject,
    outcome: ApiResult<()>,
) -> Response {
    let replay = matches!(
        &outcome,
        Err(e) if e.code() == Some(ErrorCode::IdempotencyConflict)
    );
    if let Err(e) = &outcome {
        if !replay {
            return refusal(e, rid);
        }
    }
    let record = match ledger::find(&s.app.db, cmd.key).await {
        Ok(Some(r)) => r,
        Ok(None) if replay => return refusal(&ledger::conflict_key(), rid),
        // Applied but unreadable right now: an unknown outcome, settled by the readback.
        _ => return super::unavailable(rid),
    };
    if record.command != cmd.command
        || record.subject_kind != subject.kind()
        || record.subject_id != subject.id()
    {
        return refusal(&ledger::conflict_key(), rid);
    }
    match subject.resource(s).await {
        Ok(resource) => answer(&record, resource),
        Err(_) => super::unavailable(rid),
    }
}

/// A key already recorded: answered from the record (same command and subject) or a
/// conflict, before anything is attempted.
async fn replayed(
    s: &OperatorState,
    rid: &str,
    cmd: &BridgeCommand,
    subject: Subject,
) -> Option<Response> {
    match ledger::find(&s.app.db, cmd.key).await {
        Ok(Some(_)) => Some(settle(s, rid, cmd, subject, Err(ledger::conflict_key())).await),
        Ok(None) => None,
        Err(_) => Some(super::unavailable(rid)),
    }
}

macro_rules! accept {
    ($s:expr, $req:expr, $raw:expr, $perm:expr, $command:expr, $versioned:expr) => {
        match accept(&$s, &$req, &$raw, $perm, $command, $versioned).await {
            Ok(a) => a,
            Err(r) => return *r,
        }
    };
}

macro_rules! path {
    ($raw:expr, $rid:expr) => {
        match path_uuid(&$raw, &$rid) {
            Ok(v) => v,
            Err(r) => return *r,
        }
    };
}

macro_rules! replay_or_go {
    ($s:expr, $a:expr, $subject:expr) => {
        if let Some(r) = replayed(&$s, &$a.rid, &$a.cmd, $subject).await {
            return r;
        }
    };
}

/// No staff member administers their own CF Meet account through HQ.
fn not_self(cmd: &BridgeCommand, user_id: Uuid, rid: &str) -> Result<(), Box<Response>> {
    if cmd.linked_user == Some(user_id) {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "SELF_ACTION_REFUSED",
            "Staff cannot administer their own account.",
            rid,
        ));
    }
    Ok(())
}

macro_rules! not_self {
    ($a:expr, $user:expr) => {
        if let Err(r) = not_self(&$a.cmd, $user, &$a.rid) {
            return *r;
        }
    };
}

// ---------------------------------------------------------------------------
// Accounts (account.administer)
// ---------------------------------------------------------------------------

enum AccountChange {
    Status(UserStatus),
    Role(UserRole),
}

fn parse_status(v: &str) -> Option<UserStatus> {
    match v {
        "ACTIVE" => Some(UserStatus::Active),
        "INACTIVE" => Some(UserStatus::Inactive),
        "SUSPENDED" => Some(UserStatus::Suspended),
        _ => None,
    }
}

fn parse_role(v: &str) -> Option<UserRole> {
    match v {
        "USER" => Some(UserRole::User),
        "ADMIN" => Some(UserRole::Admin),
        "GUEST" => Some(UserRole::Guest),
        _ => None,
    }
}

fn role_str(r: &UserRole) -> &'static str {
    match r {
        UserRole::User => "user",
        UserRole::Admin => "admin",
        UserRole::Guest => "guest",
    }
}

/// Status or role change: the console's rules (test accounts never administer; a disabled
/// account loses every credential everywhere) at HQ's expected `account_version`.
async fn change_account(
    s: &OperatorState,
    cmd: &BridgeCommand,
    user_id: Uuid,
    change: AccountChange,
) -> ApiResult<()> {
    let txn = s.app.db.begin().await?;
    ledger::claim(&txn, cmd).await?;
    let version = ledger::account_version(&txn, user_id, true)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    ledger::check_version(cmd, version)?;
    let user = users::Entity::find_by_id(user_id)
        .one(&txn)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    let (previous_status, previous_role) = (user.status.clone(), user.role.clone());
    if matches!(change, AccountChange::Role(UserRole::Admin))
        && s.app.auth_policy.test_accounts.is_test_account(&user.email)
    {
        return Err(ApiError::Forbidden(
            "Test accounts cannot be administrators".into(),
        ));
    }
    let mut active: users::ActiveModel = user.into();
    match &change {
        AccountChange::Status(st) => active.status = Set(st.clone()),
        AccountChange::Role(r) => active.role = Set(r.clone()),
    }
    active.updated_at = Set(crate::utils::now_utc());
    let user = active.update(&txn).await?;
    let attribution = json!({ "operator": cmd.operator(), "approver": cmd.approver() });
    if user.status != previous_status {
        security::record_event(
            &txn,
            SecurityEvent::new("ACCOUNT_STATUS_CHANGED", Outcome::Success)
                .user(user_id)
                .actor(ActorKind::Admin, None)
                .detail(json!({
                    "from": previous_status.as_contract_str(),
                    "to": user.status.as_contract_str(),
                    "reactivated": matches!(previous_status, UserStatus::Inactive | UserStatus::Suspended)
                        && !matches!(user.status, UserStatus::Inactive | UserStatus::Suspended),
                    "by": attribution,
                })),
        )
        .await?;
    }
    if user.role != previous_role {
        security::record_event(
            &txn,
            SecurityEvent::new("ACCOUNT_ROLE_CHANGED", Outcome::Success)
                .user(user_id)
                .actor(ActorKind::Admin, None)
                .detail(json!({
                    "from": role_str(&previous_role), "to": role_str(&user.role), "by": attribution,
                })),
        )
        .await?;
    }
    let state = match change {
        AccountChange::Status(_) => ledger::state_code(user.status.as_contract_str()),
        AccountChange::Role(_) => ledger::state_code(role_str(&user.role)),
    };
    let version = ledger::account_version(&txn, user_id, false).await?;
    ledger::record(&txn, cmd, subject::USER, user_id, &state, version).await?;
    txn.commit().await?;
    // Section 14 (ACC-010): a disabled account loses every credential everywhere; any
    // status change is mirrored onto its Platform Access principal (durable outbox).
    if user.status != previous_status {
        if matches!(user.status, UserStatus::Inactive | UserStatus::Suspended) {
            crate::services::communications::credentials_ended(&s.app, user_id, "ACCOUNT_DISABLED")
                .await;
        } else if let Err(e) = crate::services::communications::deliver_due(&s.app).await {
            tracing::warn!(%user_id, "account status mirror deferred to the relay: {e}");
        }
    }
    Ok(())
}

/// `POST …/accounts/users/{userId}/status` (`accounts.user.status.change`, R3).
pub(super) async fn user_status(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<StatusBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.user.status.change",
        true
    );
    let user_id = path!(id, a.rid);
    let Some(status) = parse_status(&a.body.status) else {
        return error(
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "status must be ACTIVE, INACTIVE or SUSPENDED.",
            &a.rid,
        );
    };
    not_self!(a, user_id);
    let subject = Subject::User(user_id);
    replay_or_go!(s, a, subject);
    let outcome = change_account(&s, &a.cmd, user_id, AccountChange::Status(status)).await;
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/users/{userId}/role` (`accounts.user.role.change`, R3).
pub(super) async fn user_role(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<RoleBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.user.role.change",
        true
    );
    let user_id = path!(id, a.rid);
    let Some(role) = parse_role(&a.body.role) else {
        return error(
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "role must be USER, ADMIN or GUEST.",
            &a.rid,
        );
    };
    not_self!(a, user_id);
    let subject = Subject::User(user_id);
    replay_or_go!(s, a, subject);
    let outcome = change_account(&s, &a.cmd, user_id, AccountChange::Role(role)).await;
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/users/{userId}/email` (`accounts.user.email.change`, R3): the
/// support-assisted change (every session and device ends, both addresses are notified).
pub(super) async fn user_email(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<EmailBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.user.email.change",
        true
    );
    let user_id = path!(id, a.rid);
    not_self!(a, user_id);
    let subject = Subject::User(user_id);
    replay_or_go!(s, a, subject);
    let reason = a.reason().to_string();
    let outcome = crate::services::email_change::admin_change_with(
        &s.app,
        None,
        user_id,
        &a.body.new_email,
        &reason,
        Some(&a.cmd),
    )
    .await
    .map(|_| ());
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/users/{userId}/sessions/revoke-all` (`accounts.sessions.revoke-all`,
/// R3): every sign-in and every credential issued before now ends (Hub, Messaging and
/// Calling with them); the account stays usable.
pub(super) async fn user_sessions_revoke(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<ReasonBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.sessions.revoke-all",
        false
    );
    let user_id = path!(id, a.rid);
    not_self!(a, user_id);
    let subject = Subject::User(user_id);
    replay_or_go!(s, a, subject);
    let reason = a.reason().to_string();
    let outcome = async {
        let txn = s.app.db.begin().await?;
        ledger::claim(&txn, &a.cmd).await?;
        if !security::user_exists(&txn, user_id).await? {
            return Err(ApiError::NotFound("User not found".into()));
        }
        let ended = security::revoke_all(&txn, user_id, RevokeReason::AdminRevoked).await?;
        security::record_event(
            &txn,
            SecurityEvent::new("ADMIN_SESSIONS_REVOKED", Outcome::Success)
                .user(user_id)
                .actor(ActorKind::Admin, None)
                .detail(json!({
                    "targetUserId": user_id, "sessionsEnded": ended, "reason": reason,
                    "operator": a.cmd.operator(), "approver": a.cmd.approver(),
                })),
        )
        .await?;
        ledger::record(&txn, &a.cmd, subject::USER, user_id, "REVOKED", None).await?;
        txn.commit().await?;
        crate::services::communications::credentials_ended(&s.app, user_id, "ADMIN_REVOKED").await;
        Ok(())
    }
    .await;
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/users/{userId}/devices/{deviceId}/revoke` (`accounts.device.revoke`,
/// R3).
pub(super) async fn user_device_revoke(
    State(s): State<OperatorState>,
    Path((id, device)): Path<(String, String)>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<ReasonBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.device.revoke",
        false
    );
    let user_id = path!(id, a.rid);
    let device_id = path!(device, a.rid);
    not_self!(a, user_id);
    let subject = Subject::Device {
        user: user_id,
        device: device_id,
    };
    replay_or_go!(s, a, subject);
    let outcome = crate::services::account_devices::operator_revoke_device(
        &s.app, user_id, device_id, &a.cmd,
    )
    .await;
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/users/{userId}/mfa/reset` (`accounts.mfa.reset`, R3): the
/// authenticator and every recovery code are removed (as the owner's own "turn off"); the
/// owner gets the security email.
pub(super) async fn user_mfa_reset(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<ReasonBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.mfa.reset",
        false
    );
    let user_id = path!(id, a.rid);
    not_self!(a, user_id);
    let subject = Subject::User(user_id);
    replay_or_go!(s, a, subject);
    let reason = a.reason().to_string();
    let outcome = async {
        let txn = s.app.db.begin().await?;
        ledger::claim(&txn, &a.cmd).await?;
        if !security::user_exists(&txn, user_id).await? {
            return Err(ApiError::NotFound("User not found".into()));
        }
        let was_enabled = crate::services::mfa::enabled(&txn, user_id).await?;
        crate::services::mfa::disable(&txn, user_id).await?;
        security::record_event(
            &txn,
            SecurityEvent::new("MFA_RESET_BY_ADMIN", Outcome::Success)
                .user(user_id)
                .actor(ActorKind::Admin, None)
                .detail(json!({
                    "wasEnabled": was_enabled, "reason": reason,
                    "operator": a.cmd.operator(), "approver": a.cmd.approver(),
                })),
        )
        .await?;
        ledger::record(&txn, &a.cmd, subject::USER, user_id, "NOT_ENROLLED", None).await?;
        txn.commit().await?;
        security::notify_security(
            &s.app,
            user_id,
            crate::services::email::SecurityNotice::MfaDisabled,
            &a.cmd.key.to_string(),
        )
        .await;
        Ok(())
    }
    .await;
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/users/{userId}/delete` (`accounts.user.delete`, R4): the account
/// deletion every owner gets (`DELETE /me/account`), relayed by HQ.
pub(super) async fn user_delete(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<VersionedBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.user.delete",
        true
    );
    let user_id = path!(id, a.rid);
    not_self!(a, user_id);
    let subject = Subject::User(user_id);
    replay_or_go!(s, a, subject);
    let outcome =
        crate::services::account_erasure::erase_with(&s.app, user_id, None, Some(&a.cmd)).await;
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/users/{userId}/export` (`accounts.user.export`, R4): the owner's
/// personal-data export, OWNER-DELIVERED (design brief 2 §1): the Meeting Service builds the
/// document `GET /api/v2/me/data-export` answers, keeps it only sealed (7 days at most) and
/// emails the owner's verified address a single-use link that needs the owner's own sign-in.
/// The answer is a reference only (`REQUESTED`; the resource's `state` follows the build:
/// READY once the link is queued, DELIVERED once downloaded, EXPIRED, FAILED).
pub(super) async fn user_export(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<VersionedBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.user.export",
        true
    );
    let user_id = path!(id, a.rid);
    not_self!(a, user_id);
    let subject = Subject::Export {
        user: user_id,
        key: a.cmd.key,
    };
    replay_or_go!(s, a, subject);
    let outcome = crate::services::account_export::request(&s.app, user_id, &a.cmd)
        .await
        .map(|_| ());
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/accounts/duplicates/enforce` (`accounts.duplicates.enforce`, R3): creates each
/// case-insensitive unique index whose field has no case-only duplicates left
/// (`ENFORCED`, `PARTIALLY_ENFORCED` or `NOT_ENFORCED` afterwards).
pub(super) async fn duplicates_enforce(
    State(s): State<OperatorState>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<ReasonBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::AccountAdminister,
        "accounts.duplicates.enforce",
        false
    );
    let subject = Subject::Identity;
    replay_or_go!(s, a, subject);
    let outcome = async {
        let txn = s.app.db.begin().await?;
        ledger::claim(&txn, &a.cmd).await?;
        security::enforce_identity_uniqueness(&txn).await?;
        let report = security::identity_report(&txn).await?;
        let state = match (report.email.enforced, report.username.enforced) {
            (true, true) => "ENFORCED",
            (false, false) => "NOT_ENFORCED",
            _ => "PARTIALLY_ENFORCED",
        };
        ledger::record(
            &txn,
            &a.cmd,
            subject::IDENTITY_UNIQUENESS,
            Uuid::nil(),
            state,
            None,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }
    .await;
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

// ---------------------------------------------------------------------------
// Meetings (meeting.administer)
// ---------------------------------------------------------------------------

/// `POST …/meetings/conferences/{conferenceId}/end` (`meetings.meeting.end`, R3).
pub(super) async fn conference_end(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<VersionedBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::MeetingAdminister,
        "meetings.meeting.end",
        true
    );
    let conference_id = path!(id, a.rid);
    let subject = Subject::Conference(conference_id);
    replay_or_go!(s, a, subject);
    let outcome = s
        .app
        .meeting
        .conferences
        .end_by_operator(&a.cmd, conference_id)
        .await
        .map(|_| ());
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/meetings/spaces/{spaceId}/archive` (`meetings.space.archive`, R3): soft
/// archive; every LIVE conference of the space ends with it.
pub(super) async fn space_archive(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<VersionedBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::MeetingAdminister,
        "meetings.space.archive",
        true
    );
    let space_id = path!(id, a.rid);
    let subject = Subject::Space(space_id);
    replay_or_go!(s, a, subject);
    let outcome = s
        .app
        .meeting
        .spaces
        .archive_with(
            &ActorContext::system("admin-bridge"),
            space_id,
            None,
            Some(&a.cmd),
        )
        .await
        .map(|_| ());
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/meetings/spaces/{spaceId}/owner-transfer` (`meetings.space.transfer`, R3).
pub(super) async fn space_transfer(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<TransferBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::MeetingAdminister,
        "meetings.space.transfer",
        true
    );
    let space_id = path!(id, a.rid);
    let Ok(version) = i32::try_from(a.body.expected_version.unwrap_or_default()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "expectedVersion is out of range.",
            &a.rid,
        );
    };
    let subject = Subject::Space(space_id);
    replay_or_go!(s, a, subject);
    let outcome = s
        .app
        .meeting
        .spaces
        .transfer_owner_with(
            &ActorContext::system("admin-bridge"),
            space_id,
            OwnerTransferRequest {
                new_owner_user_id: a.body.new_owner_user_id,
                version,
                retain_previous_owner: a.body.retain_previous_owner,
            },
            Some(&a.cmd),
        )
        .await
        .map(|_| ());
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

async fn artifact_delete(
    s: OperatorState,
    ids: (String, String),
    req: Parts,
    raw: Bytes,
    command: &'static str,
    kind: ArtifactType,
) -> Response {
    let a: Accepted<VersionedBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::MeetingAdminister,
        command,
        true
    );
    let conference_id = path!(ids.0, a.rid);
    let artifact_id = path!(ids.1, a.rid);
    let subject = Subject::Artifact {
        conference: conference_id,
        artifact: artifact_id,
    };
    replay_or_go!(s, a, subject);
    let outcome = s
        .app
        .meeting
        .artifacts
        .operator_delete(&a.cmd, conference_id, artifact_id, kind)
        .await
        .map(|_| ());
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/conferences/{conferenceId}/recordings/{artifactId}/delete`
/// (`meetings.recording.delete`, R4).
pub(super) async fn recording_delete(
    State(s): State<OperatorState>,
    Path(ids): Path<(String, String)>,
    req: Parts,
    raw: Bytes,
) -> Response {
    artifact_delete(
        s,
        ids,
        req,
        raw,
        "meetings.recording.delete",
        ArtifactType::Recording,
    )
    .await
}

/// `POST …/conferences/{conferenceId}/transcripts/{artifactId}/delete`
/// (`meetings.transcript.delete`, R4).
pub(super) async fn transcript_delete(
    State(s): State<OperatorState>,
    Path(ids): Path<(String, String)>,
    req: Parts,
    raw: Bytes,
) -> Response {
    artifact_delete(
        s,
        ids,
        req,
        raw,
        "meetings.transcript.delete",
        ArtifactType::Transcript,
    )
    .await
}

/// `POST …/meetings/chat-migration/transitions` (`meetings.chat-migration.change`, R3):
/// the gated meeting-chat migration step; HQ's approver is the second operator of a
/// cutover or retirement.
pub(super) async fn chat_migration_transition(
    State(s): State<OperatorState>,
    req: Parts,
    raw: Bytes,
) -> Response {
    let a: Accepted<ChatBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::MeetingAdminister,
        "meetings.chat-migration.change",
        true
    );
    let subject = Subject::ChatMigration;
    replay_or_go!(s, a, subject);
    let outcome = match s.app.meeting.core.unified.as_ref() {
        None => Err(ApiError::domain(
            ErrorCode::FeatureDisabled,
            "Unified Messaging is not enabled",
        )),
        Some(unified) => unified
            .transition_with(
                a.cmd.operator(),
                TransitionRequest {
                    target_state: a.body.target_state,
                    expected_version: a.body.expected_version.unwrap_or_default(),
                    reason: format!("{} {}", a.cmd.operator(), a.reason()),
                    approved_by: None,
                },
                Some(&a.cmd),
            )
            .await
            .map(|_| ()),
    };
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

/// `POST …/meetings/critical-lane/decisions/{decisionId}/reroute`
/// (`meetings.critical-lane.change`, R3): the operator reroute of one CRITICAL decision to
/// the legacy path, at HQ's expected `decision_version`.
pub(super) async fn critical_reroute(
    State(s): State<OperatorState>,
    Path(id): Path<String>,
    req: Parts,
    raw: Bytes,
) -> Response {
    use crate::services::critical_notifications::{self as critical, Failover};
    let a: Accepted<VersionedBody> = accept!(
        s,
        req,
        raw,
        InternalPermission::MeetingAdminister,
        "meetings.critical-lane.change",
        true
    );
    let decision_id = path!(id, a.rid);
    let subject = Subject::Decision(decision_id);
    replay_or_go!(s, a, subject);
    let outcome = match critical::operator_reroute_bridge(
        &s.app.db,
        critical::installed(),
        decision_id,
        chrono::Utc::now(),
        &a.cmd,
    )
    .await
    {
        Ok(Failover::Sent { .. }) => Ok(()),
        Ok(Failover::Unknown) => Err(ApiError::NotFound("No such CRITICAL decision".into())),
        Ok(other) => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                other.as_str(),
                "The decision cannot be rerouted now; nothing changed.",
                &a.rid,
            )
        }
        Err(e) => Err(e),
    };
    settle(&s, &a.rid, &a.cmd, subject, outcome).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_codes_are_upper_snake() {
        assert!(valid_reason_code("SUPPORT_TICKET"));
        assert!(valid_reason_code("FRAUD_2"));
        assert!(!valid_reason_code("x"));
        assert!(!valid_reason_code("lower"));
        assert!(!valid_reason_code("_LEADING"));
        assert!(!valid_reason_code("A B"));
        assert!(!valid_reason_code(&"A".repeat(65)));
    }

    #[test]
    fn only_version_and_key_conflicts_answer_409() {
        let r = refusal(
            &ApiError::domain(ErrorCode::ConferenceStateConflict, "x"),
            "r",
        );
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let r = refusal(
            &ApiError::domain(ErrorCode::MeetingVersionConflict, "x"),
            "r",
        );
        assert_eq!(r.status(), StatusCode::CONFLICT);
        let r = refusal(&ApiError::Conflict("taken".into()), "r");
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let r = refusal(&ApiError::NotFound("x".into()), "r");
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn statuses_and_roles_parse_strictly() {
        assert_eq!(parse_status("SUSPENDED"), Some(UserStatus::Suspended));
        assert_eq!(parse_status("PENDING_VERIFICATION"), None);
        assert_eq!(parse_status("active"), None);
        assert_eq!(parse_role("ADMIN"), Some(UserRole::Admin));
        assert_eq!(parse_role("OWNER"), None);
    }

    #[test]
    fn unknown_body_fields_are_refused() {
        assert!(serde_json::from_str::<ReasonBody>(r#"{"expectedVersion":1}"#).is_err());
        assert!(serde_json::from_str::<VersionedBody>(r#"{"expectedVersion":1}"#).is_ok());
        assert!(serde_json::from_str::<StatusBody>(r#"{"status":"ACTIVE","note":"x"}"#).is_err());
    }
}

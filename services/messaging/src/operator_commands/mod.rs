//! The admin bridge's operator COMMANDS on CF Meet chats (ADR 0068, design §6.3 and
//! §17.6; phases A4 / A5): `POST /internal/v1/operator/*`, plus the settlement read
//! `GET /internal/v1/operator/operations/{idempotencyKey}`.
//!
//! Every command follows one owner contract:
//!
//! * only `cfmeet.admin-bridge` holding `messaging-firstparty.administer`, with a named HQ
//!   operator (`X-CF-Operator: hq:<staff>`, verified by the internal-auth middleware);
//!   anyone else is 403 `INTERNAL_PERMISSION_DENIED` before anything is read;
//! * an `Idempotency-Key` (UUID) is required (400 `IDEMPOTENCY_KEY_REQUIRED` /
//!   `VALIDATION_ERROR`); R3 / R4 commands also require the second person
//!   (`X-CF-Approver`, `X-CF-Approval-Ref`, `X-CF-Intent-Digest`; 403
//!   `INTERNAL_PERMISSION_DENIED` / `SECOND_PERSON_REQUIRED` otherwise);
//! * the camelCase body is closed (`deny_unknown_fields`) and names the tenant
//!   (`tenantId`, re-checked against the resource: another tenant's resource is 404) and,
//!   where the resource has a row version, `expectedVersion`: the change happens only at
//!   that version, under a row lock (409 `VERSION_CONFLICT` otherwise);
//! * the change and its row in `messaging_admin_operations` commit in ONE transaction; a
//!   repeated key for the same command and subject answers what the first call answered
//!   and applies nothing; the same key for anything else is 409 `CONFLICT` /
//!   `IDEMPOTENCY_KEY_REUSED`;
//! * the answer is `{status, version, resource}`: an UPPER_SNAKE resulting state, the
//!   resulting row version as a string (null when the resource has none) and a small
//!   closed object of ids, states and times;
//! * refusals are definite 4xx and change nothing (a refusal the console sees as 503
//!   `FEATURE_DISABLED`, e.g. an engaged kill switch, is 409 `CONFLICT` with the same
//!   reason here); a 5xx is a database failure the bridge settles from the record.
//!
//! The bridge never reads end-to-end encrypted content: no command reads or returns a
//! message body, ciphertext, attachment, key or report evidence; moderation acts on ids
//! (a removed message is a server tombstone, the stored ciphertext / text is purged later
//! unless held).

pub mod channels;
pub mod compliance;
pub mod moderation;
pub mod operations;
pub mod policy;

use std::collections::BTreeSet;

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    identity::{EnvironmentKind, OwnerScope, TrustedContext},
    operator_reads::require_bridge,
    state::AppState,
};

/// The credential id recorded on owner rows the admin bridge creates or changes (moderation
/// actions, resolved reports, holds, policies, deletion requests). It is no Platform Access
/// credential; the HQ staff member is in `messaging_admin_operations`.
pub const ADMIN_BRIDGE_CREDENTIAL_ID: Uuid =
    Uuid::from_u128(0x0000_0000_0000_4000_8000_0068_ad31_b41d);
/// The operator name on `messaging_operator_audit`, kill switches and incidents.
pub const ADMIN_BRIDGE: &str = "cfmeet.admin-bridge";

/// Routes nested under `/internal/v1` (`POST /operator/holds` and
/// `POST /operator/deletion-requests` share their path with a read and are registered in
/// [`crate::operator_reads::routes`]).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/operator/operations/:idempotencyKey",
            get(operation_record),
        )
        .route(
            "/operator/conversations/:conversationId",
            get(moderation::conversation),
        )
        .route(
            "/operator/conversations/:conversationId/members/:principalId",
            get(moderation::member),
        )
        .route(
            "/operator/conversations/:conversationId/restrict",
            post(moderation::restrict_conversation),
        )
        .route(
            "/operator/conversations/:conversationId/members/:principalId/mute",
            post(moderation::mute_member),
        )
        .route(
            "/operator/conversations/:conversationId/members/:principalId/remove",
            post(moderation::remove_member),
        )
        .route(
            "/operator/reports/:reportId/remove-message",
            post(moderation::remove_message),
        )
        .route(
            "/operator/reports/:reportId/dismiss",
            post(moderation::dismiss_report),
        )
        .route("/operator/policy", get(policy::read).post(policy::change))
        .route(
            "/operator/holds/:holdId/release",
            post(compliance::release_hold),
        )
        .route(
            "/operator/switches/:switchName",
            post(operations::change_switch),
        )
        .route(
            "/operator/incidents/:incidentId/resolve",
            post(operations::resolve_incident),
        )
        .route(
            "/operator/failed-messages/:eventId/retry",
            post(operations::retry_failed_message),
        )
        .route(
            "/operator/key-transparency/checkpoint",
            post(operations::checkpoint),
        )
        // ADR 0072: channel verification and moderation.
        .route(
            "/operator/channel-verifications",
            get(channels::verifications),
        )
        .route(
            "/operator/channel-verifications/:applicationId/decide",
            post(channels::decide_verification),
        )
        .route("/operator/channel-reports", get(channels::reports))
        .route(
            "/operator/channel-reports/:reportId/resolve",
            post(channels::resolve_report),
        )
        .route("/operator/channel-actions", get(channels::actions))
        .route("/operator/channels/:channelId", get(channels::channel))
        .route(
            "/operator/channels/:channelId/moderate",
            post(channels::moderate),
        )
        .route(
            "/operator/channels/:channelId/verification/revoke",
            post(channels::revoke_verification),
        )
        .route(
            "/operator/channels/:channelId/posts/:postId/takedown",
            post(channels::take_down),
        )
}

/// One verified bridge command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCommand {
    /// HQ's `Idempotency-Key` (= the operation id).
    pub key: Uuid,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)` of a two-person command.
    pub approval: Option<(String, String)>,
}

impl BridgeCommand {
    /// The bridge with its grant and a named operator (403 otherwise), its
    /// `Idempotency-Key` (400 when missing or not a UUID) and, when `two_person`, the
    /// approval (403 `SECOND_PERSON_REQUIRED`).
    pub fn of(
        caller: &InternalCaller,
        headers: &HeaderMap,
        two_person: bool,
    ) -> Result<Self, ApiError> {
        require_bridge(caller)?;
        let attribution = caller
            .operator()
            .ok_or(Error::code(ErrorCode::InternalPermissionDenied))?;
        let raw = headers
            .get("idempotency-key")
            .ok_or(Error::code(ErrorCode::IdempotencyKeyRequired))?;
        let key = raw
            .to_str()
            .ok()
            .and_then(|v| Uuid::parse_str(v.trim()).ok())
            .ok_or_else(|| Error::validation("Idempotency-Key must be a UUID"))?;
        let approval = attribution
            .approval
            .as_ref()
            .map(|a| (a.approver.as_str().to_string(), a.approval_ref.clone()));
        if two_person && approval.is_none() {
            return Err(Error::reason(
                ErrorCode::InternalPermissionDenied,
                "SECOND_PERSON_REQUIRED",
            ));
        }
        Ok(Self {
            key,
            operator: attribution.operator.as_str().to_string(),
            approval,
        })
    }
}

/// What a command acted on (`subject_kind`, `subject_id` of the record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub kind: &'static str,
    pub id: String,
}

impl Subject {
    pub fn new(kind: &'static str, id: impl ToString) -> Self {
        Self {
            kind,
            id: id.to_string(),
        }
    }
}

/// A command's result: the resulting state, row version and answer resource.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub state: &'static str,
    pub version: Option<i64>,
    pub resource: Value,
}

/// `{status, version, resource}` (the version as a string of the integer, or null).
pub fn answer(state: &str, version: Option<i64>, resource: Value) -> Value {
    json!({
        "status": state,
        "version": version.map(|v| v.to_string()),
        "resource": resource,
    })
}

#[derive(Debug, FromQueryResult)]
struct LedgerRow {
    command: String,
    subject_kind: String,
    subject_id: String,
    resulting_state: String,
    resulting_version: Option<i64>,
    resource: Value,
}

const LEDGER_COLUMNS: &str =
    "command, subject_kind, subject_id, resulting_state, resulting_version, resource";

/// Opens a command in `txn`: serializes every request carrying the same key (until the
/// transaction ends) and answers a key already applied — `Some(answer)` for the same
/// command and subject (nothing is applied again), 409 `IDEMPOTENCY_KEY_REUSED` for
/// anything else.
pub(crate) async fn begin<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: &Subject,
) -> Result<Option<Value>, ApiError> {
    db::exec(
        txn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 68))",
        vec![format!("messaging-admin-operation:{}", cmd.key).into()],
    )
    .await?;
    let row: Option<LedgerRow> = db::one(
        txn,
        &format!(
            "SELECT {LEDGER_COLUMNS} FROM messaging_admin_operations WHERE idempotency_key = $1"
        ),
        vec![cmd.key.into()],
    )
    .await?;
    match row {
        None => Ok(None),
        Some(r)
            if r.command == command
                && r.subject_kind == subject.kind
                && r.subject_id == subject.id =>
        {
            Ok(Some(answer(
                &r.resulting_state,
                r.resulting_version,
                r.resource,
            )))
        }
        Some(_) => Err(Error::reason(ErrorCode::Conflict, "IDEMPOTENCY_KEY_REUSED")),
    }
}

/// Records the applied command in `txn` (the same transaction as its change) with its
/// operator-audit entry, and returns the answer.
pub(crate) async fn finish<C: ConnectionTrait>(
    txn: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: &Subject,
    outcome: Outcome,
    now: DateTime<Utc>,
) -> Result<Value, ApiError> {
    let (approver, approval_ref) = match &cmd.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    let inserted = db::exec(
        txn,
        "INSERT INTO messaging_admin_operations (idempotency_key, command, subject_kind, subject_id, operator_ref, \
           approver_ref, approval_ref, resulting_state, resulting_version, resource, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) ON CONFLICT (idempotency_key) DO NOTHING",
        vec![
            cmd.key.into(),
            command.into(),
            subject.kind.into(),
            subject.id.clone().into(),
            cmd.operator.clone().into(),
            approver.clone().into(),
            approval_ref.clone().into(),
            outcome.state.into(),
            outcome.version.into(),
            outcome.resource.clone().into(),
            now.into(),
        ],
    )
    .await?;
    if inserted != 1 {
        return Err(Error::reason(ErrorCode::Conflict, "IDEMPOTENCY_KEY_REUSED"));
    }
    crate::operations::audit(
        txn,
        ADMIN_BRIDGE,
        "ADMIN_BRIDGE_COMMAND",
        &format!("{command}:{}", subject.id),
        json!({
            "idempotency_key": cmd.key,
            "operator_ref": cmd.operator,
            "approver_ref": approver,
            "approval_ref": approval_ref,
            "resulting_state": outcome.state,
            "resulting_version": outcome.version,
        }),
        now,
    )
    .await?;
    Ok(answer(outcome.state, outcome.version, outcome.resource))
}

/// `GET /operator/operations/{idempotencyKey}`: the bridge's settlement read of an applied
/// command (`outcome` is always APPLIED: a refused command records nothing). 404 for an
/// unknown key and for any caller other than the bridge with a named operator.
async fn operation_record(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if require_bridge(&caller).is_err() {
        return Err(Error::code(ErrorCode::NotFound));
    }
    let key = Uuid::parse_str(&key).map_err(|_| Error::code(ErrorCode::NotFound))?;
    let row: LedgerRow = db::one(
        &state.db,
        &format!(
            "SELECT {LEDGER_COLUMNS} FROM messaging_admin_operations WHERE idempotency_key = $1"
        ),
        vec![key.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(Json(json!({
        "idempotency_key": key,
        "outcome": "APPLIED",
        "source_status": row.resulting_state,
        "source_version": row.resulting_version.map(|v| v.to_string()),
    })))
}

/// The body of a command, or 400 (never echoing the input).
pub(crate) fn body<T>(raw: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    raw.map(|Json(b)| b)
        .map_err(|_| Error::validation("the request body is not a valid command"))
}

/// `expectedVersion` is an integer of at least 1.
pub(crate) fn expected(version: i64) -> Result<i64, ApiError> {
    if version >= 1 {
        Ok(version)
    } else {
        Err(Error::validation(
            "expectedVersion must be an integer of at least 1",
        ))
    }
}

/// `reasonCode` is an UPPER_SNAKE_CASE code of 2-48 characters.
pub(crate) fn reason(code: &str) -> Result<(), ApiError> {
    if crate::membership::valid_reason(code) {
        Ok(())
    } else {
        Err(Error::validation(
            "reasonCode must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ))
    }
}

/// A refusal the console sees as 503 (`FEATURE_DISABLED` / `SERVICE_UNAVAILABLE` with a
/// reason: an engaged kill switch, moderation or exports turned off) is a definite 409
/// for the bridge, which treats every 5xx as an unknown outcome. Database failures stay
/// 503.
pub(crate) fn definite(e: Error) -> Error {
    match e {
        Error::Code {
            code: ErrorCode::FeatureDisabled | ErrorCode::ServiceUnavailable,
            reason,
            message,
        } => Error::Code {
            code: ErrorCode::Conflict,
            reason,
            message,
        },
        other => other,
    }
}

/// An owner scope (tenant, application, environment) of a resource the bridge acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromQueryResult)]
pub(crate) struct ScopeRow {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

impl ScopeRow {
    /// The trusted context the owner's own operations run under for a bridge command:
    /// the resource's scope ids and [`ADMIN_BRIDGE_CREDENTIAL_ID`], no scopes, no principal.
    /// Only the scope ids and the credential id are consulted by the operations the
    /// commands reuse (`environment_kind` and `service_account_id` are placeholders).
    pub fn context(&self) -> TrustedContext {
        TrustedContext {
            scope: OwnerScope {
                tenant_id: self.tenant_id,
                application_id: self.application_id,
                environment_id: self.environment_id,
                environment_kind: EnvironmentKind::Production,
                service_account_id: Uuid::nil(),
            },
            credential_id: ADMIN_BRIDGE_CREDENTIAL_ID,
            personal_session_issued_at: None,
            first_party: false,
            full_authority: true,
            scopes: BTreeSet::new(),
            principal_restriction: None,
            oauth: None,
        }
    }
}

/// The scope of a conversation of `tenant_id` (locked when `lock`), or 404 — also when it
/// belongs to another tenant.
pub(crate) async fn conversation_scope<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    tenant_id: Uuid,
    lock: bool,
) -> Result<ScopeRow, ApiError> {
    let row: Option<ScopeRow> = db::one(
        db,
        &format!(
            "SELECT tenant_id, application_id, environment_id FROM messaging_conversations WHERE id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![conversation_id.into()],
    )
    .await?;
    row.filter(|r| r.tenant_id == tenant_id)
        .ok_or(Error::code(ErrorCode::NotFound))
}

/// The scope of an application environment Messaging knows for `tenant_id` (it has a
/// policy, a baseline or a conversation), or 404.
pub(crate) async fn environment_scope<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    tenant_id: Uuid,
) -> Result<ScopeRow, ApiError> {
    let row: Option<ScopeRow> = db::one(
        db,
        "SELECT tenant_id, application_id, environment_id FROM ( \
           SELECT tenant_id, application_id, environment_id, 1 AS o FROM messaging_application_policies WHERE environment_id = $1 \
           UNION ALL \
           SELECT tenant_id, application_id, environment_id, 2 FROM messaging_application_baselines WHERE environment_id = $1 \
           UNION ALL \
           (SELECT tenant_id, application_id, environment_id, 3 FROM messaging_conversations WHERE environment_id = $1 LIMIT 1) \
         ) s ORDER BY o LIMIT 1",
        vec![environment_id.into()],
    )
    .await?;
    row.filter(|r| r.tenant_id == tenant_id)
        .ok_or(Error::code(ErrorCode::NotFound))
}

/// The current `version` of one row (inside the command's transaction).
pub(crate) async fn version_of<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<i64, ApiError> {
    #[derive(FromQueryResult)]
    struct V {
        version: i64,
    }
    let v: V = db::one(db, sql, values)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(v.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_carry_the_version_as_a_string() {
        assert_eq!(
            answer("REMOVED", Some(3), json!({ "id": 1 })),
            json!({ "status": "REMOVED", "version": "3", "resource": { "id": 1 } })
        );
        assert_eq!(answer("RETRIED", None, json!({}))["version"], Value::Null);
    }

    #[test]
    fn console_refusals_become_definite_conflicts() {
        let e = definite(Error::reason(
            ErrorCode::FeatureDisabled,
            "KILL_SWITCH_ENGAGED",
        ));
        assert_eq!(e.error_code(), ErrorCode::Conflict);
        assert!(e.to_string().contains("KILL_SWITCH_ENGAGED"));
        let e = definite(Error::code(ErrorCode::NotFound));
        assert_eq!(e.error_code(), ErrorCode::NotFound);
        let e = definite(Error::Database {
            summary: "x".into(),
            connectivity: true,
        });
        assert_eq!(e.error_code(), ErrorCode::ServiceUnavailable);
    }

    #[test]
    fn versions_and_reasons_are_validated() {
        assert!(expected(1).is_ok());
        assert!(expected(0).is_err());
        assert!(expected(-4).is_err());
        assert!(reason("TERMS_VIOLATION").is_ok());
        for bad in ["", "x", "lower_case", "WITH SPACE", &"A".repeat(49)] {
            assert!(reason(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_bridge_context_carries_the_resource_scope_and_no_authority() {
        let scope = ScopeRow {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
        };
        let ctx = scope.context();
        assert_eq!(ctx.scope.tenant_id, scope.tenant_id);
        assert_eq!(ctx.scope.environment_id, scope.environment_id);
        assert_eq!(ctx.credential_id, ADMIN_BRIDGE_CREDENTIAL_ID);
        assert!(ctx.scopes.is_empty() && !ctx.first_party);
        assert_eq!(ADMIN_BRIDGE_CREDENTIAL_ID.get_version_num(), 4);
    }
}

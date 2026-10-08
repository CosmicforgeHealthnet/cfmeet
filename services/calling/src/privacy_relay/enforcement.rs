//! Privacy Relay → Calling active-call enforcement
//! (`POST /internal/v1/privacy-relay/enforcement`, contract
//! `cfmeet.privacy-relay.enforcement-command`).
//!
//! Only an authenticated internal caller granted `communication-authority.enforce` by
//! CALLING's trust bundle (the permission registry allows only `cfmeet.privacy-relay`)
//! with a signed delegated platform context may call it; the delegated context is the
//! only scope. The command is RESTRICTIVE ONLY: it can end protected calls, never grant
//! anything.
//!
//! Calling finds its own calls by the opaque references it stored
//! (`communication_context_ref`, `privacy_relay_ref`) — it never queries Privacy Relay —
//! and terminates them through the ordinary call commands: pre-answer calls are
//! CANCELLED; answered / connected / held / transferring calls END (ENDING → provider
//! cleanup → ENDED). Calls whose Privacy Relay authorization is at or after the enforced
//! version already reflect the change and are left alone, so a stale (lower-version)
//! command can never affect newer authority.
//!
//! Deduplicated by `enforcementEventId`: a replay answers the recorded result without
//! touching any call again (no duplicate terminal events, no duplicate provider cleanup).

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    calls::{command::CallCommandContext, errors::CallError, ids::CallId, state::CallState},
    error::CallingError,
    internal_auth::middleware::InternalCaller,
    state::AppState,
};

const REASONS: &[&str] = &[
    "CONTEXT_REVOKED",
    "CONTEXT_SUSPENDED",
    "CONTEXT_COMPLETED",
    "CONTEXT_EXPIRED",
    "PARTICIPANT_REMOVED",
    "SESSION_REVOKED",
    "SESSION_SUSPENDED",
    "SESSION_EXPIRED",
];

/// Internal routes of this module (nested under `/internal/v1`).
pub fn internal_routes() -> Router<AppState> {
    Router::new().route("/privacy-relay/enforcement", post(enforce))
}

/// The enforcement command (ids, versions, reason, flag — nothing else).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnforcementCommand {
    pub enforcement_event_id: Uuid,
    pub context_id: Uuid,
    pub context_version: i64,
    #[serde(default)]
    pub relay_session_ids: Vec<Uuid>,
    #[serde(default)]
    pub relay_session_version: Option<i64>,
    pub reason: String,
    #[serde(default)]
    pub affected_principal_ids: Vec<Uuid>,
    pub terminate_active_calls: bool,
}

#[derive(Debug, FromQueryResult)]
struct Recorded {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    status: String,
    matched_calls: i32,
    terminated_calls: i32,
}

#[derive(Debug, FromQueryResult)]
struct Candidate {
    id: Uuid,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Serializes protected-call authority writes per communication context (shared with
/// protected call creation and transfer, which check the enforcement record under it).
pub(crate) fn context_lock_key(context_id: Uuid) -> String {
    format!("cfmeet.calling.privacy-context:{context_id}")
}

pub(crate) async fn lock_context<C: ConnectionTrait>(
    db: &C,
    context_id: Uuid,
) -> Result<(), sea_orm::DbErr> {
    db.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![context_lock_key(context_id).into()],
    ))
    .await
    .map(|_| ())
}

/// Whether an enforcement newer than a decision (context version / session version)
/// was already received for one of `principals` (empty: any). Called under
/// [`lock_context`] by protected creation / transfer, so an enforcement that committed
/// first is always seen, and one that commits later always sees the new call.
pub(crate) async fn superseded<C: ConnectionTrait>(
    db: &C,
    scope: (Uuid, Uuid, Uuid),
    context_id: Uuid,
    context_version: i64,
    session_id: Uuid,
    session_version: i64,
    principals: &[Uuid],
) -> Result<bool, sea_orm::DbErr> {
    let row = db
        .query_one(stmt(
            "SELECT count(*)::bigint FROM call_privacy_enforcements \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND communication_context_ref = $4 \
               AND ((cardinality(relay_session_refs) = 0 AND context_version > $5 \
                     AND (cardinality(affected_principal_refs) = 0 OR affected_principal_refs && $8)) \
                 OR ($6 = ANY(relay_session_refs) AND COALESCE(session_version, 0) > $7))",
            vec![
                scope.0.into(),
                scope.1.into(),
                scope.2.into(),
                context_id.into(),
                context_version.into(),
                session_id.into(),
                session_version.into(),
                principals.to_vec().into(),
            ],
        ))
        .await?;
    Ok(row
        .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0)
        > 0)
}

fn result_body(event: Uuid, matched: i32, terminated: i32) -> Value {
    json!({
        "status": "APPLIED",
        "enforcementEventId": event,
        "matchedCalls": matched,
        "terminatedCalls": terminated,
    })
}

async fn enforce(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<EnforcementCommand>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    caller.require_permission(InternalPermission::CommunicationAuthorityEnforce)?;
    let cmd = CallCommandContext::internal(caller.context())
        .map_err(|_| CallingError::InternalPermissionDenied)?;
    let Json(command) =
        body.map_err(|_| CallingError::Validation("The enforcement command is invalid."))?;
    if !REASONS.contains(&command.reason.as_str())
        || command.context_version < 1
        || command.relay_session_ids.len() > 16
        || command.affected_principal_ids.len() > 256
        || command.relay_session_version.is_some_and(|v| v < 1)
        || (command.relay_session_ids.is_empty() != command.relay_session_version.is_none())
    {
        return Err(CallingError::Validation(
            "The enforcement command is invalid.",
        ));
    }
    let scope = cmd.scope();
    let owner = (
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
    );
    let db = &state.db;
    let now: DateTime<Utc> = state.clock.now();

    // 1. Record (or find) the command, under the context lock.
    let txn = db.begin().await.map_err(CallError::from)?;
    lock_context(&txn, command.context_id)
        .await
        .map_err(CallError::from)?;
    txn.execute(stmt(
        "INSERT INTO call_privacy_enforcements (enforcement_event_id, tenant_id, application_id, environment_id, \
           communication_context_ref, context_version, relay_session_refs, session_version, affected_principal_refs, \
           reason, terminate_active_calls, requested_by_service, request_id, correlation_id, status, matched_calls, \
           terminated_calls, received_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'RECEIVED', 0, 0, $15) \
         ON CONFLICT (enforcement_event_id) DO NOTHING",
        vec![
            command.enforcement_event_id.into(),
            owner.0.into(),
            owner.1.into(),
            owner.2.into(),
            command.context_id.into(),
            command.context_version.into(),
            command.relay_session_ids.clone().into(),
            command.relay_session_version.into(),
            command.affected_principal_ids.clone().into(),
            command.reason.clone().into(),
            command.terminate_active_calls.into(),
            caller.context().authenticated_service().as_str().into(),
            cmd.request_id().as_str().into(),
            cmd.correlation_id().as_str().into(),
            now.into(),
        ],
    ))
    .await
    .map_err(CallError::from)?;
    let recorded = Recorded::find_by_statement(stmt(
        "SELECT tenant_id, application_id, environment_id, status, matched_calls, terminated_calls \
         FROM call_privacy_enforcements WHERE enforcement_event_id = $1",
        vec![command.enforcement_event_id.into()],
    ))
    .one(&txn)
    .await
    .map_err(CallError::from)?
    .ok_or(CallingError::Internal)?;
    txn.commit().await.map_err(CallError::from)?;
    if (
        recorded.tenant_id,
        recorded.application_id,
        recorded.environment_id,
    ) != owner
    {
        // An event id of another scope: indistinguishable from an unknown resource.
        return Err(CallingError::NotFound);
    }
    if recorded.status == "APPLIED" {
        return Ok((
            StatusCode::OK,
            Json(result_body(
                command.enforcement_event_id,
                recorded.matched_calls,
                recorded.terminated_calls,
            )),
        ));
    }

    // 2. This scope's protected calls under the context (and sessions / principals),
    //    authorized BEFORE the enforced version.
    let candidates = Candidate::find_by_statement(stmt(
        "SELECT c.id FROM calls c \
         WHERE c.tenant_id = $1 AND c.application_id = $2 AND c.environment_id = $3 \
           AND c.communication_context_ref = $4 \
           AND c.current_state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED') \
           AND (cardinality($5::uuid[]) = 0 OR c.privacy_relay_ref = ANY($5)) \
           AND (cardinality($6::uuid[]) = 0 OR c.initiator_principal_id = ANY($6) OR EXISTS ( \
                 SELECT 1 FROM call_participants p WHERE p.call_id = c.id AND p.ended_at IS NULL AND p.principal_id = ANY($6))) \
           AND NOT EXISTS ( \
                 SELECT 1 FROM call_privacy_authorizations a WHERE a.call_id = c.id \
                   AND ((cardinality($5::uuid[]) = 0 AND a.context_version >= $7) \
                     OR (cardinality($5::uuid[]) > 0 AND a.session_version >= COALESCE($8, 0)))) \
         ORDER BY c.created_at, c.id",
        vec![
            owner.0.into(),
            owner.1.into(),
            owner.2.into(),
            command.context_id.into(),
            command.relay_session_ids.clone().into(),
            command.affected_principal_ids.clone().into(),
            command.context_version.into(),
            command.relay_session_version.into(),
        ],
    ))
    .all(db)
    .await
    .map_err(CallError::from)?;
    let matched = candidates.len() as i32;
    let mut terminated = 0;
    if command.terminate_active_calls {
        for candidate in &candidates {
            if terminate(&state, &cmd, CallId::from_uuid(candidate.id)).await? {
                terminated += 1;
            }
        }
    }

    // 3. Record the result (later replays answer it).
    db.execute(stmt(
        "UPDATE call_privacy_enforcements SET status = 'APPLIED', matched_calls = $2, terminated_calls = $3, completed_at = $4 \
         WHERE enforcement_event_id = $1 AND status = 'RECEIVED'",
        vec![command.enforcement_event_id.into(), matched.into(), terminated.into(), state.clock.now().into()],
    ))
    .await
    .map_err(CallError::from)?;
    tracing::info!(
        enforcement_event_id = %command.enforcement_event_id,
        communication_context_ref = %command.context_id,
        context_version = command.context_version,
        reason = %command.reason,
        matched,
        terminated,
        "privacy relay enforcement applied"
    );
    Ok((
        StatusCode::OK,
        Json(result_body(
            command.enforcement_event_id,
            matched,
            terminated,
        )),
    ))
}

/// Ends one protected call through the ordinary call commands (retrying stale versions).
/// Returns whether this command moved it toward termination.
async fn terminate(
    state: &AppState,
    cmd: &CallCommandContext,
    call_id: CallId,
) -> Result<bool, CallingError> {
    for _ in 0..5 {
        let call = state.calls.call(cmd.scope(), call_id).await?;
        let outcome = match call.state {
            CallState::Requested
            | CallState::Authorized
            | CallState::Resolving
            | CallState::Routing
            | CallState::Dialing
            | CallState::Ringing => state.app.cancel(cmd, call_id, call.version, None).await,
            CallState::Answered
            | CallState::Connected
            | CallState::Held
            | CallState::Transferring => state.app.end(cmd, call_id, call.version, None).await,
            // Already ending (cleanup in progress) or terminal.
            _ => return Ok(false),
        };
        match outcome {
            Ok(o) => return Ok(!o.replayed),
            Err(CallError::StaleVersion { .. } | CallError::InvalidTransition { .. }) => continue,
            Err(other) => return Err(other.into()),
        }
    }
    Err(CallingError::ServiceUnavailable)
}

//! A4 moderation commands (all R3, two-person) and the two version reads they need.
//!
//! | Command | Route | Subject (version) | Status |
//! |---|---|---|---|
//! | `messaging.message.remove` | `POST /operator/reports/{reportId}/remove-message` | REPORT (report) | `REMOVED` |
//! | `messaging.report.dismiss` | `POST /operator/reports/{reportId}/dismiss` | REPORT (report) | `DISMISSED` |
//! | `messaging.member.mute` | `POST /operator/conversations/{id}/members/{principalId}/mute` | CONVERSATION_MEMBER (member) | `MUTED` |
//! | `messaging.member.remove` | `POST /operator/conversations/{id}/members/{principalId}/remove` | CONVERSATION_MEMBER (member) | `REMOVED` |
//! | `messaging.conversation.restrict` | `POST /operator/conversations/{id}/restrict` | CONVERSATION (conversation) | `LOCKED` |
//!
//! They run the application moderators' own actions ([`crate::moderation::actions`]) under
//! the resource's scope, so the kill switch `MODERATION_ACTIONS`, the environment's
//! moderation policy, the append-only action record, its event and audit entry all apply
//! unchanged. A removed message is a server tombstone (`MODERATED`) by id: nothing reads
//! or returns its ciphertext or text. Restricting a conversation locks it (`LOCKED`: no
//! new messages; members keep reading what they could read).

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    begin, body, conversation_scope, definite, expected, finish, reason, version_of, BridgeCommand,
    Outcome, ScopeRow, Subject,
};
use crate::{
    audit::{self, AuditEntry},
    conversations::{
        service::{self, request_context, rfc3339},
        LifecycleState,
    },
    db,
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    moderation::actions::{self, ActionRequest},
    operator_reads::require_bridge,
    state::AppState,
};

pub const REMOVE_MESSAGE: &str = "messaging.message.remove";
pub const DISMISS_REPORT: &str = "messaging.report.dismiss";
pub const MUTE_MEMBER: &str = "messaging.member.mute";
pub const REMOVE_MEMBER: &str = "messaging.member.remove";
pub const RESTRICT_CONVERSATION: &str = "messaging.conversation.restrict";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveMessage {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    /// One of the report's messages.
    pub message_id: Uuid,
    pub reason_code: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionedReason {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    pub reason_code: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MuteMember {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    pub reason_code: String,
    /// 60 .. 2592000 (30 days).
    pub duration_seconds: i64,
}

// ---- Reports -------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct ReportLock {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    conversation_id: Uuid,
    message_ids: Vec<Uuid>,
    status: String,
    version: i64,
}

impl ReportLock {
    fn scope(&self) -> ScopeRow {
        ScopeRow {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
        }
    }
}

/// Locks an OPEN report of `tenant_id` at `expected` (404 / 409 otherwise).
async fn lock_report<C: ConnectionTrait>(
    txn: &C,
    id: Uuid,
    tenant_id: Uuid,
    expected: i64,
) -> Result<ReportLock, ApiError> {
    let row: ReportLock = db::one(
        txn,
        "SELECT tenant_id, application_id, environment_id, conversation_id, message_ids, status, version \
           FROM messaging_reports WHERE id = $1 FOR UPDATE",
        vec![id.into()],
    )
    .await?
    .filter(|r: &ReportLock| r.tenant_id == tenant_id)
    .ok_or(Error::code(ErrorCode::NotFound))?;
    if row.version != expected {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    if row.status != "OPEN" {
        return Err(Error::reason(ErrorCode::Conflict, "REPORT_NOT_OPEN"));
    }
    Ok(row)
}

async fn report_version<C: ConnectionTrait>(txn: &C, id: Uuid) -> Result<i64, ApiError> {
    version_of(
        txn,
        "SELECT version FROM messaging_reports WHERE id = $1",
        vec![id.into()],
    )
    .await
}

/// `messaging.message.remove`: tombstones one of an OPEN report's messages (`MODERATED`)
/// and resolves the report `ACTIONED`. Version: the report's.
pub async fn remove_message(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(report_id): Path<Uuid>,
    raw: Result<Json<RemoveMessage>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("REPORT", report_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, REMOVE_MESSAGE, &subject).await? {
        return Ok(Json(done));
    }
    let report = lock_report(&txn, report_id, req.tenant_id, version).await?;
    if !report.message_ids.contains(&req.message_id) {
        return Err(Error::validation(
            "messageId must be one of the report's messages",
        ));
    }
    let ctx = report.scope().context();
    let action = ActionRequest {
        kind: "REMOVE_MESSAGE".into(),
        conversation_id: Some(report.conversation_id),
        message_id: Some(req.message_id),
        principal_id: None,
        report_id: Some(report_id),
        reason: req.reason_code.clone(),
        duration_seconds: None,
    };
    let prepared = actions::prepare(&state, &ctx, &action)
        .await
        .map_err(definite)?;
    let applied = actions::apply_in(&txn, &ctx, &action, None, &prepared, now)
        .await
        .map_err(definite)?;
    let outcome = Outcome {
        state: "REMOVED",
        version: Some(report_version(&txn, report_id).await?),
        resource: json!({
            "reportId": report_id,
            "reportStatus": "ACTIONED",
            "conversationId": report.conversation_id,
            "messageId": req.message_id,
            "messageLifecycle": "MODERATED",
            "actionId": applied.id,
            "actedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, REMOVE_MESSAGE, &subject, outcome, now).await?;
    txn.commit().await?;
    after_action(&state);
    Ok(Json(answer))
}

/// `messaging.report.dismiss`: resolves an OPEN report `DISMISSED` (the reason code is its
/// resolution). Version: the report's.
pub async fn dismiss_report(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(report_id): Path<Uuid>,
    raw: Result<Json<VersionedReason>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("REPORT", report_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, DISMISS_REPORT, &subject).await? {
        return Ok(Json(done));
    }
    let report = lock_report(&txn, report_id, req.tenant_id, version).await?;
    let ctx = report.scope().context();
    let action = ActionRequest {
        kind: "DISMISS_REPORT".into(),
        conversation_id: None,
        message_id: None,
        principal_id: None,
        report_id: Some(report_id),
        reason: req.reason_code.clone(),
        duration_seconds: None,
    };
    let prepared = actions::prepare(&state, &ctx, &action)
        .await
        .map_err(definite)?;
    let applied = actions::apply_in(&txn, &ctx, &action, None, &prepared, now)
        .await
        .map_err(definite)?;
    let outcome = Outcome {
        state: "DISMISSED",
        version: Some(report_version(&txn, report_id).await?),
        resource: json!({
            "reportId": report_id,
            "reportStatus": "DISMISSED",
            "resolution": req.reason_code,
            "actionId": applied.id,
            "actedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, DISMISS_REPORT, &subject, outcome, now).await?;
    txn.commit().await?;
    after_action(&state);
    Ok(Json(answer))
}

// ---- Members -------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct MemberRow {
    conversation_id: Uuid,
    principal_id: Uuid,
    role: String,
    state: String,
    send_restricted_until: Option<DateTime<Utc>>,
    version: i64,
    updated_at: DateTime<Utc>,
}

const MEMBER_COLUMNS: &str =
    "conversation_id, principal_id, role, state, send_restricted_until, version, updated_at";

async fn load_member<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    principal_id: Uuid,
    lock: bool,
) -> Result<Option<MemberRow>, ApiError> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {MEMBER_COLUMNS} FROM messaging_conversation_members WHERE conversation_id = $1 AND principal_id = $2{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![conversation_id.into(), principal_id.into()],
    )
    .await?)
}

/// Locks the conversation (first, as every membership change does) and an ACTIVE member
/// at `expected`.
async fn lock_member<C: ConnectionTrait>(
    txn: &C,
    conversation_id: Uuid,
    principal_id: Uuid,
    tenant_id: Uuid,
    expected: i64,
) -> Result<(ScopeRow, MemberRow), ApiError> {
    let scope = conversation_scope(txn, conversation_id, tenant_id, true).await?;
    let member = load_member(txn, conversation_id, principal_id, true)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if member.version != expected {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    if member.state != "ACTIVE" {
        return Err(Error::reason(ErrorCode::Conflict, "MEMBER_NOT_ACTIVE"));
    }
    Ok((scope, member))
}

fn member_subject(conversation_id: Uuid, principal_id: Uuid) -> Subject {
    Subject::new(
        "CONVERSATION_MEMBER",
        format!("{conversation_id}:{principal_id}"),
    )
}

/// `messaging.member.mute`: the member may not send until `now + durationSeconds`.
/// Version: the member's.
pub async fn mute_member(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((conversation_id, principal_id)): Path<(Uuid, Uuid)>,
    raw: Result<Json<MuteMember>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = member_subject(conversation_id, principal_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, MUTE_MEMBER, &subject).await? {
        return Ok(Json(done));
    }
    let (scope, _) =
        lock_member(&txn, conversation_id, principal_id, req.tenant_id, version).await?;
    let ctx = scope.context();
    let action = ActionRequest {
        kind: "MUTE_MEMBER".into(),
        conversation_id: Some(conversation_id),
        message_id: None,
        principal_id: Some(principal_id),
        report_id: None,
        reason: req.reason_code.clone(),
        duration_seconds: Some(req.duration_seconds),
    };
    let prepared = actions::prepare(&state, &ctx, &action)
        .await
        .map_err(definite)?;
    let applied = actions::apply_in(&txn, &ctx, &action, None, &prepared, now)
        .await
        .map_err(definite)?;
    let member = load_member(&txn, conversation_id, principal_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    let outcome = Outcome {
        state: "MUTED",
        version: Some(member.version),
        resource: json!({
            "conversationId": conversation_id,
            "principalId": principal_id,
            "memberState": member.state,
            "sendRestrictedUntil": member.send_restricted_until.map(rfc3339),
            "actionId": applied.id,
            "actedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, MUTE_MEMBER, &subject, outcome, now).await?;
    txn.commit().await?;
    after_action(&state);
    Ok(Json(answer))
}

/// `messaging.member.remove`: removes an ACTIVE member (`REMOVED`; never the owner, never
/// from a DIRECT conversation; in PRIVATE_E2EE the cryptographic removal follows by a
/// member's MLS commit). Version: the member's.
pub async fn remove_member(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((conversation_id, principal_id)): Path<(Uuid, Uuid)>,
    raw: Result<Json<VersionedReason>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = member_subject(conversation_id, principal_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, REMOVE_MEMBER, &subject).await? {
        return Ok(Json(done));
    }
    let (scope, _) =
        lock_member(&txn, conversation_id, principal_id, req.tenant_id, version).await?;
    let ctx = scope.context();
    let action = ActionRequest {
        kind: "REMOVE_MEMBER".into(),
        conversation_id: Some(conversation_id),
        message_id: None,
        principal_id: Some(principal_id),
        report_id: None,
        reason: req.reason_code.clone(),
        duration_seconds: None,
    };
    let prepared = actions::prepare(&state, &ctx, &action)
        .await
        .map_err(definite)?;
    let applied = actions::apply_in(&txn, &ctx, &action, None, &prepared, now)
        .await
        .map_err(definite)?;
    let member = load_member(&txn, conversation_id, principal_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    let outcome = Outcome {
        state: "REMOVED",
        version: Some(member.version),
        resource: json!({
            "conversationId": conversation_id,
            "principalId": principal_id,
            "memberState": member.state,
            "actionId": applied.id,
            "actedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, REMOVE_MEMBER, &subject, outcome, now).await?;
    txn.commit().await?;
    after_action(&state);
    Ok(Json(answer))
}

// ---- Conversations ---------------------------------------------------------------------

/// `messaging.conversation.restrict`: locks an ACTIVE conversation (`LOCKED`: it accepts
/// no new messages). Version: the conversation's.
pub async fn restrict_conversation(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    raw: Result<Json<VersionedReason>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("CONVERSATION", conversation_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, RESTRICT_CONVERSATION, &subject).await? {
        return Ok(Json(done));
    }
    let scope = conversation_scope(&txn, conversation_id, req.tenant_id, false).await?;
    let ctx = scope.context();
    let row = service::load(&txn, &ctx, conversation_id, true)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if row.version != version {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    if row.state() != LifecycleState::Active {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "CONVERSATION_NOT_ACTIVE",
        ));
    }
    crate::operations::require_open(
        &txn,
        crate::operations::KillSwitch::ModerationActions,
        scope.environment_id,
    )
    .await
    .map_err(definite)?;
    let rctx = request_context();
    let updated = service::transition(
        &txn,
        &ctx,
        &row,
        LifecycleState::Locked,
        Some(&req.reason_code),
        None,
        None,
        now,
        &rctx,
    )
    .await?;
    audit::record(
        &txn,
        AuditEntry {
            ctx: &ctx,
            principal_id: None,
            action: "CONVERSATION_RESTRICTED",
            target_kind: "CONVERSATION",
            target_id: conversation_id,
            detail: json!({ "reason": req.reason_code, "lifecycle_state": "LOCKED", "via": "ADMIN_BRIDGE" }),
        },
        &rctx,
        now,
    )
    .await?;
    let outcome = Outcome {
        state: "LOCKED",
        version: Some(updated.version),
        resource: json!({
            "conversationId": conversation_id,
            "lifecycleState": updated.lifecycle_state,
            "lifecycleReason": updated.lifecycle_reason,
            "stateChangedAt": rfc3339(updated.state_changed_at),
        }),
    };
    let answer = finish(&txn, &cmd, RESTRICT_CONVERSATION, &subject, outcome, now).await?;
    txn.commit().await?;
    after_action(&state);
    Ok(Json(answer))
}

fn after_action(state: &AppState) {
    crate::metrics::inc("messaging_moderation_actions_total", &[]);
    state.wake_outbox();
}

// ---- Version reads -------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct ConversationRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    context_class: String,
    security_profile: String,
    lifecycle_state: String,
    version: i64,
    created_at: DateTime<Utc>,
    state_changed_at: DateTime<Utc>,
}

/// `GET /operator/conversations/{conversationId}`: ids, class, profile, lifecycle and
/// version (what `restrict` needs). Never a title, avatar, member list or content.
pub async fn conversation(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(conversation_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let c: ConversationRow = db::one(
        &state.db,
        "SELECT id, tenant_id, application_id, environment_id, context_class, security_profile, lifecycle_state, \
           version, created_at, state_changed_at FROM messaging_conversations WHERE id = $1",
        vec![conversation_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(Json(json!({
        "id": c.id,
        "tenantId": c.tenant_id,
        "applicationId": c.application_id,
        "environmentId": c.environment_id,
        "contextClass": c.context_class,
        "securityProfile": c.security_profile,
        "lifecycleState": c.lifecycle_state,
        "version": c.version,
        "createdAt": rfc3339(c.created_at),
        "stateChangedAt": rfc3339(c.state_changed_at),
    })))
}

/// `GET /operator/conversations/{conversationId}/members/{principalId}`: the member's
/// role, state, send restriction and version (what `mute` / `remove` need).
pub async fn member(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path((conversation_id, principal_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let m = load_member(&state.db, conversation_id, principal_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(Json(json!({
        "conversationId": m.conversation_id,
        "principalId": m.principal_id,
        "role": m.role,
        "state": m.state,
        "sendRestrictedUntil": m.send_restricted_until.map(rfc3339),
        "version": m.version,
        "updatedAt": rfc3339(m.updated_at),
    })))
}

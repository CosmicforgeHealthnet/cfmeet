//! Moderation actions (§86): application moderators, append-only and audited.

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::reports;
use crate::{
    audit::{self, AuditEntry},
    conversations::{
        policy,
        service::{self, request_context, rfc3339, scope_of},
    },
    db,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
    membership, messages,
    state::AppState,
};

pub const KINDS: &[&str] = &[
    "REMOVE_MESSAGE",
    "MUTE_MEMBER",
    "UNMUTE_MEMBER",
    "REMOVE_MEMBER",
    "RESTRICT_PRINCIPAL",
    "LIFT_RESTRICTION",
    "DISMISS_REPORT",
];
/// Longest mute / restriction a single action imposes (30 days).
pub const MAX_DURATION_SECONDS: i64 = 30 * 24 * 3600;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRequest {
    pub kind: String,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    #[serde(default)]
    pub message_id: Option<Uuid>,
    #[serde(default)]
    pub principal_id: Option<Uuid>,
    #[serde(default)]
    pub report_id: Option<Uuid>,
    pub reason: String,
    #[serde(default)]
    pub duration_seconds: Option<i64>,
}

#[derive(Debug, Clone, FromQueryResult)]
struct ActionRow {
    id: Uuid,
    kind: String,
    conversation_id: Option<Uuid>,
    message_id: Option<Uuid>,
    principal_id: Option<Uuid>,
    report_id: Option<Uuid>,
    reason: String,
    effective_until: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

fn view(a: &ActionRow) -> Value {
    json!({
        "actionId": a.id,
        "kind": a.kind,
        "conversationId": a.conversation_id,
        "messageId": a.message_id,
        "principalId": a.principal_id,
        "reportId": a.report_id,
        "reason": a.reason,
        "effectiveUntil": a.effective_until.map(rfc3339),
        "createdAt": rfc3339(a.created_at),
    })
}

fn require<T>(v: Option<T>, field: &str) -> Result<T> {
    v.ok_or_else(|| Error::validation(format!("{field} is required for this kind")))
}

/// `POST /v1/messaging/moderation/actions`.
pub async fn apply(
    state: &AppState,
    ctx: &TrustedContext,
    request: ActionRequest,
) -> Result<Value> {
    if !KINDS.contains(&request.kind.as_str()) {
        return Err(Error::validation(format!(
            "kind must be one of {}",
            KINDS.join(", ")
        )));
    }
    if !membership::valid_reason(&request.reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    if !effective.moderation.enabled {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "MODERATION_DISABLED",
        ));
    }
    let timed = matches!(request.kind.as_str(), "MUTE_MEMBER" | "RESTRICT_PRINCIPAL");
    let duration = match (timed, request.duration_seconds) {
        (true, Some(d)) if (60..=MAX_DURATION_SECONDS).contains(&d) => {
            Some(chrono::Duration::seconds(d))
        }
        (true, _) => {
            return Err(Error::validation(format!(
                "durationSeconds must be between 60 and {MAX_DURATION_SECONDS}"
            )))
        }
        (false, Some(_)) => {
            return Err(Error::validation(
                "durationSeconds applies to MUTE_MEMBER and RESTRICT_PRINCIPAL",
            ))
        }
        (false, None) => None,
    };
    let grace = chrono::Duration::seconds(effective.retention.deleted_purge_grace_seconds);
    let st = state.clone();
    let ctx2 = ctx.clone();
    let req = request.clone();
    let row = state
        .db
        .transaction::<_, ActionRow, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let rctx = request_context();
                if let Some(r) = req.report_id {
                    reports::load(txn, &ctx2, r, true).await?.ok_or(Error::code(ErrorCode::NotFound))?;
                }
                let conversation = match req.conversation_id {
                    Some(c) => Some(service::load(txn, &ctx2, c, true).await?.ok_or(Error::code(ErrorCode::NotFound))?),
                    None => None,
                };
                let until = duration.map(|d| now + d);
                match req.kind.as_str() {
                    "REMOVE_MESSAGE" => {
                        let c = require(conversation.as_ref(), "conversationId")?;
                        let m = messages::load_message(txn, c.id, require(req.message_id, "messageId")?, true)
                            .await?
                            .ok_or(Error::code(ErrorCode::NotFound))?;
                        if m.message_kind == "MLS_COMMIT" {
                            return Err(Error::reason(ErrorCode::MessageNotModifiable, "HANDSHAKE"));
                        }
                        // A tombstone: no member, replay or fanout delivers it again; the
                        // purge removes the stored ciphertext / text (unless held). The
                        // moderator never sees E2EE content.
                        messages::tombstone(txn, &scope_of(&ctx2), &m, "MODERATED", &req.reason, now + grace, &rctx, now).await?;
                    }
                    "MUTE_MEMBER" | "UNMUTE_MEMBER" => {
                        let c = require(conversation.as_ref(), "conversationId")?;
                        let p = require(req.principal_id, "principalId")?;
                        membership::restrict_in(txn, &ctx2, c, p, until, &req.reason, &rctx, now).await?;
                    }
                    "REMOVE_MEMBER" => {
                        let c = require(conversation.as_ref(), "conversationId")?;
                        if c.context_class == "DIRECT" {
                            return Err(Error::reason(ErrorCode::InvalidTransition, "DIRECT_MEMBERSHIP_FIXED"));
                        }
                        let p = require(req.principal_id, "principalId")?;
                        membership::remove_in(txn, &ctx2, c, p, false, &req.reason, None, &rctx, now).await?;
                    }
                    "RESTRICT_PRINCIPAL" => {
                        let p = require(req.principal_id, "principalId")?;
                        db::exec(
                            txn,
                            "INSERT INTO messaging_principal_restrictions (id, tenant_id, application_id, environment_id, principal_id, \
                               reason, source, starts_at, ends_at, created_by_credential_id) \
                             VALUES ($1, $2, $3, $4, $5, $6, 'MODERATOR', $7, $8, $9)",
                            vec![
                                Uuid::new_v4().into(),
                                ctx2.scope.tenant_id.into(),
                                ctx2.scope.application_id.into(),
                                ctx2.scope.environment_id.into(),
                                p.into(),
                                req.reason.clone().into(),
                                now.into(),
                                until.into(),
                                ctx2.credential_id.into(),
                            ],
                        )
                        .await?;
                    }
                    "LIFT_RESTRICTION" => {
                        let p = require(req.principal_id, "principalId")?;
                        let n = db::exec(
                            txn,
                            "UPDATE messaging_principal_restrictions SET lifted_at = $3 WHERE environment_id = $1 AND principal_id = $2 \
                               AND lifted_at IS NULL AND ends_at > $3",
                            vec![ctx2.scope.environment_id.into(), p.into(), now.into()],
                        )
                        .await?;
                        if n == 0 {
                            return Err(Error::code(ErrorCode::NotFound));
                        }
                    }
                    _ => {
                        require(req.report_id, "reportId")?;
                    }
                }
                if let Some(r) = req.report_id {
                    if req.kind == "DISMISS_REPORT" {
                        reports::resolve(txn, &ctx2, r, "DISMISSED", &req.reason, now).await?;
                    } else {
                        reports::resolve(txn, &ctx2, r, "ACTIONED", &req.kind, now).await?;
                    }
                }
                let id = Uuid::new_v4();
                db::exec(
                    txn,
                    "INSERT INTO messaging_moderation_actions (id, tenant_id, application_id, environment_id, kind, conversation_id, \
                       message_id, principal_id, report_id, reason, effective_until, credential_id, created_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
                    vec![
                        id.into(),
                        ctx2.scope.tenant_id.into(),
                        ctx2.scope.application_id.into(),
                        ctx2.scope.environment_id.into(),
                        req.kind.clone().into(),
                        req.conversation_id.into(),
                        req.message_id.into(),
                        req.principal_id.into(),
                        req.report_id.into(),
                        req.reason.clone().into(),
                        until.into(),
                        ctx2.credential_id.into(),
                        now.into(),
                    ],
                )
                .await?;
                crate::events::outbox::record(
                    txn,
                    "messaging.moderation.actioned",
                    "MODERATION_ACTION",
                    id,
                    crate::conversations::service::scope_of(&ctx2),
                    json!({ "action_id": id, "kind": req.kind, "conversation_id": req.conversation_id,
                            "message_id": req.message_id, "principal_id": req.principal_id, "report_id": req.report_id }),
                    now,
                )
                .await?;
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx2,
                        principal_id: None,
                        action: "MODERATION_ACTION",
                        target_kind: "MODERATION_ACTION",
                        target_id: id,
                        detail: json!({ "kind": req.kind, "reason": req.reason, "conversation_id": req.conversation_id,
                                        "message_id": req.message_id, "principal_id": req.principal_id, "report_id": req.report_id }),
                    },
                    &rctx,
                    now,
                )
                .await?;
                Ok(ActionRow {
                    id,
                    kind: req.kind,
                    conversation_id: req.conversation_id,
                    message_id: req.message_id,
                    principal_id: req.principal_id,
                    report_id: req.report_id,
                    reason: req.reason,
                    effective_until: until,
                    created_at: now,
                })
            })
        })
        .await
        .map_err(Error::from)?;
    crate::metrics::inc("messaging_moderation_actions_total", &[]);
    Ok(view(&row))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /v1/messaging/moderation/actions` (newest first).
pub async fn list(state: &AppState, ctx: &TrustedContext, q: ListQuery) -> Result<Value> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(Error::validation("limit must be between 1 and 200"));
    }
    let rows: Vec<ActionRow> = db::all(
        &state.db,
        "SELECT id, kind, conversation_id, message_id, principal_id, report_id, reason, effective_until, created_at \
           FROM messaging_moderation_actions WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
          ORDER BY created_at DESC, id LIMIT $4",
        vec![ctx.scope.tenant_id.into(), ctx.scope.application_id.into(), ctx.scope.environment_id.into(), limit.into()],
    )
    .await?;
    Ok(json!({ "actions": rows.iter().map(view).collect::<Vec<_>>() }))
}

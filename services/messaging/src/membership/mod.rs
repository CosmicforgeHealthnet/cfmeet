//! Conversation membership (Part V Workstream E, the authority Parts E–G build on).
//!
//! * Members are ACTIVE, REMOVED or LEFT; re-admission reactivates the row with a NEW
//!   history window (`visible_from_sequence` / `visible_from_event`): a member sees only
//!   what is committed after (re)joining (AD-07: no server-granted history).
//! * Roles: OWNER (exactly the conversation owner), ADMIN, MEMBER. Administration needs
//!   `conversations.manage`; with an acting principal it must be an OWNER / ADMIN.
//! * Every change commits with its durable conversation event (`member.added`,
//!   `member.removed`, `member.updated`) so realtime fanout and replay see it in order.
//! * PRIVATE_E2EE: removing a member is an AUTHORIZATION change only. Every ACTIVE device
//!   of the removed principal is recorded as a pending MLS removal and the group accepts
//!   only commits until a member's commit declares those devices removed
//!   ([`crate::mls`]). A server-side removal alone is never presented as cryptographic
//!   exclusion.

pub mod http;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry},
    conversations::{
        access::{self, Membership},
        policy,
        service::{load, request_context, rfc3339, scope_of, ConversationRow},
        ContextClass,
    },
    db,
    domain::{RequestContext, SecurityProfile},
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    http::middleware::Acting,
    identity::{PrincipalStatus, TrustedContext},
    state::AppState,
};

/// Who administers: a principal (must be OWNER / ADMIN) or the application backend.
#[derive(Debug, Clone)]
pub enum Administrator {
    Principal(Acting),
    Application,
}

impl Administrator {
    pub fn principal_id(&self) -> Option<Uuid> {
        match self {
            Administrator::Principal(a) => Some(a.principal_id),
            Administrator::Application => None,
        }
    }
}

#[derive(Debug, Clone, sea_orm::FromQueryResult)]
struct MemberViewRow {
    principal_id: Uuid,
    role: String,
    state: String,
    joined_at: DateTime<Utc>,
    send_restricted_until: Option<DateTime<Utc>>,
    version: i64,
}

fn member_view(m: &MemberViewRow) -> Value {
    json!({
        "principalId": m.principal_id,
        "role": m.role,
        "state": m.state,
        "joinedAt": rfc3339(m.joined_at),
        "sendRestrictedUntil": m.send_restricted_until.map(rfc3339),
        "version": m.version,
    })
}

/// Authorizes an administrator of `conversation` (NOT_FOUND for a non-member principal).
async fn require_admin<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
    admin: &Administrator,
) -> Result<()> {
    if let Administrator::Principal(a) = admin {
        match access::membership(db, conversation.id, a.principal_id, false).await? {
            Some(m) if m.is_admin() => {}
            Some(m) if m.is_active() => {
                return Err(Error::reason(
                    ErrorCode::PrincipalNotAuthorized,
                    "NOT_ADMIN",
                ))
            }
            _ => return Err(Error::code(ErrorCode::NotFound)),
        }
    }
    Ok(())
}

pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    reader: Option<&Acting>,
    conversation_id: Uuid,
) -> Result<Value> {
    match reader {
        Some(a) => {
            access::as_member(&state.db, ctx, conversation_id, a.principal_id).await?;
        }
        None => {
            access::in_scope(&state.db, ctx, conversation_id).await?;
        }
    }
    let rows: Vec<MemberViewRow> = db::all(
        &state.db,
        "SELECT principal_id, role, state, joined_at, send_restricted_until, version FROM messaging_conversation_members \
         WHERE conversation_id = $1 AND state = 'ACTIVE' ORDER BY joined_at, principal_id LIMIT 1000",
        vec![conversation_id.into()],
    )
    .await?;
    Ok(json!({ "members": rows.iter().map(member_view).collect::<Vec<_>>() }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddRequest {
    pub principal_id: Uuid,
    #[serde(default)]
    pub role: Option<String>,
}

/// Next message / event positions: the history window of a member joining now.
async fn positions<C: ConnectionTrait>(db: &C, conversation_id: Uuid) -> Result<(i64, i64)> {
    #[derive(sea_orm::FromQueryResult)]
    struct P {
        next_sequence: i64,
        next_event_sequence: i64,
    }
    let p: P = db::one(
        db,
        "SELECT next_sequence, next_event_sequence FROM messaging_conversation_sequences WHERE conversation_id = $1 FOR UPDATE",
        vec![conversation_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok((p.next_sequence, p.next_event_sequence))
}

pub async fn add(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
    request: AddRequest,
) -> Result<(u16, Value)> {
    let role = match request.role.as_deref() {
        None | Some("MEMBER") => "MEMBER",
        Some("ADMIN") => "ADMIN",
        Some(_) => return Err(Error::validation("role must be MEMBER or ADMIN")),
    };
    // The new member must be an ACTIVE messaging principal of the SAME environment.
    match state
        .principals
        .resolve(&ctx.scope, request.principal_id)
        .await
    {
        Ok(s @ PrincipalStatus::Active { .. }) if s.is_messaging_capable() => {}
        Ok(_) => {
            return Err(Error::reason(
                ErrorCode::PrincipalNotAuthorized,
                "PRINCIPAL_NOT_AVAILABLE",
            ))
        }
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable");
            return Err(Error::code(ErrorCode::ServiceUnavailable));
        }
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let admin = admin.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, (u16, Value), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let conversation = load(txn, &ctx, conversation_id, true)
                    .await?
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                add_in(
                    txn,
                    &st,
                    &ctx,
                    &admin,
                    &conversation,
                    request.principal_id,
                    role,
                    &request_ctx,
                    now,
                )
                .await
            })
        })
        .await
        .map_err(Error::from)
}

/// Whether an unlifted ban keeps `principal` out of the conversation.
pub async fn banned<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    principal: Uuid,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_conversation_bans WHERE conversation_id = $1 AND principal_id = $2 AND lifted_at IS NULL",
        vec![conversation_id.into(), principal.into()],
    )
    .await?
        > 0)
}

/// Adds (or re-admits) an already-resolved ACTIVE principal inside the caller's
/// transaction holding the conversation row lock. `(200, view)` when already a member.
#[allow(clippy::too_many_arguments)]
pub async fn add_in<C: ConnectionTrait>(
    txn: &C,
    st: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation: &ConversationRow,
    principal_id: Uuid,
    role: &str,
    request_ctx: &RequestContext,
    now: DateTime<Utc>,
) -> Result<(u16, Value)> {
    let conversation_id = conversation.id;
    let request = AddRequestRef { principal_id };
    require_admin(txn, conversation, admin).await?;
    access::require_accepting(conversation, now)?;
    let class = ContextClass::parse(&conversation.context_class)
        .ok_or(Error::code(ErrorCode::InternalError))?;
    if class == ContextClass::Direct {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "DIRECT_MEMBERSHIP_FIXED",
        ));
    }
    let existing = access::membership(txn, conversation_id, request.principal_id, true).await?;
    if existing.as_ref().is_some_and(Membership::is_active) {
        let view = member_row(txn, conversation_id, request.principal_id).await?;
        return Ok((200, view));
    }
    if banned(txn, conversation_id, request.principal_id).await? {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_BANNED",
        ));
    }
    let effective = policy::current(txn, ctx.scope.environment_id).await?;
    let max = effective
        .context(class)
        .max_members
        .unwrap_or(st.conversations.max_members);
    let count = db::scalar_i64(
        txn,
        "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = $1 AND state = 'ACTIVE'",
        vec![conversation_id.into()],
    )
    .await?;
    if count >= max.min(st.conversations.max_members) {
        return Err(Error::reason(ErrorCode::Conflict, "MEMBER_LIMIT_REACHED"));
    }
    let (next_sequence, _) = positions(txn, conversation_id).await?;
    let (_, cursor) = outbox::enqueue_conversation(
        txn,
        conversation_id,
        NewEvent {
            event_type: "messaging.member.added",
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation_id,
            scope: scope_of(ctx),
            payload: json!({
                "conversation_id": conversation_id,
                "principal_id": request.principal_id,
                "role": role,
                "added_by_principal_id": admin.principal_id(),
            }),
        },
        request_ctx,
        now,
    )
    .await?;
    // The new member sees its own `member.added` event and everything after.
    let visible_event = cursor - 1;
    if existing.is_some() {
        db::exec(
            txn,
            "UPDATE messaging_conversation_members SET state = 'ACTIVE', role = $3, removed_at = NULL, \
               removed_reason = NULL, send_restricted_until = NULL, visible_from_sequence = $4, \
               visible_from_event = $5, joined_at = $6, updated_at = $6, version = version + 1 \
             WHERE conversation_id = $1 AND principal_id = $2",
            vec![
                conversation_id.into(),
                request.principal_id.into(),
                role.into(),
                next_sequence.into(),
                visible_event.into(),
                now.into(),
            ],
        )
        .await?;
    } else {
        db::exec(
            txn,
            "INSERT INTO messaging_conversation_members (conversation_id, principal_id, environment_id, role, \
               joined_at, updated_at, visible_from_sequence, visible_from_event) \
             VALUES ($1, $2, $3, $4, $5, $5, $6, $7)",
            vec![
                conversation_id.into(),
                request.principal_id.into(),
                ctx.scope.environment_id.into(),
                role.into(),
                now.into(),
                next_sequence.into(),
                visible_event.into(),
            ],
        )
        .await?;
    }
    // A re-admitted principal's devices are no longer "to be removed".
    db::exec(
        txn,
        "DELETE FROM messaging_mls_pending_removals WHERE conversation_id = $1 AND principal_id = $2",
        vec![conversation_id.into(), request.principal_id.into()],
    )
    .await?;
    crate::mls::refresh_rekey(txn, conversation_id, now).await?;
    record_audit(
        txn,
        ctx,
        admin,
        "MEMBER_ADDED",
        conversation_id,
        json!({ "member_principal_id": request.principal_id, "role": role }),
        request_ctx,
        now,
    )
    .await?;
    crate::metrics::inc("messaging_members_added_total", &[]);
    Ok((
        201,
        member_row(txn, conversation_id, request.principal_id).await?,
    ))
}

struct AddRequestRef {
    principal_id: Uuid,
}

async fn member_row<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    principal: Uuid,
) -> Result<Value> {
    let row: MemberViewRow = db::one(
        db,
        "SELECT principal_id, role, state, joined_at, send_restricted_until, version FROM messaging_conversation_members \
         WHERE conversation_id = $1 AND principal_id = $2",
        vec![conversation_id.into(), principal.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(member_view(&row))
}

#[allow(clippy::too_many_arguments)]
async fn record_audit<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    admin: &Administrator,
    action: &'static str,
    conversation_id: Uuid,
    detail: Value,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<()> {
    audit::record(
        db,
        AuditEntry {
            ctx,
            principal_id: admin.principal_id(),
            action,
            target_kind: "CONVERSATION",
            target_id: conversation_id,
            detail,
        },
        request,
        now,
    )
    .await
}

pub fn valid_reason(r: &str) -> bool {
    (2..=48).contains(&r.len())
        && r.as_bytes()[0].is_ascii_uppercase()
        && r.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Removes (`REMOVED`) or lets leave (`LEFT`, when the acting principal is the member)
/// `principal` from the conversation, inside the caller's transaction.
#[allow(clippy::too_many_arguments)]
pub async fn remove_in<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    principal: Uuid,
    leaving: bool,
    reason: &str,
    actor: Option<Uuid>,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<Value> {
    let current = access::membership(db, conversation.id, principal, true).await?;
    let Some(current) = current.filter(Membership::is_active) else {
        return Err(Error::code(ErrorCode::NotFound));
    };
    if current.role == "OWNER" {
        return Err(Error::reason(
            ErrorCode::Conflict,
            "OWNER_CANNOT_BE_REMOVED",
        ));
    }
    db::exec(
        db,
        "UPDATE messaging_conversation_members SET state = $3, removed_at = $4, removed_reason = $5, updated_at = $4, \
           version = version + 1 WHERE conversation_id = $1 AND principal_id = $2",
        vec![
            conversation.id.into(),
            principal.into(),
            (if leaving { "LEFT" } else { "REMOVED" }).into(),
            now.into(),
            reason.into(),
        ],
    )
    .await?;
    let mut rekey = false;
    if conversation.profile() == SecurityProfile::PrivateE2ee {
        rekey =
            crate::mls::require_removal_of_principal(db, conversation.id, principal, reason, now)
                .await?;
    }
    outbox::enqueue_conversation(
        db,
        conversation.id,
        NewEvent {
            event_type: "messaging.member.removed",
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation.id,
            scope: scope_of(ctx),
            payload: json!({
                "conversation_id": conversation.id,
                "principal_id": principal,
                "reason": reason,
                "left": leaving,
                "removed_by_principal_id": actor,
                "mls_rekey_required": rekey,
            }),
        },
        request,
        now,
    )
    .await?;
    crate::metrics::inc("messaging_members_removed_total", &[]);
    Ok(json!({
        "principalId": principal,
        "state": if leaving { "LEFT" } else { "REMOVED" },
        "reason": reason,
        "mlsRekeyRequired": rekey,
        "cryptographicRemoval": if conversation.profile() == SecurityProfile::PrivateE2ee {
            "PENDING_MEMBER_COMMIT"
        } else {
            "NOT_APPLICABLE"
        },
    }))
}

pub async fn remove(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
    principal: Uuid,
    reason: Option<String>,
) -> Result<Value> {
    let reason = reason.unwrap_or_else(|| "REMOVED_BY_ADMIN".into());
    if !valid_reason(&reason) {
        return Err(Error::validation(
            "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
        ));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let admin = admin.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let conversation = load(txn, &ctx, conversation_id, true)
                    .await?
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                let leaving = admin.principal_id() == Some(principal);
                if !leaving {
                    require_admin(txn, &conversation, &admin).await?;
                } else if !access::membership(txn, conversation_id, principal, false)
                    .await?
                    .is_some_and(|m| m.is_active())
                {
                    return Err(Error::code(ErrorCode::NotFound));
                }
                if conversation.context_class == "DIRECT" {
                    return Err(Error::reason(
                        ErrorCode::InvalidTransition,
                        "DIRECT_MEMBERSHIP_FIXED",
                    ));
                }
                let reason = if leaving { "LEFT".to_string() } else { reason };
                let view = remove_in(
                    txn,
                    &ctx,
                    &conversation,
                    principal,
                    leaving,
                    &reason,
                    admin.principal_id(),
                    &request_ctx,
                    now,
                )
                .await?;
                record_audit(
                    txn,
                    &ctx,
                    &admin,
                    if leaving {
                        "MEMBER_LEFT"
                    } else {
                        "MEMBER_REMOVED"
                    },
                    conversation_id,
                    json!({ "member_principal_id": principal, "reason": reason }),
                    &request_ctx,
                    now,
                )
                .await?;
                Ok(view)
            })
        })
        .await
        .map_err(Error::from)
}

/// Mutes (`until` in the future) or unmutes (`None`) a member (moderation, §86).
#[allow(clippy::too_many_arguments)]
pub async fn restrict_in<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    principal: Uuid,
    until: Option<DateTime<Utc>>,
    reason: &str,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<()> {
    let updated = db::exec(
        db,
        "UPDATE messaging_conversation_members SET send_restricted_until = $3, updated_at = $4, version = version + 1 \
         WHERE conversation_id = $1 AND principal_id = $2 AND state = 'ACTIVE'",
        vec![conversation.id.into(), principal.into(), until.into(), now.into()],
    )
    .await?;
    if updated != 1 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    outbox::enqueue_conversation(
        db,
        conversation.id,
        NewEvent {
            event_type: "messaging.member.updated",
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation.id,
            scope: scope_of(ctx),
            payload: json!({
                "conversation_id": conversation.id,
                "principal_id": principal,
                "send_restricted_until": until.map(rfc3339),
                "reason": reason,
            }),
        },
        request,
        now,
    )
    .await?;
    Ok(())
}

/// Changes an ACTIVE member's role (`MEMBER` / `ADMIN`; the OWNER keeps ownership) inside
/// the caller's transaction. `Ok(false)` when nothing changed.
pub async fn set_role_in<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    principal: Uuid,
    role: &str,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<bool> {
    if !matches!(role, "MEMBER" | "ADMIN") {
        return Err(Error::validation("role must be MEMBER or ADMIN"));
    }
    #[derive(sea_orm::FromQueryResult)]
    struct Updated {
        send_restricted_until: Option<DateTime<Utc>>,
    }
    let updated: Option<Updated> = db::one(
        db,
        "UPDATE messaging_conversation_members SET role = $3, updated_at = $4, version = version + 1 \
         WHERE conversation_id = $1 AND principal_id = $2 AND state = 'ACTIVE' AND role NOT IN ('OWNER', $3) \
         RETURNING send_restricted_until",
        vec![conversation.id.into(), principal.into(), role.into(), now.into()],
    )
    .await?;
    let Some(updated) = updated else {
        return Ok(false);
    };
    outbox::enqueue_conversation(
        db,
        conversation.id,
        NewEvent {
            event_type: "messaging.member.updated",
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation.id,
            scope: scope_of(ctx),
            payload: json!({
                "conversation_id": conversation.id,
                "principal_id": principal,
                "role": role,
                "send_restricted_until": updated.send_restricted_until.map(rfc3339),
                "reason": "ROLE_CHANGED",
            }),
        },
        request,
        now,
    )
    .await?;
    Ok(true)
}

/// Bans `principal` (removing it first when active) inside the caller's transaction: a
/// banned principal cannot be re-added until [`unban_in`].
#[allow(clippy::too_many_arguments)]
pub async fn ban_in<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    principal: Uuid,
    reason: &str,
    actor: Option<Uuid>,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<()> {
    if access::membership(db, conversation.id, principal, true)
        .await?
        .is_some_and(|m| m.is_active() && m.role != "OWNER")
    {
        remove_in(
            db,
            ctx,
            conversation,
            principal,
            false,
            reason,
            actor,
            request,
            now,
        )
        .await?;
    }
    db::exec(
        db,
        "INSERT INTO messaging_conversation_bans (conversation_id, principal_id, environment_id, reason, created_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (conversation_id, principal_id) \
         DO UPDATE SET reason = EXCLUDED.reason, created_at = EXCLUDED.created_at, lifted_at = NULL",
        vec![
            conversation.id.into(),
            principal.into(),
            ctx.scope.environment_id.into(),
            reason.into(),
            now.into(),
        ],
    )
    .await?;
    crate::metrics::inc("messaging_members_banned_total", &[]);
    Ok(())
}

/// Lifts a ban (the principal may be added again).
pub async fn unban_in<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    principal: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    Ok(db::exec(
        db,
        "UPDATE messaging_conversation_bans SET lifted_at = $3 WHERE conversation_id = $1 AND principal_id = $2 AND lifted_at IS NULL",
        vec![conversation_id.into(), principal.into(), now.into()],
    )
    .await?
        == 1)
}

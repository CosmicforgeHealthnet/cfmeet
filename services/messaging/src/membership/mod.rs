//! Conversation membership (Part V Workstream E, the authority Parts E–G build on).
//!
//! * Members are ACTIVE, REMOVED or LEFT; re-admission reactivates the row with a NEW
//!   history window (`visible_from_sequence` / `visible_from_event`): a member sees only
//!   what is committed after (re)joining (AD-07: no server-granted history).
//! * Roles: OWNER (exactly the conversation owner), ADMIN, MODERATOR, MEMBER, READ_ONLY
//!   (reads its window, sends no messages) and BOT_SERVICE (BOT_OR_SERVICE principals
//!   only). Administration needs `conversations.manage`; with an acting principal it must
//!   be an OWNER / ADMIN, except member moderation (remove, ban, unban, list bans), which a
//!   MODERATOR may also do to MEMBER / READ_ONLY / BOT_SERVICE members.
//! * Invitations (§35): an INVITED principal is not a member (no history, no events, no
//!   sends) until it accepts; invitations expire ([`INVITATION_TTL`]) and can be declined
//!   or revoked. `authority_source` records how a membership came to be.
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
    authority_source: String,
    invited_by_principal_id: Option<Uuid>,
    invitation_expires_at: Option<DateTime<Utc>>,
}

const MEMBER_VIEW_COLUMNS: &str =
    "principal_id, role, state, joined_at, send_restricted_until, version, \
     authority_source, invited_by_principal_id, invitation_expires_at";

fn member_view(m: &MemberViewRow) -> Value {
    json!({
        "principalId": m.principal_id,
        "role": m.role,
        "state": m.state,
        "joinedAt": rfc3339(m.joined_at),
        "sendRestrictedUntil": m.send_restricted_until.map(rfc3339),
        "version": m.version,
        "authoritySource": m.authority_source,
        "invitedByPrincipalId": m.invited_by_principal_id,
        "invitationExpiresAt": m.invitation_expires_at.filter(|_| m.state == "INVITED").map(rfc3339),
    })
}

/// How long an invitation stays open.
pub const INVITATION_TTL: chrono::Duration = chrono::Duration::days(7);

/// Roles an administrator may give (the OWNER is the conversation's owner, never given).
pub fn assignable_role(role: Option<&str>) -> Result<&'static str> {
    Ok(match role {
        None | Some("MEMBER") => "MEMBER",
        Some("ADMIN") => "ADMIN",
        Some("MODERATOR") => "MODERATOR",
        Some("READ_ONLY") => "READ_ONLY",
        Some("BOT_SERVICE") => "BOT_SERVICE",
        Some(_) => {
            return Err(Error::validation(
                "role must be MEMBER, ADMIN, MODERATOR, READ_ONLY or BOT_SERVICE",
            ))
        }
    })
}

/// BOT_SERVICE is only for BOT_OR_SERVICE principals, and they only get BOT_SERVICE or
/// READ_ONLY (a bot never administers or moderates).
fn role_fits_principal(role: &str, kind: &str) -> Result<()> {
    let bot = kind == "BOT_OR_SERVICE";
    match (role, bot) {
        ("BOT_SERVICE", false) => Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "BOT_SERVICE_REQUIRES_BOT_PRINCIPAL",
        )),
        ("ADMIN" | "MODERATOR", true) => Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "BOT_CANNOT_ADMINISTER",
        )),
        _ => Ok(()),
    }
}

/// The principal's kind (ACTIVE and messaging-capable), else a clear refusal.
async fn principal_kind(state: &AppState, ctx: &TrustedContext, principal: Uuid) -> Result<String> {
    match state.principals.resolve(&ctx.scope, principal).await {
        Ok(s @ PrincipalStatus::Active { .. }) if s.is_messaging_capable() => match s {
            PrincipalStatus::Active { kind } => Ok(kind),
            _ => unreachable!(),
        },
        Ok(_) => Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_NOT_AVAILABLE",
        )),
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable");
            Err(Error::code(ErrorCode::ServiceUnavailable))
        }
    }
}

/// Authorizes a moderator of `conversation` acting on a member of `target_role`: OWNER /
/// ADMIN always; a MODERATOR only on MEMBER / READ_ONLY / BOT_SERVICE members (or a
/// principal that is not a member). NOT_FOUND for a non-member principal.
async fn require_moderator<C: ConnectionTrait>(
    db: &C,
    conversation: &ConversationRow,
    admin: &Administrator,
    target_role: Option<&str>,
) -> Result<()> {
    if let Administrator::Principal(a) = admin {
        match access::membership(db, conversation.id, a.principal_id, false).await? {
            Some(m) if m.is_admin() => {}
            Some(m)
                if m.is_moderator()
                    && !matches!(target_role, Some("OWNER" | "ADMIN" | "MODERATOR")) => {}
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    /// 1–500 (default 100).
    #[serde(default)]
    pub limit: Option<i64>,
    /// `nextCursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

fn member_cursor(joined_at: chrono::DateTime<chrono::Utc>, principal: Uuid) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(format!("{}|{principal}", joined_at.timestamp_micros()))
}

fn parse_member_cursor(c: &str) -> Option<(chrono::DateTime<chrono::Utc>, Uuid)> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let raw = String::from_utf8(URL_SAFE_NO_PAD.decode(c).ok()?).ok()?;
    let (t, p) = raw.split_once('|')?;
    Some((
        chrono::DateTime::from_timestamp_micros(t.parse().ok()?)?,
        Uuid::parse_str(p).ok()?,
    ))
}

/// `GET /v1/conversations/{id}/members` — ACTIVE members in join order, keyset-paginated
/// (§30: large groups never return an unbounded list).
pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    reader: Option<&Acting>,
    conversation_id: Uuid,
    q: ListQuery,
) -> Result<Value> {
    match reader {
        Some(a) => {
            access::as_member(&state.db, ctx, conversation_id, a.principal_id).await?;
        }
        None => {
            access::in_scope(&state.db, ctx, conversation_id).await?;
        }
    }
    let limit = q.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(Error::validation("limit must be between 1 and 500"));
    }
    let after = match q.cursor.as_deref() {
        None => None,
        Some(c) => {
            Some(parse_member_cursor(c).ok_or(Error::validation("cursor is not a member cursor"))?)
        }
    };
    let rows: Vec<MemberViewRow> = db::all(
        &state.db,
        &format!(
            "SELECT {MEMBER_VIEW_COLUMNS} FROM messaging_conversation_members \
         WHERE conversation_id = $1 AND state = 'ACTIVE' \
           AND ($2::timestamptz IS NULL OR (joined_at, principal_id) > ($2, $3)) \
         ORDER BY joined_at, principal_id LIMIT $4"
        ),
        vec![
            conversation_id.into(),
            after.map(|a| a.0).into(),
            after.map(|a| a.1).into(),
            (limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page = &rows[..rows.len().min(limit as usize)];
    let next = more
        .then(|| {
            page.last()
                .map(|m| member_cursor(m.joined_at, m.principal_id))
        })
        .flatten();
    Ok(json!({ "members": page.iter().map(member_view).collect::<Vec<_>>(), "nextCursor": next }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddRequest {
    pub principal_id: Uuid,
    #[serde(default)]
    pub role: Option<String>,
    /// §35: invite instead of adding; the principal becomes a member when it accepts.
    #[serde(default)]
    pub invite: bool,
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
    let role = assignable_role(request.role.as_deref())?;
    // The new member must be an ACTIVE messaging principal of the SAME environment.
    let kind = principal_kind(state, ctx, request.principal_id).await?;
    role_fits_principal(role, &kind)?;
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
                if request.invite {
                    return invite_in(
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
                    .await;
                }
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
    require_admin(txn, conversation, admin).await?;
    activate(
        txn,
        st,
        ctx,
        admin,
        conversation,
        principal_id,
        role,
        "API",
        request_ctx,
        now,
    )
    .await
}

/// Makes `principal_id` an ACTIVE member with a fresh history window (new, re-admitted or
/// an accepted invitation), inside the caller's transaction holding the conversation lock.
#[allow(clippy::too_many_arguments)]
async fn activate<C: ConnectionTrait>(
    txn: &C,
    st: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation: &ConversationRow,
    principal_id: Uuid,
    role: &str,
    source: &str,
    request_ctx: &RequestContext,
    now: DateTime<Utc>,
) -> Result<(u16, Value)> {
    let conversation_id = conversation.id;
    let request = AddRequestRef { principal_id };
    access::require_accepting(conversation, now)?;
    let class = ContextClass::parse(&conversation.context_class)
        .ok_or(Error::code(ErrorCode::InternalError))?;
    let existing = access::membership(txn, conversation_id, request.principal_id, true).await?;
    // A DIRECT conversation's membership is fixed, except that the counterpart of a
    // message request joins by accepting it — and then sees the request from the start.
    let direct_request = class == ContextClass::Direct
        && source == "INVITATION"
        && existing.as_ref().is_some_and(Membership::is_invited);
    if class == ContextClass::Direct && !direct_request {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "DIRECT_MEMBERSHIP_FIXED",
        ));
    }
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
        "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = $1 \
           AND (state = 'ACTIVE' OR (state = 'INVITED' AND principal_id <> $2))",
        vec![conversation_id.into(), request.principal_id.into()],
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
    // The new member sees its own `member.added` event and everything after (the
    // counterpart of a message request: the whole conversation).
    let (next_sequence, visible_event) = if direct_request {
        (1, 0)
    } else {
        (next_sequence, cursor - 1)
    };
    if existing.is_some() {
        db::exec(
            txn,
            "UPDATE messaging_conversation_members SET state = 'ACTIVE', role = $3, removed_at = NULL, \
               removed_reason = NULL, send_restricted_until = NULL, visible_from_sequence = $4, \
               visible_from_event = $5, joined_at = $6, updated_at = $6, version = version + 1, \
               authority_source = $7, invitation_expires_at = NULL \
             WHERE conversation_id = $1 AND principal_id = $2",
            vec![
                conversation_id.into(),
                request.principal_id.into(),
                role.into(),
                next_sequence.into(),
                visible_event.into(),
                now.into(),
                source.into(),
            ],
        )
        .await?;
    } else {
        db::exec(
            txn,
            "INSERT INTO messaging_conversation_members (conversation_id, principal_id, environment_id, role, \
               joined_at, updated_at, visible_from_sequence, visible_from_event, authority_source) \
             VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8)",
            vec![
                conversation_id.into(),
                request.principal_id.into(),
                ctx.scope.environment_id.into(),
                role.into(),
                now.into(),
                next_sequence.into(),
                visible_event.into(),
                source.into(),
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
        if source == "INVITATION" { "MEMBER_INVITATION_ACCEPTED" } else { "MEMBER_ADDED" },
        conversation_id,
        json!({ "member_principal_id": request.principal_id, "role": role, "authority_source": source }),
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
        &format!(
            "SELECT {MEMBER_VIEW_COLUMNS} FROM messaging_conversation_members \
         WHERE conversation_id = $1 AND principal_id = $2"
        ),
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
                let target = access::membership(txn, conversation_id, principal, true).await?;
                if target.as_ref().is_some_and(Membership::is_invited) {
                    // Revoking an invitation (or the invitee declining it).
                    if !leaving {
                        require_admin(txn, &conversation, &admin).await?;
                    }
                    let outcome = if leaving { "DECLINED" } else { "REVOKED" };
                    close_invitation(
                        txn,
                        &ctx,
                        &conversation,
                        principal,
                        outcome,
                        admin.principal_id(),
                        &request_ctx,
                        now,
                    )
                    .await?;
                    record_audit(
                        txn,
                        &ctx,
                        &admin,
                        "MEMBER_INVITATION_CLOSED",
                        conversation_id,
                        json!({ "member_principal_id": principal, "outcome": outcome }),
                        &request_ctx,
                        now,
                    )
                    .await?;
                    return member_row(txn, conversation_id, principal).await;
                }
                if !leaving {
                    require_moderator(
                        txn,
                        &conversation,
                        &admin,
                        target.as_ref().map(|m| m.role.as_str()),
                    )
                    .await?;
                } else if !target.as_ref().is_some_and(|m| m.is_active()) {
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
    assignable_role(Some(role))?;
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
    let current = access::membership(db, conversation.id, principal, true).await?;
    if current.as_ref().is_some_and(Membership::is_invited) {
        close_invitation(
            db,
            ctx,
            conversation,
            principal,
            "REVOKED",
            actor,
            request,
            now,
        )
        .await?;
    }
    if current.is_some_and(|m| m.is_active() && m.role != "OWNER") {
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

// ---------------------------------------------------------------------------------------
// Part V I §113 — role changes and conversation bans (administrators).
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleChange {
    pub role: String,
}

/// `PATCH /v1/conversations/{id}/members/{principalId}`: ADMIN, MODERATOR, MEMBER,
/// READ_ONLY or BOT_SERVICE (never the owner; bots only BOT_SERVICE / READ_ONLY).
pub async fn change_role(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
    principal: Uuid,
    request: RoleChange,
) -> Result<Value> {
    let role = assignable_role(Some(request.role.as_str()))?;
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
                require_admin(txn, &conversation, &admin).await?;
                let member = access::membership(txn, conversation_id, principal, true)
                    .await?
                    .filter(|m| m.is_active())
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                if member.role == "OWNER" {
                    return Err(Error::reason(ErrorCode::PrincipalNotAuthorized, "OWNER_ROLE_FIXED"));
                }
                // Checked only once the conversation and the member are known (no oracle).
                role_fits_principal(role, &principal_kind(&st, &ctx, principal).await?)?;
                let changed = if member.role != request.role {
                    set_role_in(txn, &ctx, &conversation, principal, &request.role, &request_ctx, now).await?
                } else {
                    false
                };
                if changed {
                    record_audit(
                        txn,
                        &ctx,
                        &admin,
                        "MEMBER_ROLE_CHANGED",
                        conversation_id,
                        json!({ "member_principal_id": principal, "role": request.role }),
                        &request_ctx,
                        now,
                    )
                    .await?;
                }
                Ok(json!({ "conversationId": conversation_id, "principalId": principal, "role": request.role, "changed": changed }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BanRequest {
    pub principal_id: Uuid,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /v1/conversations/{id}/bans`: removes the member (if present) and keeps them
/// from being re-added until the ban is lifted. PRIVATE_E2EE removal still needs the
/// members' MLS commit (reported as `mlsRekeyRequired`).
pub async fn ban(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
    request: BanRequest,
) -> Result<Value> {
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| "BANNED_BY_ADMIN".into());
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
                let current = access::membership(txn, conversation_id, request.principal_id, true).await?;
                require_moderator(txn, &conversation, &admin, current.as_ref().filter(|m| m.is_active()).map(|m| m.role.as_str())).await?;
                if admin.principal_id() == Some(request.principal_id) {
                    return Err(Error::validation("an administrator cannot ban themselves"));
                }
                if current.as_ref().is_some_and(|m| m.role == "OWNER") {
                    return Err(Error::reason(ErrorCode::PrincipalNotAuthorized, "OWNER_ROLE_FIXED"));
                }
                if conversation.context_class == "DIRECT" {
                    return Err(Error::reason(ErrorCode::InvalidTransition, "DIRECT_MEMBERSHIP_FIXED"));
                }
                let was_active = current.as_ref().is_some_and(|m| m.is_active());
                ban_in(txn, &ctx, &conversation, request.principal_id, &reason, admin.principal_id(), &request_ctx, now).await?;
                record_audit(
                    txn,
                    &ctx,
                    &admin,
                    "MEMBER_BANNED",
                    conversation_id,
                    json!({ "member_principal_id": request.principal_id, "reason": reason }),
                    &request_ctx,
                    now,
                )
                .await?;
                Ok(json!({
                    "conversationId": conversation_id,
                    "principalId": request.principal_id,
                    "banned": true,
                    "reason": reason,
                    "mlsRekeyRequired": was_active && conversation.profile() == SecurityProfile::PrivateE2ee,
                }))
            })
        })
        .await
        .map_err(Error::from)
}

/// `DELETE /v1/conversations/{id}/bans/{principalId}` (does not re-add the member).
pub async fn unban(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
    principal: Uuid,
) -> Result<Value> {
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
                require_moderator(txn, &conversation, &admin, None).await?;
                let lifted = unban_in(txn, conversation_id, principal, now).await?;
                if lifted {
                    record_audit(
                        txn,
                        &ctx,
                        &admin,
                        "MEMBER_UNBANNED",
                        conversation_id,
                        json!({ "member_principal_id": principal }),
                        &request_ctx,
                        now,
                    )
                    .await?;
                }
                Ok(json!({ "conversationId": conversation_id, "principalId": principal, "banned": false, "changed": lifted }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(sea_orm::FromQueryResult)]
struct BanRow {
    principal_id: Uuid,
    reason: Option<String>,
    created_at: DateTime<Utc>,
}

/// `GET /v1/conversations/{id}/bans` (administrators).
pub async fn bans(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
) -> Result<Value> {
    let conversation = load(&state.db, ctx, conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    require_moderator(&state.db, &conversation, admin, None).await?;
    let rows: Vec<BanRow> = db::all(
        &state.db,
        "SELECT principal_id, reason, created_at FROM messaging_conversation_bans \
         WHERE conversation_id = $1 AND lifted_at IS NULL ORDER BY created_at",
        vec![conversation_id.into()],
    )
    .await?;
    Ok(
        json!({ "bans": rows.iter().map(|b| json!({ "principalId": b.principal_id, "reason": b.reason, "bannedAt": rfc3339(b.created_at) })).collect::<Vec<_>>() }),
    )
}

// ---------------------------------------------------------------------------------------
// Invitations (§35)
// ---------------------------------------------------------------------------------------

/// Invites `principal_id` (an OWNER / ADMIN or the application backend): an INVITED row
/// with no history window, counted toward the member limit, announced on the conversation
/// stream (`messaging.member.invited`). An ACTIVE member is returned unchanged (200).
#[allow(clippy::too_many_arguments)]
pub async fn invite_in<C: ConnectionTrait>(
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
    let existing = access::membership(txn, conversation.id, principal_id, true).await?;
    if existing.as_ref().is_some_and(Membership::is_active) {
        return Ok((200, member_row(txn, conversation.id, principal_id).await?));
    }
    if banned(txn, conversation.id, principal_id).await? {
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
        "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = $1 \
           AND state IN ('ACTIVE', 'INVITED') AND principal_id <> $2",
        vec![conversation.id.into(), principal_id.into()],
    )
    .await?;
    if count >= max.min(st.conversations.max_members) {
        return Err(Error::reason(ErrorCode::Conflict, "MEMBER_LIMIT_REACHED"));
    }
    let expires = now + INVITATION_TTL;
    let inviter = admin.principal_id();
    if existing.is_some() {
        db::exec(
            txn,
            "UPDATE messaging_conversation_members SET state = 'INVITED', role = $3, removed_at = NULL, removed_reason = NULL, \
               invited_by_principal_id = $4, invited_at = $5, invitation_expires_at = $6, updated_at = $5, \
               version = version + 1 WHERE conversation_id = $1 AND principal_id = $2",
            vec![conversation.id.into(), principal_id.into(), role.into(), inviter.into(), now.into(), expires.into()],
        )
        .await?;
    } else {
        db::exec(
            txn,
            "INSERT INTO messaging_conversation_members (conversation_id, principal_id, environment_id, role, state, \
               joined_at, updated_at, invited_by_principal_id, invited_at, invitation_expires_at) \
             VALUES ($1, $2, $3, $4, 'INVITED', $5, $5, $6, $5, $7)",
            vec![conversation.id.into(), principal_id.into(), ctx.scope.environment_id.into(), role.into(),
                 now.into(), inviter.into(), expires.into()],
        )
        .await?;
    }
    outbox::enqueue_conversation(
        txn,
        conversation.id,
        NewEvent {
            event_type: "messaging.member.invited",
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation.id,
            scope: scope_of(ctx),
            payload: json!({
                "conversation_id": conversation.id,
                "principal_id": principal_id,
                "role": role,
                "invited_by_principal_id": inviter,
                "expires_at": rfc3339(expires),
            }),
        },
        request_ctx,
        now,
    )
    .await?;
    record_audit(
        txn,
        ctx,
        admin,
        "MEMBER_INVITED",
        conversation.id,
        json!({ "member_principal_id": principal_id, "role": role }),
        request_ctx,
        now,
    )
    .await?;
    crate::metrics::inc("messaging_members_invited_total", &[]);
    Ok((201, member_row(txn, conversation.id, principal_id).await?))
}

/// Closes an open invitation (DECLINED by the invitee, REVOKED by an administrator, or
/// EXPIRED): state LEFT / REMOVED, announced as `messaging.member.invitation-closed`.
#[allow(clippy::too_many_arguments)]
async fn close_invitation<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    principal: Uuid,
    outcome: &str,
    actor: Option<Uuid>,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<()> {
    let n = db::exec(
        db,
        "UPDATE messaging_conversation_members SET state = $3, removed_at = $4, removed_reason = $5, updated_at = $4, \
           invitation_expires_at = NULL, version = version + 1 \
         WHERE conversation_id = $1 AND principal_id = $2 AND state = 'INVITED'",
        vec![
            conversation.id.into(),
            principal.into(),
            (if outcome == "DECLINED" { "LEFT" } else { "REMOVED" }).into(),
            now.into(),
            format!("INVITATION_{outcome}").into(),
        ],
    )
    .await?;
    if n != 1 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    outbox::enqueue_conversation(
        db,
        conversation.id,
        NewEvent {
            event_type: "messaging.member.invitation-closed",
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation.id,
            scope: scope_of(ctx),
            payload: json!({ "conversation_id": conversation.id, "principal_id": principal,
                             "outcome": outcome, "closed_by_principal_id": actor }),
        },
        request,
        now,
    )
    .await?;
    Ok(())
}

/// `POST /v1/conversations/{id}/invitation/accept` | `.../decline` — the invitee itself.
/// Accepting makes it an ACTIVE member with the invited role and a history window that
/// starts now (`authoritySource` INVITATION); an expired invitation is closed instead.
pub async fn answer_invitation(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    accept: bool,
) -> Result<Value> {
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    let request_ctx = request_context();
    let outcome = state
        .db
        .transaction::<_, std::result::Result<Value, Error>, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let conversation = load(txn, &ctx, conversation_id, true)
                    .await?
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                let invitee = Administrator::Principal(acting.clone());
                let row: Option<(String, Option<DateTime<Utc>>)> = {
                    #[derive(sea_orm::FromQueryResult)]
                    struct Inv {
                        role: String,
                        invitation_expires_at: Option<DateTime<Utc>>,
                    }
                    db::one::<Inv, _>(
                        txn,
                        "SELECT role, invitation_expires_at FROM messaging_conversation_members \
                         WHERE conversation_id = $1 AND principal_id = $2 AND state = 'INVITED' FOR UPDATE",
                        vec![conversation_id.into(), acting.principal_id.into()],
                    )
                    .await?
                    .map(|i| (i.role, i.invitation_expires_at))
                };
                // No open invitation looks like an unknown conversation.
                let Some((role, expires)) = row else {
                    return Err(Error::code(ErrorCode::NotFound));
                };
                if expires.is_some_and(|t| t <= now) {
                    close_invitation(txn, &ctx, &conversation, acting.principal_id, "EXPIRED", None, &request_ctx, now).await?;
                    // Committed (the expiry is recorded), then refused.
                    return Ok(Err(Error::reason(ErrorCode::Conflict, "INVITATION_EXPIRED")));
                }
                if !accept {
                    close_invitation(txn, &ctx, &conversation, acting.principal_id, "DECLINED", Some(acting.principal_id), &request_ctx, now).await?;
                    record_audit(txn, &ctx, &invitee, "MEMBER_INVITATION_CLOSED", conversation_id,
                        json!({ "member_principal_id": acting.principal_id, "outcome": "DECLINED" }), &request_ctx, now).await?;
                    return Ok(Ok(member_row(txn, conversation_id, acting.principal_id).await?));
                }
                if banned(txn, conversation_id, acting.principal_id).await? {
                    return Err(Error::reason(ErrorCode::PrincipalNotAuthorized, "PRINCIPAL_BANNED"));
                }
                let (_, view) = activate(txn, &st, &ctx, &invitee, &conversation, acting.principal_id, &role,
                    "INVITATION", &request_ctx, now).await?;
                Ok(Ok(view))
            })
        })
        .await
        .map_err(Error::from)?;
    outcome
}

#[derive(sea_orm::FromQueryResult)]
struct InvitationRow {
    conversation_id: Uuid,
    role: String,
    invited_by_principal_id: Option<Uuid>,
    invited_at: Option<DateTime<Utc>>,
    invitation_expires_at: Option<DateTime<Utc>>,
    context_class: String,
    security_profile: String,
}

/// `GET /v1/messaging/invitations` — the acting principal's open invitations.
pub async fn invitations(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let rows: Vec<InvitationRow> = db::all(
        &state.db,
        "SELECT m.conversation_id, m.role, m.invited_by_principal_id, m.invited_at, m.invitation_expires_at, \
                c.context_class, c.security_profile \
           FROM messaging_conversation_members m JOIN messaging_conversations c ON c.id = m.conversation_id \
          WHERE m.environment_id = $1 AND m.principal_id = $2 AND m.state = 'INVITED' AND m.invitation_expires_at > $3 \
            AND c.tenant_id = $4 AND c.application_id = $5 AND c.lifecycle_state = 'ACTIVE' \
          ORDER BY m.invited_at DESC LIMIT 200",
        vec![ctx.scope.environment_id.into(), acting.principal_id.into(), state.now().into(),
             ctx.scope.tenant_id.into(), ctx.scope.application_id.into()],
    )
    .await?;
    Ok(json!({ "invitations": rows.iter().map(|r| json!({
        "conversationId": r.conversation_id,
        "role": r.role,
        "invitedByPrincipalId": r.invited_by_principal_id,
        "invitedAt": r.invited_at.map(rfc3339),
        "expiresAt": r.invitation_expires_at.map(rfc3339),
        "contextClass": r.context_class,
        "securityProfile": r.security_profile,
    })).collect::<Vec<_>>() }))
}

//! Part V I §113 — reactions, pins, replies / threads and unread state.
//!
//! * **Reactions** are server rows for MANAGED conversations only. In PRIVATE_E2EE a
//!   reaction is an encrypted application message (the server must not learn who reacted
//!   with what), so the row API refuses it (`E2EE_REACTIONS_ARE_MESSAGES`).
//! * **Pins** reference message ids (any profile; administrators); no content.
//! * **Replies / threads**: `replyToMessageId` on send records the reply relation and the
//!   thread root (ids only). For PRIVATE_E2EE the relation is metadata the sending client
//!   chose to disclose; any quoted content stays inside its ciphertext.
//! * **Unread**: counted from the member's read cursor over visible, ACTIVE application
//!   messages of others (never content).
//!
//! Every change is one transaction with its durable conversation event.

pub mod http;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::{
        access, policy,
        service::{request_context, rfc3339, scope_of, ConversationRow},
    },
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    http::middleware::Acting,
    identity::TrustedContext,
    messages::{load_message, MessageRow},
    state::AppState,
};

pub const MAX_REACTIONS_PER_PRINCIPAL: i64 = 20;
pub const MAX_PINS: i64 = 50;

/// A reaction is one emoji grapheme sequence or a `:short_code:` (no whitespace, no
/// control characters, at most 64 bytes).
pub fn valid_reaction(r: &str) -> bool {
    if r.is_empty() || r.len() > 64 || r.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let short = r.len() >= 3
        && r.starts_with(':')
        && r.ends_with(':')
        && r[1..r.len() - 1].bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'+' | b'-')
        });
    // Emoji: no ASCII letters / digits (e.g. "👍", "❤️", "👩‍💻").
    let emoji =
        !r.is_ascii() && r.chars().count() <= 8 && !r.chars().any(|c| c.is_ascii_alphanumeric());
    short || emoji
}

async fn visible_message<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    message_id: Uuid,
    acting: &Acting,
) -> Result<(ConversationRow, MessageRow)> {
    let (conversation, member) =
        access::as_member(db, ctx, conversation_id, acting.principal_id).await?;
    let message = load_message(db, conversation.id, message_id, false)
        .await?
        .filter(|m| m.sequence >= member.visible_from_sequence && m.message_kind == "APPLICATION")
        .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok((conversation, message))
}

async fn event<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    event_type: &'static str,
    payload: Value,
    now: DateTime<Utc>,
) -> Result<()> {
    outbox::enqueue_conversation(
        txn,
        conversation_id,
        NewEvent {
            event_type,
            aggregate_type: "CONVERSATION",
            aggregate_id: conversation_id,
            scope: scope_of(ctx),
            payload,
        },
        &request_context(),
        now,
    )
    .await?;
    Ok(())
}

/// `PUT` (add: true) / `DELETE` (add: false) `/v1/conversations/{id}/messages/{mid}/reactions/{reaction}`.
pub async fn react(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
    reaction: String,
    add: bool,
) -> Result<Value> {
    if !valid_reaction(&reaction) {
        return Err(Error::validation(
            "reaction must be one emoji or a :short_code: (at most 64 bytes)",
        ));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (conversation, message) = visible_message(txn, &ctx, conversation_id, message_id, &acting).await?;
                if conversation.profile() != SecurityProfile::Managed {
                    return Err(Error::reason(ErrorCode::ContentRejected, "E2EE_REACTIONS_ARE_MESSAGES"));
                }
                // Turning reactions off stops new ones; existing ones can still be removed.
                if add && !policy::current(txn, ctx.scope.environment_id).await?.messages.reactions {
                    return Err(Error::reason(ErrorCode::FeatureDisabled, "REACTIONS_DISABLED"));
                }
                if add && !message.is_active() {
                    return Err(Error::reason(ErrorCode::MessageNotModifiable, "MESSAGE_NOT_ACTIVE"));
                }
                let changed = if add {
                    let mine = db::scalar_i64(
                        txn,
                        "SELECT count(*) FROM messaging_message_reactions WHERE message_id = $1 AND principal_id = $2",
                        vec![message.id.into(), acting.principal_id.into()],
                    )
                    .await?;
                    if mine >= MAX_REACTIONS_PER_PRINCIPAL {
                        return Err(Error::reason(ErrorCode::Conflict, "REACTION_LIMIT_REACHED"));
                    }
                    db::exec(
                        txn,
                        "INSERT INTO messaging_message_reactions (message_id, conversation_id, principal_id, reaction, created_at) \
                         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                        vec![message.id.into(), conversation.id.into(), acting.principal_id.into(), reaction.clone().into(), now.into()],
                    )
                    .await?
                        == 1
                } else {
                    db::exec(
                        txn,
                        "DELETE FROM messaging_message_reactions WHERE message_id = $1 AND principal_id = $2 AND reaction = $3",
                        vec![message.id.into(), acting.principal_id.into(), reaction.clone().into()],
                    )
                    .await?
                        == 1
                };
                if changed {
                    event(
                        txn,
                        &ctx,
                        conversation.id,
                        if add { "messaging.reaction.added" } else { "messaging.reaction.removed" },
                        json!({ "conversation_id": conversation.id, "message_id": message.id,
                                "principal_id": acting.principal_id, "reaction": reaction }),
                        now,
                    )
                    .await?;
                }
                Ok(json!({ "messageId": message.id, "reaction": reaction, "reacted": add, "changed": changed }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(FromQueryResult)]
struct ReactionCount {
    reaction: String,
    count: i64,
    mine: bool,
}

/// `GET /v1/conversations/{id}/messages/{mid}/reactions` (members): counts per reaction.
pub async fn reactions(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
) -> Result<Value> {
    let (_, message) = visible_message(&state.db, ctx, conversation_id, message_id, acting).await?;
    let rows: Vec<ReactionCount> = db::all(
        &state.db,
        "SELECT reaction, count(*) AS count, bool_or(principal_id = $2) AS mine FROM messaging_message_reactions \
         WHERE message_id = $1 GROUP BY reaction ORDER BY min(created_at), reaction",
        vec![message.id.into(), acting.principal_id.into()],
    )
    .await?;
    Ok(
        json!({ "messageId": message.id, "reactions": rows.iter().map(|r| json!({ "reaction": r.reaction, "count": r.count, "reactedByMe": r.mine })).collect::<Vec<_>>() }),
    )
}

/// `PUT` / `DELETE /v1/conversations/{id}/pins/{messageId}` (OWNER / ADMIN).
pub async fn pin(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    message_id: Uuid,
    add: bool,
) -> Result<Value> {
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let (conversation, member) = access::as_member(txn, &ctx, conversation_id, acting.principal_id).await?;
                if !member.is_admin() {
                    return Err(Error::reason(ErrorCode::PrincipalNotAuthorized, "NOT_ADMIN"));
                }
                if add && !policy::current(txn, ctx.scope.environment_id).await?.messages.pins {
                    return Err(Error::reason(ErrorCode::FeatureDisabled, "PINS_DISABLED"));
                }
                let changed = if add {
                    let message = load_message(txn, conversation.id, message_id, false)
                        .await?
                        .filter(|m| m.is_active() && m.message_kind == "APPLICATION")
                        .ok_or(Error::code(ErrorCode::NotFound))?;
                    let pinned = db::scalar_i64(
                        txn,
                        "SELECT count(*) FROM messaging_message_pins WHERE conversation_id = $1",
                        vec![conversation.id.into()],
                    )
                    .await?;
                    if pinned >= MAX_PINS {
                        return Err(Error::reason(ErrorCode::Conflict, "PIN_LIMIT_REACHED"));
                    }
                    db::exec(
                        txn,
                        "INSERT INTO messaging_message_pins (conversation_id, message_id, pinned_by_principal_id, pinned_at) \
                         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                        vec![conversation.id.into(), message.id.into(), acting.principal_id.into(), now.into()],
                    )
                    .await?
                        == 1
                } else {
                    db::exec(
                        txn,
                        "DELETE FROM messaging_message_pins WHERE conversation_id = $1 AND message_id = $2",
                        vec![conversation.id.into(), message_id.into()],
                    )
                    .await?
                        == 1
                };
                if changed {
                    event(
                        txn,
                        &ctx,
                        conversation.id,
                        if add { "messaging.pin.added" } else { "messaging.pin.removed" },
                        json!({ "conversation_id": conversation.id, "message_id": message_id, "principal_id": acting.principal_id }),
                        now,
                    )
                    .await?;
                }
                Ok(json!({ "conversationId": conversation.id, "messageId": message_id, "pinned": add, "changed": changed }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(FromQueryResult)]
struct PinRow {
    message_id: Uuid,
    pinned_by_principal_id: Uuid,
    pinned_at: DateTime<Utc>,
    sequence: i64,
}

/// `GET /v1/conversations/{id}/pins` (members; only messages inside their history window).
pub async fn pins(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
) -> Result<Value> {
    let (conversation, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let rows: Vec<PinRow> = db::all(
        &state.db,
        "SELECT p.message_id, p.pinned_by_principal_id, p.pinned_at, m.sequence FROM messaging_message_pins p \
           JOIN messaging_messages m ON m.id = p.message_id \
         WHERE p.conversation_id = $1 AND m.sequence >= $2 AND m.lifecycle = 'ACTIVE' ORDER BY p.pinned_at",
        vec![conversation.id.into(), member.visible_from_sequence.into()],
    )
    .await?;
    Ok(
        json!({ "pins": rows.iter().map(|p| json!({ "messageId": p.message_id, "sequence": p.sequence,
        "pinnedByPrincipalId": p.pinned_by_principal_id, "pinnedAt": rfc3339(p.pinned_at) })).collect::<Vec<_>>() }),
    )
}

#[derive(FromQueryResult)]
struct UnreadRow {
    unread: i64,
    last_sequence: Option<i64>,
    read_sequence: Option<i64>,
}

/// `GET /v1/conversations/{id}/unread` (members).
pub async fn unread(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
) -> Result<Value> {
    let (conversation, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    let row: UnreadRow = db::one(
        &state.db,
        "SELECT (SELECT count(*) FROM messaging_messages m WHERE m.conversation_id = $1 AND m.message_kind = 'APPLICATION' \
                   AND m.lifecycle = 'ACTIVE' AND m.sender_principal_id <> $2 AND m.sequence >= $3 \
                   AND m.sequence > COALESCE((SELECT read_sequence FROM messaging_read_cursors WHERE conversation_id = $1 AND principal_id = $2), 0) \
                   AND NOT EXISTS (SELECT 1 FROM messaging_message_hidden h WHERE h.message_id = m.id AND h.principal_id = $2)) AS unread, \
                (SELECT max(sequence) FROM messaging_messages WHERE conversation_id = $1) AS last_sequence, \
                (SELECT read_sequence FROM messaging_read_cursors WHERE conversation_id = $1 AND principal_id = $2) AS read_sequence",
        vec![conversation.id.into(), acting.principal_id.into(), member.visible_from_sequence.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(
        json!({ "conversationId": conversation.id, "unreadCount": row.unread,
               "lastSequence": row.last_sequence.unwrap_or(0), "readSequence": row.read_sequence.unwrap_or(0) }),
    )
}

/// Records the reply relation of a new message (same transaction as the message).
/// Returns the reloaded row. The target must be a visible application message of the
/// same conversation. With threads turned off by policy (§47) the reply is kept but no
/// thread root is recorded.
pub async fn set_reply<C: ConnectionTrait>(
    txn: &C,
    conversation: &ConversationRow,
    row: MessageRow,
    reply_to: Option<Uuid>,
    threads: bool,
) -> Result<MessageRow> {
    let Some(target) = reply_to else {
        return Ok(row);
    };
    let visible_from_sequence =
        access::membership(txn, conversation.id, row.sender_principal_id, false)
            .await?
            .map(|m| m.visible_from_sequence)
            .unwrap_or(i64::MAX);
    #[derive(FromQueryResult)]
    struct Target {
        id: Uuid,
        thread_root_id: Option<Uuid>,
    }
    let t: Target = db::one(
        txn,
        "SELECT id, thread_root_id FROM messaging_messages WHERE id = $1 AND conversation_id = $2 \
           AND message_kind = 'APPLICATION' AND sequence >= $3 AND sequence < $4",
        vec![
            target.into(),
            conversation.id.into(),
            visible_from_sequence.into(),
            row.sequence.into(),
        ],
    )
    .await?
    .ok_or(Error::reason(ErrorCode::NotFound, "REPLY_TARGET_NOT_FOUND"))?;
    db::exec(
        txn,
        "UPDATE messaging_messages SET reply_to_message_id = $2, thread_root_id = $3 WHERE id = $1",
        vec![
            row.id.into(),
            t.id.into(),
            threads.then(|| t.thread_root_id.unwrap_or(t.id)).into(),
        ],
    )
    .await?;
    load_message(txn, conversation.id, row.id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))
}

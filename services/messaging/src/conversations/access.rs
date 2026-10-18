//! Authoritative membership checks shared by every conversation-scoped operation
//! (messages, MLS, realtime subscriptions, attachments, search, reports).
//!
//! A principal that is not an ACTIVE member of a conversation in the caller's scope is
//! told `NOT_FOUND` — indistinguishable from a conversation that does not exist — so no
//! operation leaks a conversation's existence or membership.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use uuid::Uuid;

use super::{service::ConversationRow, LifecycleState};
use crate::{
    db,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
};

/// One principal's membership row.
#[derive(Debug, Clone, FromQueryResult)]
pub struct Membership {
    pub principal_id: Uuid,
    pub role: String,
    pub state: String,
    pub send_restricted_until: Option<DateTime<Utc>>,
    pub visible_from_sequence: i64,
    pub visible_from_event: i64,
    pub version: i64,
}

impl Membership {
    pub fn is_active(&self) -> bool {
        self.state == "ACTIVE"
    }

    pub fn is_admin(&self) -> bool {
        self.is_active() && (self.role == "OWNER" || self.role == "ADMIN")
    }

    pub fn may_send(&self, now: DateTime<Utc>) -> bool {
        self.is_active() && self.send_restricted_until.is_none_or(|t| t <= now)
    }
}

const MEMBER_COLUMNS: &str =
    "principal_id, role, state, send_restricted_until, visible_from_sequence, \
     visible_from_event, version";

/// The membership row of `principal` (any state), optionally locked.
pub async fn membership<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    principal_id: Uuid,
    lock: bool,
) -> Result<Option<Membership>> {
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

/// The ACTIVE members of a conversation.
pub async fn active_members<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
) -> Result<Vec<Membership>> {
    Ok(db::all(
        db,
        &format!(
            "SELECT {MEMBER_COLUMNS} FROM messaging_conversation_members WHERE conversation_id = $1 AND state = 'ACTIVE' \
             ORDER BY principal_id"
        ),
        vec![conversation_id.into()],
    )
    .await?)
}

/// The conversation (in the caller's scope) and `principal`'s ACTIVE membership of it;
/// `NOT_FOUND` otherwise.
pub async fn as_member<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    principal_id: Uuid,
) -> Result<(ConversationRow, Membership)> {
    let conversation = super::service::load(db, ctx, conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    match membership(db, conversation_id, principal_id, false).await? {
        Some(m) if m.is_active() => Ok((conversation, m)),
        _ => Err(Error::code(ErrorCode::NotFound)),
    }
}

/// The conversation in the caller's scope (administration view, no principal).
pub async fn in_scope<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    conversation_id: Uuid,
) -> Result<ConversationRow> {
    super::service::load(db, ctx, conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))
}

/// New messages are accepted only in ACTIVE conversations.
pub fn require_accepting(conversation: &ConversationRow, now: DateTime<Utc>) -> Result<()> {
    let state = conversation.state();
    if !state.accepts_messages() || conversation.expires_at.is_some_and(|t| t <= now) {
        return Err(Error::reason(
            ErrorCode::ConversationNotActive,
            match state {
                LifecycleState::Locked => "LOCKED",
                LifecycleState::Closed => "CLOSED",
                LifecycleState::Archived => "ARCHIVED",
                LifecycleState::Deleted => "DELETED",
                _ => "EXPIRED",
            },
        ));
    }
    Ok(())
}

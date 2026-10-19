//! MeetingMessagingService (Part F §20–§27).
//!
//! The published meeting messaging API depends only on this service. Storage sits
//! behind [`MessageStore`]; the current adapter is [`PgMessageStore`]
//! (`conference_messages`). Phase 5 Unified Messaging can replace the adapter without
//! changing the `/api/v2/conferences/{id}/messages` contract. The legacy v1
//! `chat_messages` table (bound to legacy meetings/participants) is left untouched.
//!
//! Identity: the sender is ALWAYS the caller's own ADMITTED ConferenceParticipant —
//! request bodies carry content and an optional recipient / breakout selector only.
//!
//! Scope (every message has a `conference_id`):
//!
//! | scope    | stored as                               | delivered to (server-side audience) |
//! |----------|-----------------------------------------|-------------------------------------|
//! | MAIN     | `breakout_room_id NULL, recipient NULL` | main-room subscribers + `chat.moderate` |
//! | BREAKOUT | `breakout_room_id = B`                  | members/visitors of B + `breakout.enter_any` |
//! | PRIVATE  | `recipient = R`                         | sender and R only (never moderators) |
//!
//! Pagination is database-level, newest first, cursor = (`created_at`, `id`).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::AdmissionState,
    error::{ApiError, ApiResult, ErrorCode},
    models::{breakout_rooms, conference_participants},
    services::{
        actor::ActorContext,
        meeting_core::{
            find_scoped_participant, find_space, lock_conference, permission_denied, Core,
        },
        meeting_events::{action, audit, emit_to, event, Audience, AuditRecord, OutboxRecord},
        meeting_permissions::{can_moderate, Permission},
    },
};

use super::{
    breakouts::{media_context, BreakoutStatus, MediaContext},
    effective_policy,
    metrics::{inc, COLLAB_METRICS as CM},
    policy::{ChatSendMode, CollaborationPolicy, PrivateChatMode},
    Collab, RateAction,
};

pub const MAX_PAGE: u64 = 100;
pub const DEFAULT_PAGE: u64 = 50;
pub const MAX_BODY: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessageKind {
    Message,
    Announcement,
    BreakoutAnnouncement,
}

impl MessageKind {
    fn as_str(&self) -> &'static str {
        match self {
            MessageKind::Message => "MESSAGE",
            MessageKind::Announcement => "ANNOUNCEMENT",
            MessageKind::BreakoutAnnouncement => "BREAKOUT_ANNOUNCEMENT",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "ANNOUNCEMENT" => MessageKind::Announcement,
            "BREAKOUT_ANNOUNCEMENT" => MessageKind::BreakoutAnnouncement,
            _ => MessageKind::Message,
        }
    }
}

/// Where a message lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageScope {
    Main,
    Breakout(Uuid),
    /// Conversation with this peer (from the reader's / sender's point of view).
    Private(Uuid),
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub breakout_room_id: Option<Uuid>,
    pub sender_participant_id: Uuid,
    pub recipient_participant_id: Option<Uuid>,
    pub kind: MessageKind,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewMessage {
    pub conference_id: Uuid,
    pub breakout_room_id: Option<Uuid>,
    pub sender_participant_id: Uuid,
    pub recipient_participant_id: Option<Uuid>,
    pub kind: MessageKind,
    pub body: String,
}

/// Stable cursor over (created_at, id), newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

impl Cursor {
    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "{}|{}",
            self.created_at.timestamp_micros(),
            self.id
        ))
    }

    pub fn decode(s: &str) -> Option<Self> {
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .ok()?;
        let raw = String::from_utf8(raw).ok()?;
        let (ts, id) = raw.split_once('|')?;
        Some(Self {
            created_at: DateTime::from_timestamp_micros(ts.parse().ok()?)?,
            id: Uuid::parse_str(id).ok()?,
        })
    }
}

/// Storage adapter (replaceable by Phase 5 Unified Messaging).
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Work a store must do BEFORE the send transaction takes its row locks (the
    /// conference and the sender): e.g. resolving the scope's conversation and its
    /// membership, whose rows reference those locked rows. Default: nothing.
    async fn prepare(
        &self,
        _conference_id: Uuid,
        _breakout_room_id: Option<Uuid>,
        _sender_participant_id: Uuid,
        _recipient_participant_id: Option<Uuid>,
    ) -> ApiResult<()> {
        Ok(())
    }
    async fn insert(&self, txn: &DatabaseTransaction, m: NewMessage) -> ApiResult<StoredMessage>;
    /// One page, newest first, strictly older than `before`. `reader` scopes private
    /// conversations to (reader, peer).
    async fn page(
        &self,
        db: &DatabaseConnection,
        conference_id: Uuid,
        scope: MessageScope,
        reader: Option<Uuid>,
        before: Option<Cursor>,
        limit: u64,
    ) -> ApiResult<Vec<StoredMessage>>;
    async fn find(
        &self,
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        message_id: Uuid,
    ) -> ApiResult<Option<StoredMessage>>;
    /// Soft delete; returns whether the row changed.
    async fn soft_delete(
        &self,
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        message_id: Uuid,
        by: &str,
    ) -> ApiResult<bool>;
}

pub struct PgMessageStore;

fn row_to_message(r: &sea_orm::QueryResult) -> ApiResult<StoredMessage> {
    let kind: String = r.try_get_by_index(5)?;
    Ok(StoredMessage {
        id: r.try_get_by_index(0)?,
        conference_id: r.try_get_by_index(1)?,
        breakout_room_id: r.try_get_by_index(2)?,
        sender_participant_id: r.try_get_by_index(3)?,
        recipient_participant_id: r.try_get_by_index(4)?,
        kind: MessageKind::parse(&kind),
        body: r.try_get_by_index(6)?,
        created_at: r.try_get_by_index(7)?,
        deleted_at: r.try_get_by_index(8)?,
    })
}

const COLUMNS: &str = "id, conference_id, breakout_room_id, sender_participant_id, \
     recipient_participant_id, kind, body, created_at, deleted_at";

#[async_trait]
impl MessageStore for PgMessageStore {
    async fn insert(&self, txn: &DatabaseTransaction, m: NewMessage) -> ApiResult<StoredMessage> {
        let row = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "INSERT INTO conference_messages (id, conference_id, breakout_room_id, sender_participant_id, \
                     recipient_participant_id, kind, body) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING {COLUMNS}"
                ),
                [
                    Uuid::new_v4().into(),
                    m.conference_id.into(),
                    m.breakout_room_id.into(),
                    m.sender_participant_id.into(),
                    m.recipient_participant_id.into(),
                    m.kind.as_str().into(),
                    m.body.into(),
                ],
            ))
            .await?
            .ok_or_else(|| ApiError::InternalError("message insert returned nothing".into()))?;
        row_to_message(&row)
    }

    async fn page(
        &self,
        db: &DatabaseConnection,
        conference_id: Uuid,
        scope: MessageScope,
        reader: Option<Uuid>,
        before: Option<Cursor>,
        limit: u64,
    ) -> ApiResult<Vec<StoredMessage>> {
        let mut values: Vec<sea_orm::Value> = vec![conference_id.into()];
        let mut sql = format!("SELECT {COLUMNS} FROM conference_messages WHERE conference_id = $1");
        match scope {
            MessageScope::Main => {
                sql.push_str(" AND breakout_room_id IS NULL AND recipient_participant_id IS NULL")
            }
            MessageScope::Breakout(b) => {
                values.push(b.into());
                sql.push_str(&format!(
                    " AND breakout_room_id = ${} AND recipient_participant_id IS NULL",
                    values.len()
                ));
            }
            MessageScope::Private(peer) => {
                let me = reader
                    .ok_or_else(|| permission_denied("Private messages need a participant"))?;
                values.push(me.into());
                values.push(peer.into());
                let (a, b) = (values.len() - 1, values.len());
                sql.push_str(&format!(
                    " AND recipient_participant_id IS NOT NULL AND ((sender_participant_id = ${a} AND recipient_participant_id = ${b}) \
                       OR (sender_participant_id = ${b} AND recipient_participant_id = ${a}))"
                ));
            }
        }
        if let Some(c) = before {
            values.push(c.created_at.into());
            values.push(c.id.into());
            let (t, i) = (values.len() - 1, values.len());
            sql.push_str(&format!(" AND (created_at, id) < (${t}, ${i})"));
        }
        values.push((limit.min(MAX_PAGE) as i64).into());
        sql.push_str(&format!(
            " ORDER BY created_at DESC, id DESC LIMIT ${}",
            values.len()
        ));
        let rows = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?;
        rows.iter().map(row_to_message).collect()
    }

    async fn find(
        &self,
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        message_id: Uuid,
    ) -> ApiResult<Option<StoredMessage>> {
        let row = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!("SELECT {COLUMNS} FROM conference_messages WHERE id = $1 AND conference_id = $2 FOR UPDATE"),
                [message_id.into(), conference_id.into()],
            ))
            .await?;
        row.as_ref().map(row_to_message).transpose()
    }

    async fn soft_delete(
        &self,
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        message_id: Uuid,
        by: &str,
    ) -> ApiResult<bool> {
        Ok(txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE conference_messages SET deleted_at = now(), deleted_by = $3 \
                 WHERE id = $1 AND conference_id = $2 AND deleted_at IS NULL",
                [message_id.into(), conference_id.into(), by.into()],
            ))
            .await?
            .rows_affected()
            > 0)
    }
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendMessageRequest {
    pub body: String,
    /// Private message recipient (conference-scoped participant id).
    pub recipient_participant_id: Option<Uuid>,
    /// Breakout chat of this room (must be the sender's current breakout).
    pub breakout_id: Option<Uuid>,
    /// Host-only announcement to the main room (`chat.moderate`).
    #[serde(default)]
    pub announcement: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub breakout_id: Option<Uuid>,
    pub sender_participant_id: Uuid,
    pub sender_display_name: String,
    pub recipient_participant_id: Option<Uuid>,
    pub kind: MessageKind,
    /// `null` once deleted by moderation.
    pub body: Option<String>,
    pub deleted: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub messages: Vec<MessageView>,
    /// Pass as `before` to fetch older messages; absent at the end.
    pub next_cursor: Option<String>,
}

fn audience_for(m: &StoredMessage) -> Audience {
    match (m.recipient_participant_id, m.breakout_room_id) {
        (Some(r), _) => Audience::Participants {
            ids: vec![m.sender_participant_id, r],
            moderator_permission: None,
        },
        (None, Some(b)) => Audience::Breakout { breakout_id: b },
        (None, None) => Audience::MainRoom,
    }
}

fn message_payload(m: &StoredMessage, sender_name: &str) -> serde_json::Value {
    json!({
        "messageId": m.id,
        "conferenceId": m.conference_id,
        "breakoutId": m.breakout_room_id,
        "senderParticipantId": m.sender_participant_id,
        "senderDisplayName": sender_name,
        "recipientParticipantId": m.recipient_participant_id,
        "kind": m.kind,
        "body": m.body,
        "createdAt": m.created_at,
    })
}

/// Pure send-policy evaluation (unit tested).
pub fn check_send_policy(
    policy: &CollaborationPolicy,
    moderator: bool,
    private: bool,
    peer_is_moderator: bool,
    breakout: bool,
) -> Result<(), ErrorCode> {
    if private {
        return match policy.private_chat_mode {
            PrivateChatMode::Disabled => Err(ErrorCode::PrivateChatDisabled),
            PrivateChatMode::HostsOnly if !moderator && !peer_is_moderator => {
                Err(ErrorCode::PrivateChatDisabled)
            }
            _ if !policy.chat_enabled && !moderator => Err(ErrorCode::ChatDisabled),
            _ => Ok(()),
        };
    }
    if breakout && !policy.breakout_chat_enabled && !moderator {
        return Err(ErrorCode::ChatDisabled);
    }
    if !moderator && (!policy.chat_enabled || policy.chat_send_mode == ChatSendMode::HostsOnly) {
        return Err(ErrorCode::ChatDisabled);
    }
    Ok(())
}

pub struct MeetingMessagingService {
    core: Core,
    store: Arc<dyn MessageStore>,
    /// Part V H: Unified Messaging (main-room chat authority once cut over).
    unified: Option<(
        Arc<crate::services::unified_messaging::UnifiedMessaging>,
        Arc<dyn MessageStore>,
    )>,
}

impl MeetingMessagingService {
    pub fn new(core: Core) -> Self {
        Self::with_store(core, Arc::new(PgMessageStore))
    }

    pub fn with_store(core: Core, store: Arc<dyn MessageStore>) -> Self {
        Self {
            core,
            store,
            unified: None,
        }
    }

    /// Routes main-room chat through Unified Messaging while it is authoritative.
    pub fn with_unified(
        mut self,
        unified: Option<Arc<crate::services::unified_messaging::UnifiedMessaging>>,
    ) -> Self {
        self.unified = unified.map(|um| {
            let store: Arc<dyn MessageStore> = Arc::new(
                crate::services::unified_messaging::store::UnifiedMessageStore::new(um.clone()),
            );
            (um, store)
        });
        self
    }

    /// The store of meeting chat now, every scope (legacy until the operator cuts over).
    async fn main_store(&self) -> ApiResult<Arc<dyn MessageStore>> {
        if let Some((um, store)) = &self.unified {
            if um.state().await?.messaging_authoritative() {
                return Ok(store.clone());
            }
        }
        Ok(self.store.clone())
    }

    /// A legacy write that lost a race with the cutover is refused by the database
    /// (`CHAT_MIGRATED`); the client retries and lands on Messaging.
    fn cutover_race(e: ApiError) -> ApiError {
        if e.to_string().contains("CHAT_MIGRATED") {
            return ApiError::domain(
                ErrorCode::ChatUnavailable,
                "Chat is switching over; please retry",
            );
        }
        e
    }

    async fn active_breakout(
        &self,
        conference_id: Uuid,
        breakout_id: Uuid,
    ) -> ApiResult<breakout_rooms::Model> {
        use sea_orm::{EntityTrait, QueryFilter};
        breakout_rooms::Entity::find_by_id(breakout_id)
            .filter(breakout_rooms::Column::ConferenceId.eq(conference_id))
            .one(&self.core.db)
            .await?
            .ok_or_else(|| ApiError::domain(ErrorCode::BreakoutNotFound, "Breakout room not found"))
    }

    fn denied(code: ErrorCode) -> ApiError {
        let msg = match code {
            ErrorCode::PrivateChatDisabled => "Private chat is not available in this meeting",
            _ => "Chat is not available to you in this meeting",
        };
        inc(&CM.chat_rejects);
        ApiError::domain(code, msg)
    }

    pub async fn send_message(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: SendMessageRequest,
    ) -> ApiResult<MessageView> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        c.require(
            Permission::ChatSend,
            "Your role does not allow sending messages",
        )?;
        c.require_live()?;
        let body = req.body.trim().to_string();
        if body.is_empty() || body.chars().count() > MAX_BODY {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                format!("Messages must be 1–{MAX_BODY} characters"),
            ));
        }
        if req.recipient_participant_id.is_some() && req.breakout_id.is_some() {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "A message is either private or breakout-scoped",
            ));
        }
        if req.announcement && (req.recipient_participant_id.is_some() || req.breakout_id.is_some())
        {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Announcements go to the main room",
            ));
        }
        let moderator = c.allows(Permission::ChatModerate);
        if req.announcement && !moderator {
            return Err(permission_denied("Only hosts can post announcements"));
        }
        // Scope validation before the rate limit so foreign ids are always NOT_FOUND.
        let recipient = match req.recipient_participant_id {
            Some(r) => {
                let peer = find_scoped_participant(&self.core.db, conference_id, r, false).await?;
                if peer.id == me.id {
                    return Err(ApiError::domain(
                        ErrorCode::ValidationError,
                        "You cannot message yourself",
                    ));
                }
                if peer.admission_state != AdmissionState::Admitted {
                    return Err(ApiError::domain(
                        ErrorCode::ParticipantNotFound,
                        "Participant not found in this conference",
                    ));
                }
                c.require(
                    Permission::ChatPrivate,
                    "Your role does not allow private messages",
                )?;
                Some(peer)
            }
            None => None,
        };
        if let Some(b) = req.breakout_id {
            let room = self.active_breakout(conference_id, b).await?;
            if !BreakoutStatus::parse(&room.status).is_active() {
                return Err(ApiError::domain(
                    ErrorCode::BreakoutClosed,
                    "This breakout room is closed",
                ));
            }
        }
        self.core
            .collab_limiter
            .check(RateAction::Chat, &me.id.to_string())
            .await?;

        // Every chat scope follows the migration (main room, breakout rooms, private).
        let store = self.main_store().await?;
        store
            .prepare(
                conference_id,
                req.breakout_id,
                me.id,
                recipient.as_ref().map(|p| p.id),
            )
            .await?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        // Re-read sender, policy and context under the conference lock: a restriction
        // or policy change that committed first always wins (C11).
        let sender = find_scoped_participant(&txn, conference_id, me.id, true).await?;
        if sender.admission_state != AdmissionState::Admitted {
            return Err(permission_denied(
                "You are no longer a participant of this conference",
            ));
        }
        if sender.chat_restricted_at.is_some() && !moderator {
            inc(&CM.chat_rejects);
            return Err(ApiError::domain(
                ErrorCode::ChatParticipantRestricted,
                "A host has disabled chat for you",
            ));
        }
        let policy = effective_policy(
            &find_space(&txn, conference.meeting_space_id).await?,
            &conference,
        );
        let peer_moderator = recipient
            .as_ref()
            .map(|p| c.pctx.allows(p.role, Permission::ChatModerate))
            .unwrap_or(false);
        check_send_policy(
            &policy,
            moderator,
            recipient.is_some(),
            peer_moderator,
            req.breakout_id.is_some(),
        )
        .map_err(Self::denied)?;
        let my_ctx = media_context(&txn, &conference, &sender).await?;
        let enter_any = c.allows(Permission::BreakoutEnterAny);
        match (req.breakout_id, &my_ctx) {
            (Some(b), MediaContext::Breakout { breakout_id, .. }) if *breakout_id == b => {}
            (Some(b), _) => {
                let room = self.active_breakout(conference_id, b).await?;
                if !(enter_any && BreakoutStatus::parse(&room.status).is_active()) {
                    return Err(permission_denied(
                        "You can only post in your own breakout room",
                    ));
                }
            }
            (None, MediaContext::Breakout { visit: false, .. })
                if recipient.is_none() && !moderator =>
            {
                return Err(permission_denied(
                    "You are in a breakout room; post to your breakout chat",
                ));
            }
            _ => {}
        }
        if let (
            Some(peer),
            MediaContext::Breakout {
                breakout_id,
                visit: false,
                ..
            },
        ) = (&recipient, &my_ctx)
        {
            let peer_ctx = media_context(&txn, &conference, peer).await?;
            if peer_ctx.breakout_id() != Some(*breakout_id) && !peer_moderator && !moderator {
                return Err(permission_denied(
                    "Private messages from a breakout stay inside the breakout",
                ));
            }
        }
        let stored = store
            .insert(
                &txn,
                NewMessage {
                    conference_id,
                    breakout_room_id: req.breakout_id,
                    sender_participant_id: sender.id,
                    recipient_participant_id: recipient.as_ref().map(|p| p.id),
                    kind: if req.announcement {
                        MessageKind::Announcement
                    } else {
                        MessageKind::Message
                    },
                    body,
                },
            )
            .await
            .map_err(Self::cutover_race)?;
        emit_to(
            &txn,
            OutboxRecord {
                aggregate_type: "conference_message",
                aggregate_id: stored.id,
                event_type: event::CHAT_MESSAGE_CREATED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference_id),
                payload: message_payload(&stored, &sender.display_name),
            },
            Some(audience_for(&stored)),
        )
        .await?;
        if req.announcement {
            audit(
                &txn,
                &c.audit,
                AuditRecord {
                    meeting_space_id: conference.meeting_space_id,
                    conference_id: Some(conference_id),
                    action: action::CHAT_ANNOUNCEMENT,
                    target_type: Some("MESSAGE"),
                    target_ref: Some(stored.id.to_string()),
                    metadata: json!({ "length": stored.body.chars().count() }),
                },
            )
            .await?;
        }
        txn.commit()
            .await
            .map_err(|e| Self::cutover_race(e.into()))?;
        inc(&CM.chat_sends);
        Ok(view(&stored, &sender.display_name))
    }

    /// Authorize reading `scope` for the caller.
    async fn authorize_read(&self, c: &Collab, scope: MessageScope) -> ApiResult<()> {
        c.require(
            Permission::ChatRead,
            "Your role does not allow reading chat",
        )?;
        let moderator = c.allows(Permission::ChatModerate);
        let ctx = match &c.participant {
            Some(p) => Some(media_context(&self.core.db, &c.conference, p).await?),
            None => None,
        };
        match scope {
            MessageScope::Main => {
                if !c.policy.chat_enabled && !moderator {
                    return Err(Self::denied(ErrorCode::ChatDisabled));
                }
                if let Some(MediaContext::Breakout { visit: false, .. }) = ctx {
                    if !c.policy.main_chat_visible_in_breakouts && !moderator {
                        return Err(permission_denied(
                            "Main chat is hidden while you are in a breakout room",
                        ));
                    }
                }
            }
            MessageScope::Breakout(b) => {
                self.active_breakout(c.conference.id, b).await?;
                let mine = ctx.as_ref().and_then(|x| x.breakout_id()) == Some(b);
                if !mine && !c.allows(Permission::BreakoutEnterAny) {
                    return Err(permission_denied(
                        "You can only read your own breakout chat",
                    ));
                }
            }
            MessageScope::Private(peer) => {
                c.me()?;
                find_scoped_participant(&self.core.db, c.conference.id, peer, false).await?;
            }
        }
        Ok(())
    }

    pub async fn list_messages(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        scope: MessageScope,
        before: Option<&str>,
        limit: Option<u64>,
    ) -> ApiResult<MessagePage> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        self.authorize_read(&c, scope).await?;
        let before =
            match before {
                Some(s) => Some(Cursor::decode(s).ok_or_else(|| {
                    ApiError::domain(ErrorCode::ValidationError, "Invalid cursor")
                })?),
                None => None,
            };
        let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        let store = self.main_store().await?;
        let rows = store
            .page(
                &self.core.db,
                conference_id,
                scope,
                c.participant.as_ref().map(|p| p.id),
                before,
                limit,
            )
            .await?;
        let names = self.names(conference_id, &rows).await?;
        let next_cursor = (rows.len() as u64 == limit)
            .then(|| {
                rows.last().map(|m| {
                    Cursor {
                        created_at: m.created_at,
                        id: m.id,
                    }
                    .encode()
                })
            })
            .flatten();
        Ok(MessagePage {
            messages: rows
                .iter()
                .map(|m| {
                    view(
                        m,
                        names
                            .get(&m.sender_participant_id)
                            .map(String::as_str)
                            .unwrap_or(""),
                    )
                })
                .collect(),
            next_cursor,
        })
    }

    async fn names(
        &self,
        conference_id: Uuid,
        rows: &[StoredMessage],
    ) -> ApiResult<std::collections::HashMap<Uuid, String>> {
        use sea_orm::{EntityTrait, QueryFilter};
        let ids: Vec<Uuid> = rows.iter().map(|m| m.sender_participant_id).collect();
        Ok(conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::Id.is_in(ids))
            .all(&self.core.db)
            .await?
            .into_iter()
            .map(|p| (p.id, p.display_name))
            .collect())
    }

    /// Moderators hide main/breakout messages; senders may delete their own messages.
    /// Private messages are never visible to moderators and can only be deleted by
    /// their sender. Idempotent.
    pub async fn delete_message(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        message_id: Uuid,
    ) -> ApiResult<bool> {
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        // Migrated messages (any scope) live in Messaging; older unmigrated rows here.
        let main = self.main_store().await?;
        let (m, store) = match main.find(&txn, conference_id, message_id).await? {
            Some(m) if !Arc::ptr_eq(&main, &self.store) => (m, main),
            _ => (
                self.store
                    .find(&txn, conference_id, message_id)
                    .await?
                    .ok_or_else(|| {
                        ApiError::domain(ErrorCode::MessageNotFound, "Message not found")
                    })?,
                self.store.clone(),
            ),
        };
        let own = c.participant.as_ref().map(|p| p.id) == Some(m.sender_participant_id);
        if m.recipient_participant_id.is_some() {
            let party = c.participant.as_ref().map(|p| p.id);
            if party != Some(m.sender_participant_id) && party != m.recipient_participant_id {
                // Not a party to the conversation: indistinguishable from unknown.
                return Err(ApiError::domain(
                    ErrorCode::MessageNotFound,
                    "Message not found",
                ));
            }
            if !own {
                return Err(permission_denied(
                    "Only the sender can delete a private message",
                ));
            }
        } else if !own {
            c.require(
                Permission::ChatModerate,
                "Your role does not allow moderating chat",
            )?;
            let sender =
                find_scoped_participant(&txn, conference_id, m.sender_participant_id, false)
                    .await?;
            if !can_moderate(c.role, sender.role) {
                return Err(permission_denied(
                    "You can only moderate less privileged participants",
                ));
            }
        }
        let changed = store
            .soft_delete(&txn, conference_id, message_id, &c.audit.reference())
            .await?;
        if changed {
            emit_to(
                &txn,
                OutboxRecord {
                    aggregate_type: "conference_message",
                    aggregate_id: m.id,
                    event_type: event::CHAT_MESSAGE_DELETED,
                    meeting_space_id: Some(conference.meeting_space_id),
                    conference_id: Some(conference_id),
                    payload: json!({ "messageId": m.id, "breakoutId": m.breakout_room_id, "byModerator": !own }),
                },
                Some(audience_for(&m)),
            )
            .await?;
            if !own {
                audit(
                    &txn,
                    &c.audit,
                    AuditRecord {
                        meeting_space_id: conference.meeting_space_id,
                        conference_id: Some(conference_id),
                        action: action::CHAT_MESSAGE_DELETE,
                        target_type: Some("MESSAGE"),
                        target_ref: Some(m.id.to_string()),
                        metadata: json!({ "senderParticipantId": m.sender_participant_id }),
                    },
                )
                .await?;
                inc(&CM.chat_moderations);
            }
        }
        txn.commit().await?;
        Ok(changed)
    }

    /// Disable / re-enable chat for one participant (idempotent).
    pub async fn moderate_participant_chat(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
        restricted: bool,
    ) -> ApiResult<bool> {
        use sea_orm::{ActiveModelTrait, Set};
        let c = Collab::load(&self.core, ctx, conference_id).await?;
        let target =
            find_scoped_participant(&self.core.db, conference_id, participant_id, false).await?;
        c.require(
            Permission::ChatModerate,
            "Your role does not allow moderating chat",
        )?;
        if c.participant.as_ref().map(|p| p.id) == Some(target.id)
            || !can_moderate(c.role, target.role)
        {
            return Err(permission_denied(
                "You can only moderate less privileged participants",
            ));
        }
        c.require_configurable()?;
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let p = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        if p.chat_restricted_at.is_some() == restricted {
            txn.commit().await?;
            return Ok(false);
        }
        let now = Utc::now();
        let mut am: conference_participants::ActiveModel = p.clone().into();
        am.chat_restricted_at = Set(restricted.then_some(now));
        am.chat_restricted_by = Set(restricted.then(|| c.audit.reference()));
        am.collab_version = Set(p.collab_version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: if restricted {
                    action::CHAT_PARTICIPANT_RESTRICT
                } else {
                    action::CHAT_PARTICIPANT_UNRESTRICT
                },
                target_type: Some("PARTICIPANT"),
                target_ref: Some(updated.id.to_string()),
                metadata: json!({}),
            },
        )
        .await?;
        emit_to(
            &txn,
            OutboxRecord {
                aggregate_type: "conference_participant",
                aggregate_id: updated.id,
                event_type: if restricted {
                    event::CHAT_PARTICIPANT_RESTRICTED
                } else {
                    event::CHAT_PARTICIPANT_UNRESTRICTED
                },
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference_id),
                payload: json!({ "participantId": updated.id, "restricted": restricted }),
            },
            Some(Audience::Participants {
                ids: vec![updated.id],
                moderator_permission: Some(Permission::ChatModerate.key().into()),
            }),
        )
        .await?;
        txn.commit().await?;
        inc(&CM.chat_moderations);
        Ok(true)
    }
}

fn view(m: &StoredMessage, sender_name: &str) -> MessageView {
    MessageView {
        id: m.id,
        conference_id: m.conference_id,
        breakout_id: m.breakout_room_id,
        sender_participant_id: m.sender_participant_id,
        sender_display_name: sender_name.to_string(),
        recipient_participant_id: m.recipient_participant_id,
        kind: m.kind,
        body: m.deleted_at.is_none().then(|| m.body.clone()),
        deleted: m.deleted_at.is_some(),
        created_at: m.created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MeetingType;

    #[test]
    fn cursor_roundtrip_and_rejects_garbage() {
        let c = Cursor {
            created_at: Utc::now(),
            id: Uuid::new_v4(),
        };
        let back = Cursor::decode(&c.encode()).unwrap();
        assert_eq!(back.id, c.id);
        assert_eq!(
            back.created_at.timestamp_micros(),
            c.created_at.timestamp_micros()
        );
        assert!(Cursor::decode("not-a-cursor").is_none());
        assert!(Cursor::decode("").is_none());
    }

    #[test]
    fn send_policy_matrix() {
        let mut p = CollaborationPolicy::defaults(MeetingType::Standard);
        assert!(check_send_policy(&p, false, false, false, false).is_ok());
        p.chat_send_mode = ChatSendMode::HostsOnly;
        assert_eq!(
            check_send_policy(&p, false, false, false, false),
            Err(ErrorCode::ChatDisabled)
        );
        assert!(
            check_send_policy(&p, true, false, false, false).is_ok(),
            "announcements"
        );
        p.chat_send_mode = ChatSendMode::Everyone;
        p.private_chat_mode = PrivateChatMode::Disabled;
        assert_eq!(
            check_send_policy(&p, false, true, false, false),
            Err(ErrorCode::PrivateChatDisabled)
        );
        assert_eq!(
            check_send_policy(&p, true, true, false, false),
            Err(ErrorCode::PrivateChatDisabled)
        );
        p.private_chat_mode = PrivateChatMode::HostsOnly;
        assert_eq!(
            check_send_policy(&p, false, true, false, false),
            Err(ErrorCode::PrivateChatDisabled)
        );
        assert!(
            check_send_policy(&p, false, true, true, false).is_ok(),
            "attendee → host allowed"
        );
        p.breakout_chat_enabled = false;
        assert_eq!(
            check_send_policy(&p, false, false, false, true),
            Err(ErrorCode::ChatDisabled)
        );
        p.chat_enabled = false;
        assert_eq!(
            check_send_policy(&p, false, false, false, false),
            Err(ErrorCode::ChatDisabled)
        );
    }

    #[test]
    fn private_messages_never_reach_moderators() {
        let m = StoredMessage {
            id: Uuid::new_v4(),
            conference_id: Uuid::new_v4(),
            breakout_room_id: None,
            sender_participant_id: Uuid::new_v4(),
            recipient_participant_id: Some(Uuid::new_v4()),
            kind: MessageKind::Message,
            body: "x".into(),
            created_at: Utc::now(),
            deleted_at: None,
        };
        match audience_for(&m) {
            Audience::Participants {
                ids,
                moderator_permission,
            } => {
                assert_eq!(ids.len(), 2);
                assert!(moderator_permission.is_none());
            }
            other => panic!("{other:?}"),
        }
    }
}

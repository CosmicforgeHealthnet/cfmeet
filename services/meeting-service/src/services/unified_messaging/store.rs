//! Part V H §99 — the compatibility adapter: Messaging behind Meeting's existing
//! [`MessageStore`] port, so `/api/v2/conferences/{id}/messages` keeps its contract.
//!
//! Used for MAIN-room chat while Messaging is authoritative (MESSAGING_PRIMARY /
//! LEGACY_RETIRED). Meeting still decides who may send, read and moderate (its roles,
//! policy, restrictions and breakout rules run before the store is called); Messaging
//! stores and orders the canonical message. Meeting keeps ids only
//! (`conference_chat_message_links`), never a second body copy.
//!
//! Id stability: a message imported during shadow validation keeps its legacy id in the
//! Meeting API; a Messaging-native message is exposed with its Messaging id.

use async_trait::async_trait;
use axum::http::Method;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DatabaseTransaction};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::{
    chat_error, exec, one, parse_time, Acting, ConferenceLink, Peer, PrincipalKind,
    UnifiedMessaging,
};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        collaboration::messaging::{
            Cursor, MessageKind, MessageScope, MessageStore, NewMessage, StoredMessage,
        },
        ops_metrics::{inc, OPS_METRICS as OM},
    },
};

pub struct UnifiedMessageStore {
    um: Arc<UnifiedMessaging>,
}

impl UnifiedMessageStore {
    pub fn new(um: Arc<UnifiedMessaging>) -> Self {
        Self { um }
    }

    fn owner(link: &ConferenceLink) -> Acting {
        Acting {
            principal_id: link.owner_principal_id,
            kind: PrincipalKind::NativeCfUser,
        }
    }

    /// (Messaging id, the id the Meeting API exposes) of a message referenced by either.
    async fn resolve_id<C: sea_orm::ConnectionTrait>(
        db: &C,
        conference_id: Uuid,
        id: Uuid,
    ) -> ApiResult<Option<(Uuid, Uuid, i64)>> {
        one(
            db,
            "SELECT messaging_message_id, COALESCE(legacy_message_id, messaging_message_id), messaging_sequence \
               FROM conference_chat_message_links \
             WHERE conference_id = $1 AND (messaging_message_id = $2 OR legacy_message_id = $2) LIMIT 1",
            vec![conference_id.into(), id.into()],
        )
        .await?
        .map(|r| -> ApiResult<_> {
            Ok((r.try_get_by_index(0)?, r.try_get_by_index(1)?, r.try_get_by_index(2)?))
        })
        .transpose()
    }

    async fn to_stored(&self, conference_id: Uuid, m: &Value) -> ApiResult<Option<StoredMessage>> {
        let Some(mid) = m["messageId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Ok(None);
        };
        if m["kind"] != "APPLICATION" {
            return Ok(None);
        }
        let exposed = match Self::resolve_id(&self.um.db, conference_id, mid).await? {
            Some((_, exposed, _)) => exposed,
            None => mid,
        };
        let sender = self
            .um
            .participant_of(
                conference_id,
                m["senderPrincipalId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok()),
            )
            .await?;
        let Some(sender) = sender else {
            return Ok(None);
        };
        let created = parse_time(&m["originalCreatedAt"])
            .or_else(|| parse_time(&m["createdAt"]))
            .unwrap_or_else(Utc::now);
        let active = m["lifecycle"] == "ACTIVE";
        Ok(Some(StoredMessage {
            id: exposed,
            conference_id,
            breakout_room_id: None,
            sender_participant_id: sender,
            recipient_participant_id: None,
            kind: if m["managed"]["presentation"] == "ANNOUNCEMENT" {
                MessageKind::Announcement
            } else {
                MessageKind::Message
            },
            body: m["managed"]["text"].as_str().unwrap_or("").to_string(),
            created_at: created,
            deleted_at: (!active).then(|| parse_time(&m["lifecycleChangedAt"]).unwrap_or(created)),
        }))
    }
}

fn main_only(m: &NewMessage) -> ApiResult<()> {
    if m.breakout_room_id.is_some() || m.recipient_participant_id.is_some() {
        return Err(ApiError::InternalError(
            "the unified store serves main-room chat only".into(),
        ));
    }
    Ok(())
}

#[async_trait]
impl MessageStore for UnifiedMessageStore {
    async fn insert(&self, txn: &DatabaseTransaction, m: NewMessage) -> ApiResult<StoredMessage> {
        main_only(&m)?;
        let um = &self.um;
        let link = um.ensure_link(&um.db, m.conference_id).await?;
        // The sender's current admission and role, before the message (on demand: the
        // background sync may not have run since they were admitted).
        um.sync_participant(&link, m.sender_participant_id).await?;
        use sea_orm::EntityTrait;
        let participant =
            crate::models::conference_participants::Entity::find_by_id(m.sender_participant_id)
                .one(&um.db)
                .await?
                .ok_or_else(|| ApiError::InternalError("sender vanished".into()))?;
        let (principal, kind) = um.principal_for_participant(&participant).await?;
        let mut body = json!({
            "clientMessageId": Uuid::new_v4(),
            "text": m.body,
            // Meeting delivers its own realtime events; no extra device notifications.
            "notify": false,
        });
        if m.kind == MessageKind::Announcement {
            body["managedPresentation"] = json!("ANNOUNCEMENT");
        }
        let v = um
            .gateway
            .call(
                Peer::Messaging,
                Some(Acting {
                    principal_id: principal,
                    kind,
                }),
                Method::POST,
                &format!(
                    "/internal/v1/delegated/v1/conversations/{}/messages",
                    link.conversation_id
                ),
                &[],
                Some(&body),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        let mid = v["messageId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| {
                ApiError::domain(ErrorCode::ChatUnavailable, "Unexpected Messaging answer")
            })?;
        let sequence = v["sequence"].as_i64().unwrap_or(0);
        let created: DateTime<Utc> = parse_time(&v["createdAt"]).unwrap_or_else(Utc::now);
        exec(
            txn,
            "INSERT INTO conference_chat_message_links (messaging_message_id, conference_id, messaging_sequence, origin) \
             VALUES ($1, $2, $3, 'MESSAGING_NATIVE') ON CONFLICT DO NOTHING",
            vec![mid.into(), m.conference_id.into(), sequence.into()],
        )
        .await?;
        inc(&OM.unified_messaging_messages_sent);
        Ok(StoredMessage {
            id: mid,
            conference_id: m.conference_id,
            breakout_room_id: None,
            sender_participant_id: m.sender_participant_id,
            recipient_participant_id: None,
            kind: m.kind,
            body: m.body,
            created_at: created,
            deleted_at: None,
        })
    }

    async fn page(
        &self,
        _db: &DatabaseConnection,
        conference_id: Uuid,
        scope: MessageScope,
        _reader: Option<Uuid>,
        before: Option<Cursor>,
        limit: u64,
    ) -> ApiResult<Vec<StoredMessage>> {
        if scope != MessageScope::Main {
            return Err(ApiError::InternalError(
                "the unified store serves main-room chat only".into(),
            ));
        }
        let um = &self.um;
        let link = um.ensure_link(&um.db, conference_id).await?;
        let before_sequence = match before {
            Some(c) => match Self::resolve_id(&um.db, conference_id, c.id).await? {
                Some((_, _, seq)) => seq,
                None => {
                    return Err(ApiError::domain(
                        ErrorCode::ValidationError,
                        "Invalid cursor",
                    ))
                }
            },
            None => i64::MAX,
        };
        let mut out = Vec::new();
        let mut cursor = before_sequence;
        // Meeting already authorized this read (roles, policy, breakout visibility);
        // reading as the conversation owner reproduces meeting chat semantics (every
        // admitted participant sees the main-room history).
        while (out.len() as u64) < limit {
            let cursor_s = cursor.to_string();
            let page_limit = (limit - out.len() as u64).clamp(1, 100).to_string();
            let page = um
                .gateway
                .call(
                    Peer::Messaging,
                    Some(Self::owner(&link)),
                    Method::GET,
                    &format!(
                        "/internal/v1/delegated/v1/conversations/{}/messages",
                        link.conversation_id
                    ),
                    &[
                        ("beforeSequence", cursor_s.as_str()),
                        ("limit", page_limit.as_str()),
                    ],
                    None,
                )
                .await
                .map_err(|e| chat_error(&e))?;
            for m in page["messages"].as_array().cloned().unwrap_or_default() {
                if let Some(s) = self.to_stored(conference_id, &m).await? {
                    out.push(s);
                }
            }
            match page["nextBeforeSequence"].as_i64() {
                Some(n) if n < cursor => cursor = n,
                _ => break,
            }
        }
        out.truncate(limit as usize);
        Ok(out)
    }

    async fn find(
        &self,
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        message_id: Uuid,
    ) -> ApiResult<Option<StoredMessage>> {
        let um = &self.um;
        let Some((mid, _, _)) = Self::resolve_id(txn, conference_id, message_id).await? else {
            return Ok(None);
        };
        let Some(link) = um.link_of(txn, conference_id).await? else {
            return Ok(None);
        };
        let v = match um
            .gateway
            .call(
                Peer::Messaging,
                Some(Self::owner(&link)),
                Method::GET,
                &format!(
                    "/internal/v1/delegated/v1/conversations/{}/messages/{mid}",
                    link.conversation_id
                ),
                &[],
                None,
            )
            .await
        {
            Ok(v) => v,
            Err(super::GatewayError::Refused { code, .. }) if code == "NOT_FOUND" => {
                return Ok(None)
            }
            Err(e) => return Err(chat_error(&e)),
        };
        self.to_stored(conference_id, &v["message"]).await
    }

    async fn soft_delete(
        &self,
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        message_id: Uuid,
        _by: &str,
    ) -> ApiResult<bool> {
        let um = &self.um;
        let Some((mid, _, _)) = Self::resolve_id(txn, conference_id, message_id).await? else {
            return Ok(false);
        };
        let Some(link) = um.link_of(txn, conference_id).await? else {
            return Ok(false);
        };
        let v = um
            .gateway
            .call(
                Peer::Messaging,
                None,
                Method::POST,
                &format!(
                    "/internal/v1/meeting-conversations/{}/messages/{mid}/moderation",
                    link.conversation_id
                ),
                &[],
                Some(&json!({ "action": "REMOVE", "reason": "MEETING_CHAT_DELETED" })),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        Ok(v["changed"] == json!(true))
    }
}

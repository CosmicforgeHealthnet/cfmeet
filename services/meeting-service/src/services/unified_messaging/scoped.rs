//! Part V remediation — breakout and private meeting chat in Unified Messaging.
//!
//! The main-room conversation of a conference is [`super::ConferenceLink`]. Each other
//! chat scope gets its OWN generic MEETING-context conversation (no product-specific
//! Messaging model), resolved idempotently by an opaque `meetingRef`:
//!
//! | scope | meetingRef | owner | members |
//! |---|---|---|---|
//! | breakout room B | `conference:{c}:breakout:{B}` | the meeting space owner (as main) | participants assigned to B; hosts who entered B (`breakout.enter_any`) |
//! | private chat of P and Q | `conference:{c}:private:{low}:{high}` | the environment's system principal (`BOT_OR_SERVICE`) | exactly P and Q |
//!
//! Meeting still decides who may send and read (roles, policy, breakout rules) before
//! the store is called; the membership synced here is what Messaging enforces for a
//! participant's OWN client access (devices, client tokens, realtime). A private chat
//! never has a moderator or the meeting owner as member, and either party loses access
//! when they leave the meeting. Membership updates are version-fenced exactly like the
//! main room's (one global sequence, drawn under the scope link's row lock).
//!
//! Database authorization is not cryptographic revocation: these conversations are
//! MANAGED (server-readable, disclosed), never relabelled as end-to-end encrypted.

use std::collections::{HashMap, HashSet};

use axum::http::Method;
use chrono::Utc;
use sea_orm::{ConnectionTrait, EntityTrait, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    all, chat_error, conversation_role, conversation_state, exec, one, rfc3339, Acting,
    GatewayError, Peer, PrincipalKind, UnifiedMessaging,
};
use crate::{
    domain::AdmissionState,
    error::{ApiError, ApiResult, ErrorCode},
    models::conference_participants,
    services::ops_metrics::{add, inc, OPS_METRICS as OM},
};

/// A non-main chat scope of a conference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatScope {
    Breakout(Uuid),
    /// A private chat between two participants (always stored sorted).
    Private(Uuid, Uuid),
}

impl ChatScope {
    pub fn private(a: Uuid, b: Uuid) -> Self {
        if a < b {
            Self::Private(a, b)
        } else {
            Self::Private(b, a)
        }
    }

    /// The scope of a legacy / new message row.
    pub fn of(breakout: Option<Uuid>, sender: Uuid, recipient: Option<Uuid>) -> Option<Self> {
        match (breakout, recipient) {
            (_, Some(r)) => Some(Self::private(sender, r)),
            (Some(b), None) => Some(Self::Breakout(b)),
            (None, None) => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Breakout(_) => "BREAKOUT",
            Self::Private(..) => "PRIVATE",
        }
    }

    pub fn meeting_ref(&self, conference_id: Uuid) -> String {
        match self {
            Self::Breakout(b) => format!("conference:{conference_id}:breakout:{b}"),
            Self::Private(a, b) => format!("conference:{conference_id}:private:{a}:{b}"),
        }
    }

    /// The other party of a private chat.
    pub fn peer_of(&self, participant: Uuid) -> Option<Uuid> {
        match *self {
            Self::Private(a, b) if a == participant => Some(b),
            Self::Private(a, b) if b == participant => Some(a),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScopeLink {
    pub conference_id: Uuid,
    pub conversation_id: Uuid,
    pub scope: ChatScope,
    pub owner_principal_id: Uuid,
}

const LINK_COLUMNS: &str =
    "conversation_id, conference_id, scope_kind, breakout_room_id, participant_low, \
     participant_high, owner_principal_id";

fn link_from(r: &sea_orm::QueryResult) -> ApiResult<ScopeLink> {
    let kind: String = r.try_get_by_index(2)?;
    let scope = if kind == "BREAKOUT" {
        ChatScope::Breakout(r.try_get_by_index(3)?)
    } else {
        ChatScope::Private(r.try_get_by_index(4)?, r.try_get_by_index(5)?)
    };
    Ok(ScopeLink {
        conversation_id: r.try_get_by_index(0)?,
        conference_id: r.try_get_by_index(1)?,
        scope,
        owner_principal_id: r.try_get_by_index(6)?,
    })
}

/// The external reference of the environment's meeting-chat system principal.
pub const SYSTEM_PRINCIPAL_REFERENCE: &str = "cfmeet-meeting-chat-system";

impl UnifiedMessaging {
    /// The environment's non-human owner of private chats (created on first use).
    pub async fn system_principal(&self) -> ApiResult<Uuid> {
        self.ensure_principal(
            "SYSTEM",
            Uuid::nil(),
            PrincipalKind::BotOrService,
            SYSTEM_PRINCIPAL_REFERENCE.to_string(),
        )
        .await
    }

    pub async fn scope_link_of<C: ConnectionTrait>(
        &self,
        db: &C,
        conference_id: Uuid,
        scope: ChatScope,
    ) -> ApiResult<Option<ScopeLink>> {
        let row = match scope {
            ChatScope::Breakout(b) => {
                one(
                    db,
                    &format!("SELECT {LINK_COLUMNS} FROM conference_chat_scope_links \
                     WHERE conference_id = $1 AND scope_kind = 'BREAKOUT' AND breakout_room_id = $2"),
                    vec![conference_id.into(), b.into()],
                )
                .await?
            }
            ChatScope::Private(a, b) => {
                one(
                    db,
                    &format!("SELECT {LINK_COLUMNS} FROM conference_chat_scope_links \
                     WHERE conference_id = $1 AND scope_kind = 'PRIVATE' AND participant_low = $2 AND participant_high = $3"),
                    vec![conference_id.into(), a.into(), b.into()],
                )
                .await?
            }
        };
        row.as_ref().map(link_from).transpose()
    }

    pub async fn scope_link_by_conversation<C: ConnectionTrait>(
        &self,
        db: &C,
        conversation_id: Uuid,
    ) -> ApiResult<Option<ScopeLink>> {
        one(
            db,
            &format!(
                "SELECT {LINK_COLUMNS} FROM conference_chat_scope_links WHERE conversation_id = $1"
            ),
            vec![conversation_id.into()],
        )
        .await?
        .as_ref()
        .map(link_from)
        .transpose()
    }

    /// Resolves (idempotently, in Messaging) and records the scope's conversation.
    pub async fn ensure_scope_link(
        &self,
        conference_id: Uuid,
        scope: ChatScope,
    ) -> ApiResult<ScopeLink> {
        if let Some(l) = self.scope_link_of(&self.db, conference_id, scope).await? {
            return Ok(l);
        }
        let owner = match scope {
            // The meeting space owner, exactly as for the main room.
            ChatScope::Breakout(_) => {
                self.ensure_link(&self.db, conference_id)
                    .await?
                    .owner_principal_id
            }
            // Nobody who can read the meeting's other chats owns a private chat.
            ChatScope::Private(..) => self.system_principal().await?,
        };
        let body = json!({
            "meetingRef": scope.meeting_ref(conference_id),
            "ownerPrincipalId": owner,
            "securityProfile": "MANAGED",
            "managedDisclosureAcknowledged": true,
        });
        let v = self
            .gateway
            .call(
                Peer::Messaging,
                None,
                Method::POST,
                "/internal/v1/meeting-conversations",
                &[],
                Some(&body),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        let conversation_id = v["conversation"]["conversationId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| {
                ApiError::domain(ErrorCode::ChatUnavailable, "Unexpected Messaging answer")
            })?;
        let (breakout, low, high) = match scope {
            ChatScope::Breakout(b) => (Some(b), None, None),
            ChatScope::Private(a, b) => (None, Some(a), Some(b)),
        };
        exec(
            &self.db,
            "INSERT INTO conference_chat_scope_links (conversation_id, conference_id, scope_kind, breakout_room_id, \
               participant_low, participant_high, environment_id, owner_principal_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
            vec![
                conversation_id.into(),
                conference_id.into(),
                scope.kind().into(),
                breakout.into(),
                low.into(),
                high.into(),
                self.environment_id().into(),
                owner.into(),
            ],
        )
        .await?;
        if v["created"] == json!(true) {
            inc(&OM.unified_messaging_conversations_linked);
        }
        self.scope_link_of(&self.db, conference_id, scope)
            .await?
            .ok_or_else(|| ApiError::InternalError("chat scope link vanished".into()))
    }

    /// Who should be an ACTIVE member of the scope's conversation now:
    /// participant → (role, visitor).
    async fn desired_members<C: ConnectionTrait>(
        &self,
        db: &C,
        link: &ScopeLink,
    ) -> ApiResult<HashMap<Uuid, (&'static str, bool)>> {
        let mut out = HashMap::new();
        match link.scope {
            ChatScope::Breakout(b) => {
                for r in all(
                    db,
                    "SELECT p.id, p.role FROM breakout_assignments a JOIN conference_participants p ON p.id = a.participant_id \
                     WHERE a.breakout_room_id = $1 AND a.unassigned_at IS NULL AND p.admission_state = 'ADMITTED'",
                    vec![b.into()],
                )
                .await?
                {
                    let role: String = r.try_get_by_index(1)?;
                    let role = crate::domain::Role::parse(&role).map(conversation_role).unwrap_or("MEMBER");
                    out.insert(r.try_get_by_index(0)?, (role, false));
                }
                // Hosts who entered the room keep access while they are admitted hosts.
                for r in all(
                    db,
                    "SELECT p.id, p.role FROM conference_chat_scope_member_sync s JOIN conference_participants p ON p.id = s.participant_id \
                     WHERE s.conversation_id = $1 AND s.visitor AND p.admission_state = 'ADMITTED'",
                    vec![link.conversation_id.into()],
                )
                .await?
                {
                    let id: Uuid = r.try_get_by_index(0)?;
                    let role: String = r.try_get_by_index(1)?;
                    if crate::domain::Role::parse(&role).map(conversation_role) == Some("ADMIN") {
                        out.entry(id).or_insert(("ADMIN", true));
                    }
                }
            }
            ChatScope::Private(a, b) => {
                for r in all(
                    db,
                    "SELECT id FROM conference_participants WHERE id IN ($1, $2) AND admission_state = 'ADMITTED'",
                    vec![a.into(), b.into()],
                )
                .await?
                {
                    out.insert(r.try_get_by_index(0)?, ("MEMBER", false));
                }
            }
        }
        Ok(out)
    }

    /// Brings the scope conversation's membership to the desired state. `visitor`: a
    /// host entering a breakout room they are not assigned to (marked so the next sync
    /// keeps them while they remain an admitted host). Returns how many updates were sent.
    pub async fn sync_scope_members(
        &self,
        link: &ScopeLink,
        visitor: Option<Uuid>,
    ) -> ApiResult<u32> {
        let txn = self.db.begin().await?;
        exec(
            &txn,
            "SELECT 1 FROM conference_chat_scope_links WHERE conversation_id = $1 FOR UPDATE",
            vec![link.conversation_id.into()],
        )
        .await?;
        let mut desired = self.desired_members(&txn, link).await?;
        if let Some(v) = visitor {
            if let std::collections::hash_map::Entry::Vacant(e) = desired.entry(v) {
                if let Some(p) = conference_participants::Entity::find_by_id(v)
                    .one(&txn)
                    .await?
                {
                    e.insert((conversation_role(p.role), true));
                }
            }
        }
        let mut synced: HashMap<Uuid, (String, Option<String>, bool)> = HashMap::new();
        for r in all(
            &txn,
            "SELECT participant_id, synced_state, synced_role, visitor FROM conference_chat_scope_member_sync WHERE conversation_id = $1",
            vec![link.conversation_id.into()],
        )
        .await?
        {
            synced.insert(
                r.try_get_by_index(0)?,
                (r.try_get_by_index(1)?, r.try_get_by_index(2)?, r.try_get_by_index(3)?),
            );
        }
        let ids: HashSet<Uuid> = desired.keys().chain(synced.keys()).copied().collect();
        let mut sent = 0;
        for pid in ids {
            let Some(p) = conference_participants::Entity::find_by_id(pid)
                .one(&txn)
                .await?
            else {
                continue;
            };
            let previous = synced.get(&pid);
            let (target, role, is_visitor) = match desired.get(&pid) {
                Some((role, v)) => ("ACTIVE", Some(*role), *v),
                None => {
                    // Out of the scope: banned stays banned, anything else is removed.
                    let target = match conversation_state(p.admission_state) {
                        Some("BANNED") => "BANNED",
                        _ => "REMOVED",
                    };
                    (target, None, false)
                }
            };
            let unchanged = previous.is_some_and(|(s, r, v)| {
                s == target && r.as_deref() == role && (*v == is_visitor || target != "ACTIVE")
            });
            if unchanged || (previous.is_none() && target != "ACTIVE") {
                continue;
            }
            let (principal, _) = self.principal_for_participant(&p).await?;
            if principal == link.owner_principal_id {
                // The OWNER row is Messaging's; its access follows the lifecycle.
                continue;
            }
            let version: i64 = one(
                &txn,
                "SELECT nextval('conference_chat_sync_version_seq')",
                vec![],
            )
            .await?
            .ok_or_else(|| ApiError::InternalError("sequence".into()))?
            .try_get_by_index(0)?;
            let mut body = json!({
                "state": target,
                "sourceVersion": version,
                "reason": match (link.scope, target) {
                    (_, "ACTIVE") => "MEETING_ADMITTED",
                    (_, "BANNED") => "MEETING_BANNED",
                    (ChatScope::Breakout(_), _) => "BREAKOUT_LEFT",
                    _ => "MEETING_REMOVED",
                },
            });
            if let Some(role) = role {
                body["role"] = json!(role);
            }
            self.gateway
                .call(
                    Peer::Messaging,
                    None,
                    Method::PUT,
                    &format!(
                        "/internal/v1/meeting-conversations/{}/members/{}",
                        link.conversation_id, principal
                    ),
                    &[],
                    Some(&body),
                )
                .await
                .map_err(|e| chat_error(&e))?;
            exec(
                &txn,
                "INSERT INTO conference_chat_scope_member_sync (conversation_id, participant_id, principal_id, synced_version, \
                   synced_state, synced_role, visitor, synced_at) VALUES ($1, $2, $3, $4, $5, $6, $7, now()) \
                 ON CONFLICT (conversation_id, participant_id) DO UPDATE SET synced_version = EXCLUDED.synced_version, \
                   synced_state = EXCLUDED.synced_state, synced_role = EXCLUDED.synced_role, \
                   visitor = EXCLUDED.visitor, synced_at = now()",
                vec![
                    link.conversation_id.into(),
                    pid.into(),
                    principal.into(),
                    version.into(),
                    target.into(),
                    role.map(str::to_string).into(),
                    is_visitor.into(),
                ],
            )
            .await?;
            sent += 1;
        }
        txn.commit().await?;
        add(&OM.unified_messaging_members_synced, sent as u64);
        Ok(sent)
    }

    /// Records a legacy author who is no longer in the scope as a FORMER member, so an
    /// imported message keeps its true sender without granting access.
    async fn record_former_author(&self, link: &ScopeLink, participant: Uuid) -> ApiResult<()> {
        let known = one(
            &self.db,
            "SELECT 1 FROM conference_chat_scope_member_sync WHERE conversation_id = $1 AND participant_id = $2",
            vec![link.conversation_id.into(), participant.into()],
        )
        .await?
        .is_some();
        if known {
            return Ok(());
        }
        let Some(p) = conference_participants::Entity::find_by_id(participant)
            .one(&self.db)
            .await?
        else {
            return Ok(());
        };
        let (principal, _) = self.principal_for_participant(&p).await?;
        if principal == link.owner_principal_id {
            return Ok(());
        }
        let version: i64 = one(
            &self.db,
            "SELECT nextval('conference_chat_sync_version_seq')",
            vec![],
        )
        .await?
        .ok_or_else(|| ApiError::InternalError("sequence".into()))?
        .try_get_by_index(0)?;
        self.gateway
            .call(
                Peer::Messaging,
                None,
                Method::PUT,
                &format!(
                    "/internal/v1/meeting-conversations/{}/members/{}",
                    link.conversation_id, principal
                ),
                &[],
                Some(&json!({ "state": "REMOVED", "sourceVersion": version, "reason": "MEETING_REMOVED" })),
            )
            .await
            .map_err(|e| chat_error(&e))?;
        exec(
            &self.db,
            "INSERT INTO conference_chat_scope_member_sync (conversation_id, participant_id, principal_id, synced_version, synced_state) \
             VALUES ($1, $2, $3, $4, 'REMOVED') ON CONFLICT DO NOTHING",
            vec![link.conversation_id.into(), participant.into(), principal.into(), version.into()],
        )
        .await?;
        Ok(())
    }

    /// Every open scope conversation: membership follows assignments and admission.
    pub async fn sync_scoped_members(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            &format!(
                "SELECT {LINK_COLUMNS} FROM conference_chat_scope_links \
                 WHERE lifecycle_synced_state IS DISTINCT FROM 'CLOSED' ORDER BY updated_at LIMIT $1"
            ),
            vec![(limit as i64).into()],
        )
        .await?;
        let mut n = 0;
        for r in rows {
            let link = link_from(&r)?;
            n += self.sync_scope_members(&link, None).await?;
            exec(
                &self.db,
                "UPDATE conference_chat_scope_links SET updated_at = now() WHERE conversation_id = $1",
                vec![link.conversation_id.into()],
            )
            .await?;
        }
        Ok(n)
    }

    /// A closed breakout room or an ended conference closes the scope conversation.
    pub async fn sync_scoped_lifecycles(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT l.conversation_id FROM conference_chat_scope_links l \
               JOIN conference_sessions c ON c.id = l.conference_id \
               LEFT JOIN breakout_rooms b ON b.id = l.breakout_room_id \
             WHERE l.lifecycle_synced_state IS DISTINCT FROM 'CLOSED' \
               AND (c.status IN ('ENDED','CANCELLED','FAILED') OR b.status = 'CLOSED') LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut n = 0;
        for r in rows {
            let conversation_id: Uuid = r.try_get_by_index(0)?;
            let version: i64 = one(
                &self.db,
                "SELECT nextval('conference_chat_sync_version_seq')",
                vec![],
            )
            .await?
            .ok_or_else(|| ApiError::InternalError("sequence".into()))?
            .try_get_by_index(0)?;
            match self
                .gateway
                .call(
                    Peer::Messaging,
                    None,
                    Method::POST,
                    &format!("/internal/v1/meeting-conversations/{conversation_id}/lifecycle"),
                    &[],
                    Some(&json!({ "lifecycleState": "CLOSED", "sourceVersion": version, "reason": "MEETING_ENDED" })),
                )
                .await
            {
                Ok(_) | Err(GatewayError::Refused { .. }) => {}
                Err(e) => return Err(chat_error(&e)),
            }
            exec(
                &self.db,
                "UPDATE conference_chat_scope_links SET lifecycle_synced_state = 'CLOSED', updated_at = now() WHERE conversation_id = $1",
                vec![conversation_id.into()],
            )
            .await?;
            n += 1;
        }
        Ok(n)
    }

    /// Mirrors up to `limit` legacy breakout / private messages as `LEGACY_MANAGED`
    /// (SHADOW_VALIDATION; after cutover only rows written before this release remain).
    pub async fn mirror_scoped_pending(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT m.id, m.conference_id, m.sender_participant_id, m.kind, m.body, m.created_at, m.deleted_at, \
                    m.breakout_room_id, m.recipient_participant_id \
               FROM conference_messages m \
               JOIN conference_chat_links l ON l.conference_id = m.conference_id \
               LEFT JOIN conference_chat_message_links k ON k.legacy_message_id = m.id \
             WHERE (m.breakout_room_id IS NOT NULL OR m.recipient_participant_id IS NOT NULL) \
               AND m.origin = 'MEETING' AND k.messaging_message_id IS NULL \
             ORDER BY m.conference_id, m.created_at, m.id LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut groups: Vec<(ScopeLink, Vec<sea_orm::QueryResult>)> = Vec::new();
        for r in rows {
            let conference_id: Uuid = r.try_get_by_index(1)?;
            let sender: Uuid = r.try_get_by_index(2)?;
            let scope = ChatScope::of(r.try_get_by_index(7)?, sender, r.try_get_by_index(8)?)
                .ok_or_else(|| ApiError::InternalError("scoped row without a scope".into()))?;
            match groups
                .iter_mut()
                .find(|(l, _)| l.conference_id == conference_id && l.scope == scope)
            {
                Some((_, v)) => v.push(r),
                None => {
                    let link = self.ensure_scope_link(conference_id, scope).await?;
                    groups.push((link, vec![r]));
                }
            }
        }
        let mut mirrored = 0;
        for (link, rows) in groups {
            self.sync_scope_members(&link, None).await?;
            let mut items = Vec::with_capacity(rows.len());
            let mut deleted_ids = HashSet::new();
            for r in &rows {
                let id: Uuid = r.try_get_by_index(0)?;
                let sender: Uuid = r.try_get_by_index(2)?;
                let kind: String = r.try_get_by_index(3)?;
                let body: String = r.try_get_by_index(4)?;
                let created: chrono::DateTime<Utc> = r.try_get_by_index(5)?;
                let deleted: Option<chrono::DateTime<Utc>> = r.try_get_by_index(6)?;
                self.record_former_author(&link, sender).await?;
                let p = conference_participants::Entity::find_by_id(sender)
                    .one(&self.db)
                    .await?
                    .ok_or_else(|| ApiError::InternalError("message sender missing".into()))?;
                let (principal, _) = self.principal_for_participant(&p).await?;
                let mut item = json!({
                    "legacyMessageId": id.to_string(),
                    "senderPrincipalId": principal,
                    "sentAt": rfc3339(created),
                });
                if deleted.is_none() {
                    item["text"] = json!(body);
                } else {
                    deleted_ids.insert(id);
                }
                if kind == "ANNOUNCEMENT" {
                    item["presentation"] = json!("ANNOUNCEMENT");
                }
                items.push(item);
            }
            let v = self
                .gateway
                .call(
                    Peer::Messaging,
                    None,
                    Method::POST,
                    &format!(
                        "/internal/v1/meeting-conversations/{}/legacy-imports",
                        link.conversation_id
                    ),
                    &[],
                    Some(
                        &json!({ "legacySource": super::mirror::LEGACY_SOURCE, "messages": items }),
                    ),
                )
                .await
                .map_err(|e| {
                    inc(&OM.unified_messaging_mirror_failures);
                    chat_error(&e)
                })?;
            for item in v["items"].as_array().cloned().unwrap_or_default() {
                let (Some(legacy), Some(mid), Some(seq)) = (
                    item["legacyMessageId"]
                        .as_str()
                        .and_then(|s| Uuid::parse_str(s).ok()),
                    item["messageId"]
                        .as_str()
                        .and_then(|s| Uuid::parse_str(s).ok()),
                    item["sequence"].as_i64(),
                ) else {
                    continue;
                };
                exec(
                    &self.db,
                    "INSERT INTO conference_chat_message_links (messaging_message_id, conference_id, conversation_id, legacy_message_id, \
                       messaging_sequence, origin, deletion_mirrored_at) \
                     VALUES ($1, $2, $3, $4, $5, 'LEGACY_IMPORT', CASE WHEN $6 THEN now() END) ON CONFLICT DO NOTHING",
                    vec![
                        mid.into(),
                        link.conference_id.into(),
                        link.conversation_id.into(),
                        legacy.into(),
                        seq.into(),
                        deleted_ids.contains(&legacy).into(),
                    ],
                )
                .await?;
                mirrored += 1;
            }
            for refused in v["refused"].as_array().cloned().unwrap_or_default() {
                inc(&OM.unified_messaging_mirror_failures);
                tracing::warn!(reason = %refused["reason"], "legacy scoped message refused by Messaging");
            }
        }
        add(&OM.unified_messaging_messages_mirrored, mirrored as u64);
        Ok(mirrored)
    }

    /// Acting identity that reads a scope conversation (its owner: Meeting has already
    /// authorized the reader).
    pub fn scope_owner(link: &ScopeLink) -> Acting {
        Acting {
            principal_id: link.owner_principal_id,
            kind: match link.scope {
                ChatScope::Breakout(_) => PrincipalKind::NativeCfUser,
                ChatScope::Private(..) => PrincipalKind::BotOrService,
            },
        }
    }

    /// Every scope link of the conference (rollback copies and parity).
    pub async fn scope_links(&self) -> ApiResult<Vec<ScopeLink>> {
        all(
            &self.db,
            &format!("SELECT {LINK_COLUMNS} FROM conference_chat_scope_links ORDER BY created_at"),
            vec![],
        )
        .await?
        .iter()
        .map(link_from)
        .collect()
    }

    /// The scope's canonical history, oldest first.
    pub async fn scope_history(&self, link: &ScopeLink) -> ApiResult<Vec<Value>> {
        self.history_of(link.conversation_id, Self::scope_owner(link))
            .await
    }

    /// Whether a participant's admission allows them in a private chat (both parties).
    pub fn private_party_active(state: AdmissionState) -> bool {
        state == AdmissionState::Admitted
    }
}

//! Part V H §§100–101 — shadow mirroring, parity validation and rollback copies.
//!
//! * **Mirror** (SHADOW_VALIDATION): every main-room legacy message of a linked
//!   conference is imported into its conversation as `LEGACY_MANAGED` (truthful
//!   provenance, original timestamp, never relabelled as end-to-end encrypted), in
//!   order, idempotently by legacy id. Legacy deletions are applied as moderator
//!   removals. A Messaging failure only delays the mirror — the meeting and its legacy
//!   chat are unaffected.
//! * **Parity**: legacy main-room history vs the canonical conversation (presence,
//!   content digest, deletion state, unexpected extras); every mismatch is recorded.
//! * **Rollback copy**: after MESSAGING_PRIMARY → SHADOW_VALIDATION, every
//!   Messaging-native message is copied back into the legacy store (labelled
//!   `MESSAGING_ROLLBACK`) so the legacy store is complete again.

use std::collections::HashMap;

use axum::http::Method;
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    all, chat_error, exec, one, parse_time, rfc3339, Acting, ConferenceLink, Peer, PrincipalKind,
    UnifiedMessaging,
};
use crate::{
    error::{ApiError, ApiResult},
    services::ops_metrics::{add, inc, OPS_METRICS as OM},
};

pub const LEGACY_SOURCE: &str = "meeting.conference-messages";

fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

impl UnifiedMessaging {
    /// The conversation's full canonical history as its owner sees it (oldest first).
    pub async fn history(&self, link: &ConferenceLink) -> ApiResult<Vec<Value>> {
        let acting = Acting {
            principal_id: link.owner_principal_id,
            kind: PrincipalKind::NativeCfUser,
        };
        self.history_of(link.conversation_id, acting).await
    }

    /// Any conversation's full canonical history as `acting` sees it (oldest first).
    pub async fn history_of(&self, conversation_id: Uuid, acting: Acting) -> ApiResult<Vec<Value>> {
        let path = format!("/internal/v1/delegated/v1/conversations/{conversation_id}/messages");
        let mut out = Vec::new();
        let mut after: i64 = 0;
        loop {
            let after_s = after.to_string();
            let page = self
                .gateway
                .call(
                    Peer::Messaging,
                    Some(acting),
                    Method::GET,
                    &path,
                    &[("afterSequence", after_s.as_str()), ("limit", "100")],
                    None,
                )
                .await
                .map_err(|e| chat_error(&e))?;
            let messages = page["messages"].as_array().cloned().unwrap_or_default();
            out.extend(messages);
            match page["nextAfterSequence"].as_i64() {
                Some(n) if n > after => after = n,
                _ => break,
            }
            if out.len() > 200_000 {
                return Err(ApiError::InternalError(
                    "conversation history too large".into(),
                ));
            }
        }
        Ok(out)
    }

    /// Mirrors up to `limit` pending main-room legacy messages (SHADOW_VALIDATION).
    pub async fn mirror_pending(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT m.id, m.conference_id, m.sender_participant_id, m.kind, m.body, m.created_at, m.deleted_at \
               FROM conference_messages m \
               JOIN conference_chat_links l ON l.conference_id = m.conference_id \
               LEFT JOIN conference_chat_message_links k ON k.legacy_message_id = m.id \
             WHERE m.breakout_room_id IS NULL AND m.recipient_participant_id IS NULL \
               AND m.origin = 'MEETING' AND k.messaging_message_id IS NULL \
             ORDER BY m.conference_id, m.created_at, m.id LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut by_conference: Vec<(Uuid, Vec<sea_orm::QueryResult>)> = Vec::new();
        for r in rows {
            let cid: Uuid = r.try_get_by_index(1)?;
            match by_conference.last_mut() {
                Some((c, v)) if *c == cid => v.push(r),
                _ => by_conference.push((cid, vec![r])),
            }
        }
        let mut mirrored = 0;
        for (conference_id, rows) in by_conference {
            let Some(link) = self.link_of(&self.db, conference_id).await? else {
                continue;
            };
            let mut items = Vec::with_capacity(rows.len());
            for r in &rows {
                let id: Uuid = r.try_get_by_index(0)?;
                let sender: Uuid = r.try_get_by_index(2)?;
                let kind: String = r.try_get_by_index(3)?;
                let body: String = r.try_get_by_index(4)?;
                let created: chrono::DateTime<Utc> = r.try_get_by_index(5)?;
                let deleted: Option<chrono::DateTime<Utc>> = r.try_get_by_index(6)?;
                // Authors become (or are recorded as former) members first.
                self.sync_participant(&link, sender).await?;
                let principal = self.sender_principal(sender).await?;
                let mut item = json!({
                    "legacyMessageId": id.to_string(),
                    "senderPrincipalId": principal,
                    "sentAt": rfc3339(created),
                });
                if deleted.is_none() {
                    item["text"] = json!(body);
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
                    Some(&json!({ "legacySource": LEGACY_SOURCE, "messages": items })),
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
                let deleted = rows.iter().any(|r| {
                    r.try_get_by_index::<Uuid>(0).ok() == Some(legacy)
                        && r.try_get_by_index::<Option<chrono::DateTime<Utc>>>(6)
                            .ok()
                            .flatten()
                            .is_some()
                });
                exec(
                    &self.db,
                    "INSERT INTO conference_chat_message_links (messaging_message_id, conference_id, legacy_message_id, messaging_sequence, origin, deletion_mirrored_at) \
                     VALUES ($1, $2, $3, $4, 'LEGACY_IMPORT', CASE WHEN $5 THEN now() END) ON CONFLICT DO NOTHING",
                    vec![mid.into(), conference_id.into(), legacy.into(), seq.into(), deleted.into()],
                )
                .await?;
                mirrored += 1;
            }
            for refused in v["refused"].as_array().cloned().unwrap_or_default() {
                inc(&OM.unified_messaging_mirror_failures);
                tracing::warn!(reason = %refused["reason"], "legacy message refused by Messaging");
            }
        }
        add(&OM.unified_messaging_messages_mirrored, mirrored as u64);
        Ok(mirrored)
    }

    /// The principal of a message's sender (participant row of this database).
    async fn sender_principal(&self, participant_id: Uuid) -> ApiResult<Uuid> {
        use sea_orm::EntityTrait;
        let p = crate::models::conference_participants::Entity::find_by_id(participant_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| ApiError::InternalError("message sender missing".into()))?;
        Ok(self.principal_for_participant(&p).await?.0)
    }

    /// Legacy deletions of already-mirrored messages become moderator removals.
    pub async fn mirror_deletions(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT k.messaging_message_id, COALESCE(k.conversation_id, l.conversation_id) FROM conference_chat_message_links k \
               JOIN conference_messages m ON m.id = k.legacy_message_id \
               JOIN conference_chat_links l ON l.conference_id = k.conference_id \
             WHERE m.deleted_at IS NOT NULL AND k.deletion_mirrored_at IS NULL LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut n = 0;
        for r in rows {
            let mid: Uuid = r.try_get_by_index(0)?;
            let cid: Uuid = r.try_get_by_index(1)?;
            self.gateway
                .call(
                    Peer::Messaging,
                    None,
                    Method::POST,
                    &format!("/internal/v1/meeting-conversations/{cid}/messages/{mid}/moderation"),
                    &[],
                    Some(&json!({ "action": "REMOVE", "reason": "LEGACY_DELETED" })),
                )
                .await
                .map_err(|e| chat_error(&e))?;
            exec(
                &self.db,
                "UPDATE conference_chat_message_links SET deletion_mirrored_at = now() WHERE messaging_message_id = $1",
                vec![mid.into()],
            )
            .await?;
            n += 1;
        }
        Ok(n)
    }

    /// After a rollback: Messaging-native messages (every scope) go back into the legacy
    /// store, labelled `MESSAGING_ROLLBACK`.
    pub async fn copy_back_native(&self, limit: u64) -> ApiResult<u32> {
        let rows = all(
            &self.db,
            "SELECT DISTINCT conference_id, conversation_id FROM conference_chat_message_links \
             WHERE origin = 'MESSAGING_NATIVE' AND legacy_message_id IS NULL LIMIT $1",
            vec![(limit as i64).into()],
        )
        .await?;
        let mut copied = 0;
        for r in rows {
            let conference_id: Uuid = r.try_get_by_index(0)?;
            let scoped: Option<Uuid> = r.try_get_by_index(1)?;
            // (history, scope of the rows: None = main room)
            let (history, scope) = match scoped {
                None => {
                    let Some(link) = self.link_of(&self.db, conference_id).await? else {
                        continue;
                    };
                    (self.history(&link).await?, None)
                }
                Some(conversation_id) => {
                    let Some(link) = self
                        .scope_link_by_conversation(&self.db, conversation_id)
                        .await?
                    else {
                        continue;
                    };
                    (self.scope_history(&link).await?, Some(link.scope))
                }
            };
            let owed: HashMap<Uuid, i64> = all(
                &self.db,
                "SELECT messaging_message_id, messaging_sequence FROM conference_chat_message_links \
                 WHERE conference_id = $1 AND conversation_id IS NOT DISTINCT FROM $2 \
                   AND origin = 'MESSAGING_NATIVE' AND legacy_message_id IS NULL",
                vec![conference_id.into(), scoped.into()],
            )
            .await?
            .iter()
            .map(|r| Ok((r.try_get_by_index(0)?, r.try_get_by_index(1)?)))
            .collect::<ApiResult<_>>()?;
            for m in history {
                let Some(mid) = m["messageId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    continue;
                };
                if !owed.contains_key(&mid) {
                    continue;
                }
                let sender_principal = m["senderPrincipalId"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok());
                let Some(sender) = self.participant_of(conference_id, sender_principal).await?
                else {
                    tracing::warn!("rollback copy: sender has no participant in this conference");
                    continue;
                };
                let (breakout, recipient) = match scope {
                    None => (None, None),
                    Some(super::scoped::ChatScope::Breakout(b)) => (Some(b), None),
                    Some(sc) => (None, sc.peer_of(sender)),
                };
                let active = m["lifecycle"] == "ACTIVE";
                let text = m["managed"]["text"].as_str().unwrap_or("").to_string();
                let kind = if m["managed"]["presentation"] == "ANNOUNCEMENT" {
                    "ANNOUNCEMENT"
                } else {
                    "MESSAGE"
                };
                let body = if active && !text.is_empty() {
                    text
                } else {
                    "(removed)".to_string()
                };
                let created = parse_time(&m["createdAt"]).unwrap_or_else(Utc::now);
                exec(
                    &self.db,
                    "INSERT INTO conference_messages (id, conference_id, breakout_room_id, sender_participant_id, \
                       recipient_participant_id, kind, body, created_at, deleted_at, deleted_by, origin) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'MESSAGING_ROLLBACK') ON CONFLICT (id) DO NOTHING",
                    vec![
                        mid.into(),
                        conference_id.into(),
                        breakout.into(),
                        sender.into(),
                        recipient.into(),
                        kind.into(),
                        body.into(),
                        created.into(),
                        (!active).then(Utc::now).into(),
                        (!active).then_some("messaging").map(str::to_string).into(),
                    ],
                )
                .await?;
                exec(
                    &self.db,
                    "UPDATE conference_chat_message_links SET legacy_message_id = $1, \
                       deletion_mirrored_at = CASE WHEN $2 THEN now() END WHERE messaging_message_id = $1",
                    vec![mid.into(), (!active).into()],
                )
                .await?;
                copied += 1;
            }
        }
        add(&OM.unified_messaging_rollback_copies, copied as u64);
        Ok(copied)
    }

    /// The participant row (latest) of a principal in a conference.
    pub(crate) async fn participant_of(
        &self,
        conference_id: Uuid,
        principal: Option<Uuid>,
    ) -> ApiResult<Option<Uuid>> {
        let Some(principal) = principal else {
            return Ok(None);
        };
        if let Some(r) = one(
            &self.db,
            "SELECT participant_id FROM conference_chat_member_sync WHERE conference_id = $1 AND principal_id = $2 \
             ORDER BY synced_at DESC LIMIT 1",
            vec![conference_id.into(), principal.into()],
        )
        .await?
        {
            return Ok(Some(r.try_get_by_index(0)?));
        }
        // Not synced yet (e.g. the owner): resolve through the account / guest link.
        Ok(one(
            &self.db,
            "SELECT p.id FROM conference_participants p JOIN communication_principal_links k \
               ON (k.subject_kind = 'USER' AND k.subject_id = p.user_id) OR (k.subject_kind = 'PARTICIPANT' AND k.subject_id = p.id) \
             WHERE p.conference_id = $1 AND k.principal_id = $2 ORDER BY p.updated_at DESC LIMIT 1",
            vec![conference_id.into(), principal.into()],
        )
        .await?
        .map(|r| r.try_get_by_index(0))
        .transpose()?)
    }

    /// Parity of every linked conversation that has legacy history: the main room,
    /// every breakout room and every private chat. Records the run and each mismatch;
    /// PASSED only when nothing differs.
    pub async fn run_parity(&self) -> ApiResult<super::migration::ParityRunView> {
        let run_id = Uuid::new_v4();
        exec(
            &self.db,
            "INSERT INTO conference_chat_parity_runs (id, started_at, status) VALUES ($1, now(), 'RUNNING')",
            vec![run_id.into()],
        )
        .await?;
        let (mut checked_conferences, mut checked_messages, mut mismatches) = (0i32, 0i32, 0i32);
        let mut status = "PASSED";
        // Units: (conference, scope); None = the main room.
        let mut units: Vec<(Uuid, Option<super::scoped::ChatScope>)> = Vec::new();
        for r in all(
            &self.db,
            "SELECT DISTINCT conference_id, breakout_room_id, \
                    CASE WHEN recipient_participant_id IS NULL THEN NULL \
                         ELSE LEAST(sender_participant_id, recipient_participant_id) END, \
                    CASE WHEN recipient_participant_id IS NULL THEN NULL \
                         ELSE GREATEST(sender_participant_id, recipient_participant_id) END \
               FROM conference_messages",
            vec![],
        )
        .await?
        {
            let conference_id: Uuid = r.try_get_by_index(0)?;
            let breakout: Option<Uuid> = r.try_get_by_index(1)?;
            let low: Option<Uuid> = r.try_get_by_index(2)?;
            let high: Option<Uuid> = r.try_get_by_index(3)?;
            let scope = match (breakout, low, high) {
                (_, Some(a), Some(b)) => Some(super::scoped::ChatScope::Private(a, b)),
                (Some(b), _, _) => Some(super::scoped::ChatScope::Breakout(b)),
                _ => None,
            };
            if !units.contains(&(conference_id, scope)) {
                units.push((conference_id, scope));
            }
        }
        let mut conferences = std::collections::HashSet::new();
        for (conference_id, scope) in units {
            if conferences.insert(conference_id) {
                checked_conferences += 1;
            }
            let record = |legacy: Option<Uuid>, mid: Option<Uuid>, kind: &'static str| {
                let db = self.db.clone();
                async move {
                    exec(
                        &db,
                        "INSERT INTO conference_chat_mismatches (run_id, conference_id, legacy_message_id, messaging_message_id, kind) \
                         VALUES ($1, $2, $3, $4, $5)",
                        vec![run_id.into(), conference_id.into(), legacy.into(), mid.into(), kind.into()],
                    )
                    .await
                }
            };
            // The canonical conversation of the unit.
            let history = match scope {
                None => match self.link_of(&self.db, conference_id).await? {
                    Some(link) => self.history(&link).await,
                    None => {
                        record(None, None, "CONVERSATION_MISSING").await?;
                        mismatches += 1;
                        continue;
                    }
                },
                Some(sc) => match self.scope_link_of(&self.db, conference_id, sc).await? {
                    Some(link) => self.scope_history(&link).await,
                    None => {
                        record(None, None, "CONVERSATION_MISSING").await?;
                        mismatches += 1;
                        continue;
                    }
                },
            };
            let history = match history {
                Ok(h) => h,
                Err(_) => {
                    record(None, None, "MESSAGING_UNAVAILABLE").await?;
                    mismatches += 1;
                    status = "ERROR";
                    continue;
                }
            };
            let canonical: HashMap<Uuid, &Value> = history
                .iter()
                .filter_map(|m| Some((Uuid::parse_str(m["messageId"].as_str()?).ok()?, m)))
                .collect();
            let (filter, values): (&str, Vec<sea_orm::Value>) = match scope {
                None => (
                    "m.breakout_room_id IS NULL AND m.recipient_participant_id IS NULL",
                    vec![conference_id.into()],
                ),
                Some(super::scoped::ChatScope::Breakout(b)) => (
                    "m.breakout_room_id = $2 AND m.recipient_participant_id IS NULL",
                    vec![conference_id.into(), b.into()],
                ),
                Some(super::scoped::ChatScope::Private(a, b)) => (
                    "m.recipient_participant_id IS NOT NULL AND LEAST(m.sender_participant_id, m.recipient_participant_id) = $2 \
                     AND GREATEST(m.sender_participant_id, m.recipient_participant_id) = $3",
                    vec![conference_id.into(), a.into(), b.into()],
                ),
            };
            let legacy = all(
                &self.db,
                &format!(
                    "SELECT m.id, m.body, m.deleted_at IS NOT NULL, k.messaging_message_id FROM conference_messages m \
                       LEFT JOIN conference_chat_message_links k ON k.legacy_message_id = m.id \
                     WHERE m.conference_id = $1 AND {filter}"
                ),
                values,
            )
            .await?;
            let mut accounted = std::collections::HashSet::new();
            for r in legacy {
                checked_messages += 1;
                let id: Uuid = r.try_get_by_index(0)?;
                let body: String = r.try_get_by_index(1)?;
                let deleted: bool = r.try_get_by_index(2)?;
                let mid: Option<Uuid> = r.try_get_by_index(3)?;
                let Some(m) = mid.and_then(|mid| canonical.get(&mid).map(|m| (mid, *m))) else {
                    record(Some(id), mid, "MISSING_IN_MESSAGING").await?;
                    mismatches += 1;
                    continue;
                };
                accounted.insert(m.0);
                let canonical_deleted = m.1["lifecycle"] != "ACTIVE";
                if canonical_deleted != deleted {
                    record(Some(id), Some(m.0), "DELETION_DIFFERS").await?;
                    mismatches += 1;
                } else if !deleted
                    && m.1["managed"]["text"]
                        .as_str()
                        .map(|t| digest(t) != digest(&body))
                        .unwrap_or(true)
                {
                    record(Some(id), Some(m.0), "CONTENT_DIFFERS").await?;
                    mismatches += 1;
                }
            }
            // Canonical messages the legacy store does not know (outside a rollback).
            for (mid, m) in &canonical {
                if accounted.contains(mid) || m["kind"] != "APPLICATION" {
                    continue;
                }
                let native_owed = one(
                    &self.db,
                    "SELECT 1 FROM conference_chat_message_links WHERE messaging_message_id = $1",
                    vec![(*mid).into()],
                )
                .await?
                .is_some();
                if !native_owed {
                    record(None, Some(*mid), "UNEXPECTED_IN_MESSAGING").await?;
                    mismatches += 1;
                }
            }
        }
        if mismatches > 0 && status == "PASSED" {
            status = "FAILED";
        }
        add(&OM.unified_messaging_parity_mismatches, mismatches as u64);
        exec(
            &self.db,
            "UPDATE conference_chat_parity_runs SET finished_at = now(), status = $2, conferences_checked = $3, \
               messages_checked = $4, mismatches = $5 WHERE id = $1",
            vec![
                run_id.into(),
                status.into(),
                checked_conferences.into(),
                checked_messages.into(),
                mismatches.into(),
            ],
        )
        .await?;
        tracing::info!(
            status,
            checked_conferences,
            checked_messages,
            mismatches,
            "chat parity run"
        );
        self.latest_parity()
            .await?
            .ok_or_else(|| ApiError::InternalError("parity run vanished".into()))
    }
}

//! Section 11.5–11.8 and C01–C06, C17–C20, U05, U14: per-user chat controls. Every
//! read or write first proves current membership with Messaging (MS-1). The Hub stores
//! references and preferences only; message content and keys never reach it.
use crate::{
    error::{Error, Result},
    model::*,
    preference_authority::{account_view_from_service, Authority},
    relationships::uuid_field,
    store::{execute, one, rows},
    State,
};
use cf_service_auth::DelegatedPlatformContext;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MAX_SAVED: i64 = 1000;

fn preference_view(conversation: Uuid, row: Option<&Value>) -> Value {
    let now = Utc::now();
    let (notifications, muted_until, media, version) = match row {
        Some(r) => (
            r["notifications"].as_str().unwrap_or("ALL").to_owned(),
            r["muted_until"].as_str().map(str::to_owned),
            r["media_auto_download"]
                .as_str()
                .unwrap_or("WIFI")
                .to_owned(),
            r["version"].as_i64().unwrap_or(0),
        ),
        None => ("ALL".into(), None, "WIFI".into(), 0),
    };
    let expired = muted_until
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| t.with_timezone(&Utc) <= now);
    json!({"schemaVersion":1,"conversationId":conversation,"notifications":notifications,"mutedUntil":muted_until,
        "effectiveNotifications":if expired {"ALL"} else {notifications.as_str()},
        "mediaAutoDownload":media,"version":version,
        "deviceLocal":["chatLock","theme","wallpaper","translation","galleryVisibility"],
        "disappearingMessages":{"owner":"MESSAGING","note":"Use the Messaging conversation retentionSeconds policy"}})
}

impl State {
    pub async fn watermark(&self, p: &Profile, conversation: Uuid) -> Result<i64> {
        let v:Option<Value>=one(&self.store.db,"SELECT jsonb_build_object('s',cleared_through_sequence) doc FROM communication_conversation_watermarks WHERE owner_profile_id=$1 AND conversation_id=$2",vec![p.id.into(),conversation.into()]).await?;
        Ok(v.and_then(|v| v["s"].as_i64()).unwrap_or(0))
    }

    /// Watermark and the time it was set (`-infinity` when the chat was never cleared).
    pub async fn watermark_full(&self, p: &Profile, conversation: Uuid) -> Result<(i64, String)> {
        let v:Option<Value>=one(&self.store.db,"SELECT jsonb_build_object('s',cleared_through_sequence,'at',cleared_at) doc FROM communication_conversation_watermarks WHERE owner_profile_id=$1 AND conversation_id=$2",vec![p.id.into(),conversation.into()]).await?;
        Ok(match v {
            Some(v) => (
                v["s"].as_i64().unwrap_or(0),
                v["at"].as_str().unwrap_or("-infinity").to_owned(),
            ),
            None => (0, "-infinity".to_owned()),
        })
    }

    // ---- C01/C02 ----
    /// C01. Roadmap N10: with the preference switch on SERVICE, `notifications` /
    /// `mutedUntil` come from the Notification Service (media auto-download and the row
    /// version stay the Hub's); a service failure is a 503, never a stale answer.
    pub async fn get_conversation_preferences(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
    ) -> Result<Value> {
        self.conversation_preferences_view(scope, p, conversation, true)
            .await
    }
    /// The conversation preference view; `strict = false` (embedded in U05) falls back to
    /// the Hub mirror when the Notification Service cannot answer.
    pub async fn conversation_preferences_view(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        strict: bool,
    ) -> Result<Value> {
        self.conversation(scope, p, conversation, None).await?;
        let row:Option<Value>=one(&self.store.db,"SELECT to_jsonb(c) doc FROM communication_conversation_preferences c WHERE owner_profile_id=$1 AND conversation_id=$2",vec![p.id.into(),conversation.into()]).await?;
        if self.preference_authority(&self.store.db, false).await? == Authority::Service {
            match self
                .service_conversation_row(p, conversation, row.clone())
                .await
            {
                Ok(merged) => return Ok(preference_view(conversation, Some(&merged))),
                Err(e) if strict || !fallback_allowed(&e) => return Err(e),
                Err(_) => tracing::warn!(
                    service = "communications",
                    "conversation preferences embedded from the Hub mirror: Notification Service unavailable"
                ),
            }
        }
        Ok(preference_view(conversation, row.as_ref()))
    }
    pub async fn put_conversation_preferences(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Put {
            expected_version: i64,
            notifications: String,
            muted_until: Option<DateTime<Utc>>,
            media_auto_download: String,
        }
        let input: Put = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if !matches!(input.notifications.as_str(), "ALL" | "MENTIONS" | "MUTED")
            || !matches!(
                input.media_auto_download.as_str(),
                "ALWAYS" | "WIFI" | "NEVER"
            )
            || (input.muted_until.is_some() && input.notifications != "MUTED")
            || input
                .muted_until
                .is_some_and(|t| t <= Utc::now() || t > Utc::now() + chrono::Duration::days(366))
            || input.expected_version < 0
        {
            return Err(Error::Validation);
        }
        self.conversation(scope, p, conversation, None).await?;
        let txn = self.store.begin().await?;
        let authority = self.preference_authority(&txn, true).await?;
        if authority == Authority::Freeze {
            return Err(Error::PreferencesFrozen);
        }
        let op = "conversation.preferences";
        if let Some(v) = self
            .store
            .idempotency(
                &txn,
                p,
                op,
                key,
                &json!({"conversationId":conversation,"body":body}),
            )
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let values = vec![
            p.id.into(),
            conversation.into(),
            input.notifications.clone().into(),
            input.muted_until.into(),
            input.media_auto_download.clone().into(),
            input.expected_version.into(),
        ];
        let row: Option<Value> = if input.expected_version == 0 {
            one(&txn,"INSERT INTO communication_conversation_preferences AS c (owner_profile_id,conversation_id,notifications,muted_until,media_auto_download) SELECT $1,$2,$3,$4,$5 WHERE $6=0 ON CONFLICT DO NOTHING RETURNING to_jsonb(c) doc",values).await?
        } else {
            one(&txn,"UPDATE communication_conversation_preferences c SET notifications=$3,muted_until=$4,media_auto_download=$5,version=c.version+1,updated_at=now() WHERE owner_profile_id=$1 AND conversation_id=$2 AND version=$6 RETURNING to_jsonb(c) doc",values).await?
        };
        let row = row.ok_or(Error::Version)?;
        if authority == Authority::Service {
            // N10: the Notification Service is the single writer of the notification
            // level; the Hub row (media auto-download, version, mirror of the level) is
            // committed only once the service accepted it.
            let mut entry = json!({"notifications":input.notifications});
            if let Some(t) = input.muted_until {
                entry["mutedUntil"] = json!(t);
            }
            self.service_update_entry(
                p,
                json!({"conversations":{conversation.to_string():entry}}),
                key,
            )
            .await?;
        }
        self.store
            .change(
                &txn,
                p.id,
                "CONVERSATION_PREFERENCE",
                Some(conversation),
                row["version"].as_i64(),
            )
            .await?;
        let result = preference_view(conversation, Some(&row));
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    // ---- C03 clear for me ----
    pub async fn clear_for_me(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Clear {
            through_sequence: Option<i64>,
        }
        let input: Clear = if body.as_object().is_some_and(|o| o.is_empty()) {
            Clear::default()
        } else {
            serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?
        };
        let c = self.conversation(scope, p, conversation, None).await?;
        let through = input.through_sequence.unwrap_or(c.last_sequence);
        if through < 0 || through > c.last_sequence {
            return Err(Error::Validation);
        }
        let txn = self.store.begin().await?;
        let op = "conversation.clear";
        if let Some(v) = self
            .store
            .idempotency(
                &txn,
                p,
                op,
                key,
                &json!({"conversationId":conversation,"body":body}),
            )
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        // Monotonic: an older or repeated request never moves the watermark backwards.
        let updated:Option<Value>=one(&txn,"INSERT INTO communication_conversation_watermarks AS w (owner_profile_id,conversation_id,cleared_through_sequence) VALUES($1,$2,$3) ON CONFLICT(owner_profile_id,conversation_id) DO UPDATE SET cleared_through_sequence=EXCLUDED.cleared_through_sequence,cleared_at=now(),version=w.version+1 WHERE w.cleared_through_sequence<EXCLUDED.cleared_through_sequence RETURNING to_jsonb(w) doc",vec![p.id.into(),conversation.into(),through.into()]).await?;
        let row = match updated {
            Some(r) => {
                self.store
                    .change(&txn, p.id, "CLEAR_FOR_ME", Some(conversation), r["version"].as_i64())
                    .await?;
                self.store
                    .audit(&txn, p, "CONVERSATION_CLEARED_FOR_ME", Some(conversation), json!({"throughSequence":r["cleared_through_sequence"]}))
                    .await?;
                r
            }
            None => one(&txn,"SELECT to_jsonb(w) doc FROM communication_conversation_watermarks w WHERE owner_profile_id=$1 AND conversation_id=$2",vec![p.id.into(),conversation.into()]).await?.ok_or(Error::Unavailable)?,
        };
        let result = json!({"schemaVersion":1,"conversationId":conversation,"clearedThroughSequence":row["cleared_through_sequence"],"clearedAt":row["cleared_at"],"version":row["version"],
            "scope":"REQUESTING_USER_ONLY","canonicalHistoryUnchanged":true});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    // ---- C04–C06 saved messages ----
    pub async fn save_message(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Save {
            conversation_id: Uuid,
            message_id: Uuid,
        }
        let input: Save = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        let c = self
            .conversation(scope, p, input.conversation_id, Some(input.message_id))
            .await?;
        let message = c.message.ok_or(Error::NotFound)?;
        let sequence = message["sequence"].as_i64().filter(|s| *s > 0);
        if message["state"] != "VISIBLE" || message["messageId"] != json!(input.message_id) {
            return Err(Error::NotFound);
        }
        let sequence = sequence.ok_or(Error::Unavailable)?;
        if sequence <= self.watermark(p, input.conversation_id).await? {
            return Err(Error::NotFound);
        }
        let txn = self.store.begin().await?;
        let op = "saved-message.create";
        if let Some(v) = self.store.idempotency(&txn, p, op, key, &body).await? {
            txn.commit().await?;
            return Ok(v);
        }
        self.store.profile(&txn, p.scope(), p.id, true).await?;
        let existing:Option<Value>=one(&txn,"SELECT to_jsonb(s) doc FROM communication_saved_messages s WHERE owner_profile_id=$1 AND message_id=$2",vec![p.id.into(),input.message_id.into()]).await?;
        let row = match existing {
            Some(r) => r,
            None => {
                let count:Value=one(&txn,"SELECT jsonb_build_object('n',count(*)) doc FROM communication_saved_messages WHERE owner_profile_id=$1",vec![p.id.into()]).await?.ok_or(Error::Unavailable)?;
                if count["n"].as_i64().unwrap_or(MAX_SAVED) >= MAX_SAVED {
                    return Err(Error::PolicyValidation);
                }
                let id = Uuid::new_v4();
                let r:Value=one(&txn,"INSERT INTO communication_saved_messages AS s (id,owner_profile_id,conversation_id,message_id,message_sequence) VALUES($1,$2,$3,$4,$5) RETURNING to_jsonb(s) doc",vec![id.into(),p.id.into(),input.conversation_id.into(),input.message_id.into(),sequence.into()]).await?.ok_or(Error::Unavailable)?;
                self.store
                    .change(&txn, p.id, "SAVED_MESSAGE", Some(id), Some(1))
                    .await?;
                r
            }
        };
        let result = saved_view(&row, true, None);
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn list_saved(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        limit: i64,
        cursor: Uuid,
    ) -> Result<Value> {
        let limit = limit.min(50);
        let items:Vec<Value>=rows(&self.store.db,"SELECT to_jsonb(s) doc FROM communication_saved_messages s WHERE owner_profile_id=$1 AND ($2='00000000-0000-0000-0000-000000000000'::uuid OR (created_at,id)<(SELECT created_at,id FROM communication_saved_messages WHERE id=$2 AND owner_profile_id=$1)) ORDER BY created_at DESC,id DESC LIMIT $3",vec![p.id.into(),cursor.into(),(limit+1).into()]).await?;
        let more = items.len() > limit as usize;
        let mut data = Vec::new();
        let mut last = None;
        // Visibility is rechecked live: leaving a group, deletion, expiry or clear-for-me
        // makes an item unavailable without deleting the user's reference.
        let mut marks: BTreeMap<Uuid, i64> = BTreeMap::new();
        for item in items.into_iter().take(limit as usize) {
            let id = uuid_field(&item, "id")?;
            last = Some(id);
            let conversation = uuid_field(&item, "conversation_id")?;
            let message = uuid_field(&item, "message_id")?;
            let mark = match marks.get(&conversation) {
                Some(m) => *m,
                None => {
                    let m = self.watermark(p, conversation).await?;
                    marks.insert(conversation, m);
                    m
                }
            };
            let reason = if item["message_sequence"].as_i64().unwrap_or(0) <= mark {
                Some("CLEARED_FOR_ME")
            } else {
                match self
                    .conversation(scope, p, conversation, Some(message))
                    .await
                {
                    Ok(c) => match c.message.as_ref().and_then(|m| m["state"].as_str()) {
                        Some("VISIBLE") => None,
                        Some("DELETED") => Some("DELETED"),
                        Some("EXPIRED") => Some("EXPIRED"),
                        Some("HIDDEN_FOR_ME") => Some("HIDDEN_FOR_ME"),
                        _ => Some("NOT_FOUND"),
                    },
                    Err(Error::NotFound) => Some("NOT_A_MEMBER"),
                    Err(e) => return Err(e),
                }
            };
            data.push(saved_view(&item, reason.is_none(), reason));
        }
        Ok(json!({"schemaVersion":1,"data":data,"nextCursor":if more {last} else {None}}))
    }
    pub async fn delete_saved(&self, p: &Profile, id: Uuid, key: Uuid) -> Result<Value> {
        let txn = self.store.begin().await?;
        let op = "saved-message.delete";
        if let Some(v) = self
            .store
            .idempotency(&txn, p, op, key, &json!({"savedId":id}))
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let n = execute(
            &txn,
            "DELETE FROM communication_saved_messages WHERE id=$1 AND owner_profile_id=$2",
            vec![id.into(), p.id.into()],
        )
        .await?;
        if n == 0 {
            return Err(Error::NotFound);
        }
        self.store
            .change(&txn, p.id, "SAVED_MESSAGE", Some(id), None)
            .await?;
        let result = json!({"schemaVersion":1,"savedId":id,"deleted":true});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    // ---- C17/C18 ----
    /// C17. Roadmap N10: with the preference switch on SERVICE this is a facade over U6
    /// (same schema; the version is the service's); a service failure is a 503.
    pub async fn get_notification_preferences(&self, p: &Profile) -> Result<Value> {
        if self.preference_authority(&self.store.db, false).await? == Authority::Service {
            return account_view_from_service(&self.service_preferences(p).await?);
        }
        self.local_notification_preferences(p).await
    }
    async fn local_notification_preferences(&self, p: &Profile) -> Result<Value> {
        let row:Option<Value>=one(&self.store.db,"SELECT to_jsonb(n) doc FROM communication_notification_preferences n WHERE profile_id=$1",vec![p.id.into()]).await?;
        Ok(notification_view(row.as_ref()))
    }
    /// The preferences embedded in U01 bootstrap and N07 delivery status: as C17, but an
    /// unavailable Notification Service falls back to the Hub mirror (which holds every
    /// change made through these endpoints) instead of failing the whole aggregate.
    pub async fn notification_preferences_embedded(&self, p: &Profile) -> Result<Value> {
        match self.get_notification_preferences(p).await {
            Err(e) if fallback_allowed(&e) => {
                tracing::warn!(
                    service = "communications",
                    "notification preferences embedded from the Hub mirror: Notification Service unavailable"
                );
                self.local_notification_preferences(p).await
            }
            r => r,
        }
    }
    pub async fn put_notification_preferences(
        &self,
        p: &Profile,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Put {
            expected_version: i64,
            direct_messages: bool,
            group_messages: bool,
            incoming_calls: bool,
            missed_calls: bool,
            contact_requests: bool,
            group_invitations: bool,
            meeting_invitations: bool,
        }
        let i: Put = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if i.expected_version < 0 {
            return Err(Error::Validation);
        }
        let txn = self.store.begin().await?;
        let authority = self.preference_authority(&txn, true).await?;
        if authority == Authority::Freeze {
            return Err(Error::PreferencesFrozen);
        }
        let op = "notification-preferences.put";
        if let Some(v) = self.store.idempotency(&txn, p, op, key, &body).await? {
            txn.commit().await?;
            return Ok(v);
        }
        if authority == Authority::Service {
            // N10: U7 decides (optimistic version of the service document); the Hub row is
            // then updated as a mirror in the same transaction, never before.
            let accepted = self
                .service_update(
                    p,
                    json!({"expectedVersion":i.expected_version,"categories":{
                        "directMessages":i.direct_messages,"groupMessages":i.group_messages,
                        "incomingCalls":i.incoming_calls,"missedCalls":i.missed_calls,
                        "contactRequests":i.contact_requests,"groupInvitations":i.group_invitations,
                        "meetingInvitations":i.meeting_invitations}}),
                    key,
                )
                .await?
                .ok_or(Error::Version)?;
            let result = account_view_from_service(&accepted)?;
            execute(&txn,"INSERT INTO communication_notification_preferences AS n (profile_id,direct_messages,group_messages,incoming_calls,missed_calls,contact_requests,group_invitations,meeting_invitations) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (profile_id) DO UPDATE SET direct_messages=EXCLUDED.direct_messages,group_messages=EXCLUDED.group_messages,incoming_calls=EXCLUDED.incoming_calls,missed_calls=EXCLUDED.missed_calls,contact_requests=EXCLUDED.contact_requests,group_invitations=EXCLUDED.group_invitations,meeting_invitations=EXCLUDED.meeting_invitations,version=n.version+1,updated_at=now()",
                vec![p.id.into(),i.direct_messages.into(),i.group_messages.into(),i.incoming_calls.into(),i.missed_calls.into(),i.contact_requests.into(),i.group_invitations.into(),i.meeting_invitations.into()]).await?;
            self.store
                .change(
                    &txn,
                    p.id,
                    "NOTIFICATION_PREFERENCE",
                    Some(p.id),
                    result["version"].as_i64(),
                )
                .await?;
            self.store
                .finish_idempotency(&txn, p, op, key, result.clone())
                .await?;
            txn.commit().await?;
            return Ok(result);
        }
        let values = vec![
            p.id.into(),
            i.direct_messages.into(),
            i.group_messages.into(),
            i.incoming_calls.into(),
            i.missed_calls.into(),
            i.contact_requests.into(),
            i.group_invitations.into(),
            i.meeting_invitations.into(),
            i.expected_version.into(),
        ];
        let row: Option<Value> = if i.expected_version == 0 {
            one(&txn,"INSERT INTO communication_notification_preferences AS n (profile_id,direct_messages,group_messages,incoming_calls,missed_calls,contact_requests,group_invitations,meeting_invitations) SELECT $1,$2,$3,$4,$5,$6,$7,$8 WHERE $9=0 ON CONFLICT DO NOTHING RETURNING to_jsonb(n) doc",values).await?
        } else {
            one(&txn,"UPDATE communication_notification_preferences n SET direct_messages=$2,group_messages=$3,incoming_calls=$4,missed_calls=$5,contact_requests=$6,group_invitations=$7,meeting_invitations=$8,version=n.version+1,updated_at=now() WHERE profile_id=$1 AND version=$9 RETURNING to_jsonb(n) doc",values).await?
        };
        let row = row.ok_or(Error::Version)?;
        self.store
            .change(
                &txn,
                p.id,
                "NOTIFICATION_PREFERENCE",
                Some(p.id),
                row["version"].as_i64(),
            )
            .await?;
        let result = notification_view(Some(&row));
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    // ---- C19 ----
    pub async fn storage_summary(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
    ) -> Result<Value> {
        let messaging = self
            .messaging(scope)?
            .call(
                "/internal/v1/communications/storage-summary",
                &json!({"principalId":p.principal_id}),
            )
            .await?;
        let saved:Value=one(&self.store.db,"SELECT jsonb_build_object('n',count(*)) doc FROM communication_saved_messages WHERE owner_profile_id=$1",vec![p.id.into()]).await?.ok_or(Error::Unavailable)?;
        Ok(json!({"schemaVersion":1,
            "messaging":{"attachmentCount":messaging["attachmentCount"],"attachmentBytes":messaging["attachmentBytes"],
                "backupBytes":messaging["backupBytes"],"environmentQuotaBytes":messaging["environmentQuotaBytes"],"measuredAt":messaging["measuredAt"]},
            "savedMessages":{"count":saved["n"],"limit":MAX_SAVED},
            "deviceCache":{"owner":"DEVICE","note":"Local caches are measured and cleared on the device"}}))
    }

    // ---- C20 ----
    pub async fn conversation_shared_groups(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
    ) -> Result<Value> {
        let c = self.conversation(scope, p, conversation, None).await?;
        if c.context_class != "DIRECT" {
            return Err(Error::ActionNotAllowed);
        }
        let Some(contact) = self.counterpart_contact(p, &c).await? else {
            return Ok(json!({"schemaVersion":1,"groups":[],"available":false}));
        };
        let mut groups = self.common_groups(scope, p, contact).await?;
        groups["available"] = json!(true);
        Ok(groups)
    }
    /// The viewer's contact id for the other DIRECT member, if they are a contact.
    pub async fn counterpart_contact(
        &self,
        p: &Profile,
        c: &crate::owners::Conversation,
    ) -> Result<Option<Uuid>> {
        let other = c.members.iter().find(|m| **m != p.principal_id).copied();
        let Some(other) = other else { return Ok(None) };
        let target = match self.store.principal(&self.store.db, p.scope(), other).await {
            Ok(t) => t,
            Err(Error::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        let v:Option<Value>=one(&self.store.db,"SELECT jsonb_build_object('id',a.id) doc FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND ((c.left_profile_id=$1 AND c.right_profile_id=$2) OR (c.right_profile_id=$1 AND c.left_profile_id=$2))",vec![p.id.into(),target.id.into()]).await?;
        v.map(|v| uuid_field(&v, "id")).transpose()
    }

    // ---- U14 ----
    pub async fn conversation_media(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        limit: i64,
        cursor: Option<String>,
    ) -> Result<Value> {
        self.conversation(scope, p, conversation, None).await?;
        let mark = self.watermark(p, conversation).await?;
        let mut body = json!({"principalId":p.principal_id,"conversationId":conversation,"limit":limit,"afterSequence":mark});
        if let Some(c) = cursor {
            body["cursor"] = json!(c);
        }
        let v = self
            .messaging(scope)?
            .call(
                "/internal/v1/communications/conversations/attachments",
                &body,
            )
            .await?;
        let mut data = Vec::new();
        for a in v["data"].as_array().ok_or(Error::Unavailable)? {
            if a["messageSequence"].as_i64().is_some_and(|s| s <= mark) {
                continue;
            }
            data.push(media_item(conversation, a));
        }
        Ok(
            json!({"schemaVersion":1,"conversationId":conversation,"data":data,"nextCursor":v["nextCursor"],
            "download":"Use the Messaging attachment download-grant API; private files are decrypted only on the device"}),
        )
    }
}

/// U14 / M3: one shared-media item. MANAGED files carry the sanitized name, media type
/// and kind Messaging holds; PRIVATE_E2EE files carry none of them (they live in the
/// MLS-encrypted descriptor and are shown only after the device decrypts it). Retrieval
/// is a reference to the normal access-checked grant operation of THIS conversation and
/// attachment — `download.path` / `thumbnail.path` relative to the Messaging client API,
/// reached by CF Meet apps through `/api/v2/messaging/relay/` — never a URL or storage
/// key; anything else Messaging might send is dropped.
pub fn media_item(conversation: Uuid, a: &Value) -> Value {
    let managed = a["securityProfile"] == "MANAGED";
    let id = a["attachmentId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok());
    let grant = |field: &str, operation: &str, thumbnail: bool| -> Value {
        let Some(id) = id else { return Value::Null };
        let expected = format!("v1/conversations/{conversation}/attachments/{id}/{operation}");
        if a[field]["path"].as_str() != Some(expected.as_str()) || a[field]["method"] != "POST" {
            return Value::Null;
        }
        if thumbnail {
            json!({"method":"POST","path":expected,"mediaType":"image/png"})
        } else {
            json!({"method":"POST","path":expected})
        }
    };
    let text = |key: &str, max: usize| {
        a[key]
            .as_str()
            .filter(|t| managed && !t.is_empty() && t.chars().count() <= max)
            .filter(|t| !t.chars().any(char::is_control))
            .map(str::to_string)
    };
    let kind = a["kind"]
        .as_str()
        .filter(|k| managed && ["IMAGE", "DOCUMENT", "FILE", "VOICE_NOTE", "VIDEO"].contains(k));
    let profile = a["securityProfile"]
        .as_str()
        .filter(|p| ["MANAGED", "PRIVATE_E2EE"].contains(p));
    json!({"attachmentId":a["attachmentId"],"messageId":a["messageId"],"state":a["state"],
        "sizeBytes":a["sizeBytes"],"mediaClass":a["mediaClass"],"createdAt":a["createdAt"],
        "securityProfile":profile,"kind":kind,"fileName":text("fileName", 255),"mediaType":text("mediaType", 255),
        "metadataLocation":match profile {
            Some("MANAGED") => Some("SERVER"),
            Some(_) => Some("ENCRYPTED_DESCRIPTOR"),
            None => None,
        },
        "download":grant("download", "download-grants", false),
        "thumbnail":if managed {grant("thumbnail", "thumbnail-grants", true)} else {Value::Null}})
}
fn saved_view(r: &Value, available: bool, reason: Option<&str>) -> Value {
    json!({"schemaVersion":1,"savedId":r["id"],"conversationId":r["conversation_id"],"messageId":r["message_id"],
        "messageSequence":r["message_sequence"],"savedAt":r["created_at"],"available":available,"unavailableReason":reason})
}
/// Embedded views may fall back to the Hub mirror on these (never on a validation,
/// version or database error).
fn fallback_allowed(e: &Error) -> bool {
    matches!(
        e,
        Error::Unavailable | Error::RateLimited | Error::DependencyNotConfigured
    )
}

fn notification_view(r: Option<&Value>) -> Value {
    let get = |k: &str| r.map(|r| r[k] == true).unwrap_or(true);
    json!({"schemaVersion":1,"version":r.and_then(|r| r["version"].as_i64()).unwrap_or(0),
        "directMessages":get("direct_messages"),"groupMessages":get("group_messages"),"incomingCalls":get("incoming_calls"),
        "missedCalls":get("missed_calls"),"contactRequests":get("contact_requests"),"groupInvitations":get("group_invitations"),
        "meetingInvitations":get("meeting_invitations"),"securityEvents":true,
        "payloadPolicy":"ROUTING_METADATA_ONLY"})
}

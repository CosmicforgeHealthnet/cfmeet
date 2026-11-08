//! Section 12.6 and U01, U02, U05, U12, U13, N11: bootstrap, overlays and the monotonic
//! per-user change feed. Feed entries say WHAT changed; clients refetch the authoritative
//! resource, so a missed entry can never fabricate state. Pruned windows are reported as
//! RESYNC_REQUIRED, never as an empty, acknowledged gap.
use crate::{
    error::{Error, Result},
    model::*,
    store::{one, rows},
    State,
};
use cf_service_auth::DelegatedPlatformContext;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

const DOMAINS: &[&str] = &[
    "CONTACT",
    "CONTACT_LIST",
    "CONVERSATION_PREFERENCE",
    "CLEAR_FOR_ME",
    "SAVED_MESSAGE",
    "NOTIFICATION_PREFERENCE",
    "DEVICE",
    "TIMELINE",
    "MEETING_LINK",
    "PRIVACY",
    "BLOCK",
    "PROFILE",
];

impl State {
    /// (latest position, oldest retained position) of the profile's feed.
    pub async fn sync_head(&self, p: &Profile) -> Result<(i64, i64)> {
        let v:Value=one(&self.store.db,"SELECT jsonb_build_object('max',COALESCE(max(seq),0),'min',COALESCE(min(seq),0)) doc FROM communication_changes WHERE profile_id=$1",vec![p.id.into()]).await?.ok_or(Error::Unavailable)?;
        Ok((
            v["max"].as_i64().unwrap_or(0),
            v["min"].as_i64().unwrap_or(0),
        ))
    }

    /// U12 (all domains) and U13 (optionally filtered by `domains`).
    pub async fn changes(
        &self,
        p: &Profile,
        query: &BTreeMap<String, String>,
        filtered: bool,
    ) -> Result<Value> {
        if query
            .keys()
            .any(|k| !(matches!(k.as_str(), "after" | "limit") || filtered && k == "domains"))
        {
            return Err(Error::Validation);
        }
        let after = query
            .get("after")
            .map(|v| v.parse::<i64>())
            .transpose()
            .map_err(|_| Error::Validation)?
            .unwrap_or(0);
        let limit = query
            .get("limit")
            .map(|v| v.parse::<i64>())
            .transpose()
            .map_err(|_| Error::Validation)?
            .unwrap_or(100);
        if after < 0 || !(1..=500).contains(&limit) {
            return Err(Error::Validation);
        }
        let domains: Vec<String> = match query.get("domains") {
            None => DOMAINS.iter().map(|d| (*d).to_owned()).collect(),
            Some(d) => {
                let list: Vec<String> = d.split(',').map(str::to_owned).collect();
                if list.is_empty() || list.iter().any(|x| !DOMAINS.contains(&x.as_str())) {
                    return Err(Error::Validation);
                }
                list
            }
        };
        let (head, oldest) = self.sync_head(p).await?;
        if after > head {
            return Err(Error::Validation);
        }
        if after > 0 && oldest > 0 && after + 1 < oldest {
            return Ok(
                json!({"schemaVersion":1,"resyncRequired":true,"reason":"CURSOR_OLDER_THAN_RETENTION",
                "latestPosition":head,"oldestRetainedPosition":oldest,"data":[],"nextPosition":head,
                "snapshot":"Reload GET /bootstrap, then resume from latestPosition"}),
            );
        }
        let entries:Vec<Value>=rows(&self.store.db,"SELECT jsonb_build_object('position',seq,'domain',domain,'resourceId',resource_id,'resourceVersion',resource_version,'changedAt',created_at) doc FROM communication_changes WHERE profile_id=$1 AND seq>$2 AND domain IN (SELECT jsonb_array_elements_text($3::jsonb)) ORDER BY seq LIMIT $4",
            vec![p.id.into(),after.into(),json!(domains).into(),(limit+1).into()]).await?;
        let more = entries.len() > limit as usize;
        let data: Vec<Value> = entries.into_iter().take(limit as usize).collect();
        // With a filter, the cursor still advances over skipped domains only up to the
        // last returned entry (or the head when nothing further matched).
        let next = if more {
            data.last()
                .and_then(|e| e["position"].as_i64())
                .unwrap_or(after)
        } else {
            head
        };
        Ok(
            json!({"schemaVersion":1,"resyncRequired":false,"latestPosition":head,"oldestRetainedPosition":oldest,
            "data":data,"nextPosition":next,"hasMore":more,
            "acknowledgement":"Clients acknowledge applied positions per device with PUT /devices/{deviceId}/sync-state"}),
        )
    }

    /// U01: everything a freshly started client needs to render the five areas.
    pub async fn bootstrap(&self, scope: &DelegatedPlatformContext, p: &Profile) -> Result<Value> {
        let counts:Value=one(&self.store.db,"SELECT jsonb_build_object(
            'contacts',(SELECT count(*) FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND c.state='CONNECTED'),
            'favorites',(SELECT count(*) FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND a.favorite AND c.state='CONNECTED'),
            'incomingRequests',(SELECT count(*) FROM communication_contact_requests WHERE recipient_profile_id=$1 AND state='PENDING' AND expires_at>now()),
            'outgoingRequests',(SELECT count(*) FROM communication_contact_requests WHERE sender_profile_id=$1 AND state='PENDING' AND expires_at>now()),
            'contactLists',(SELECT count(*) FROM communication_contact_lists WHERE owner_profile_id=$1),
            'savedMessages',(SELECT count(*) FROM communication_saved_messages WHERE owner_profile_id=$1),
            'activeDevices',(SELECT count(*) FROM communication_devices WHERE profile_id=$1 AND state='ACTIVE')) doc",vec![p.id.into()]).await?.ok_or(Error::Unavailable)?;
        let (head, oldest) = self.sync_head(p).await?;
        Ok(
            json!({"schemaVersion":1,"me":self.me(p).await?,"capabilities":self.capabilities(scope,p).await,
            "privacy":self.get_privacy(p).await?,"notificationPreferences":self.get_notification_preferences(p).await?,
            "counts":counts,"sync":{"latestPosition":head,"oldestRetainedPosition":oldest},
            "areas":{"chats":{"owner":"MESSAGING","list":"GET /api/v2/messaging/relay/v1/conversations","overlay":"GET /api/v2/communications/inbox"},
                "calls":{"owner":"CALLING","history":"GET /api/v2/communications/calls/history"},
                "contacts":{"owner":"COMMUNICATIONS"},"meetings":{"owner":"MEETINGS","independent":true},
                "settings":{"owner":"COMMUNICATIONS","deviceLocal":["chatLock","theme","translation","galleryVisibility","cache"]}}}),
        )
    }

    /// U02: the Hub overlay for the Messaging conversation list (mute, clear-for-me,
    /// latest activity card). Messaging owns the list itself, ordering and unread counts.
    /// Section 14 (MSG-016): rows are limited to conversations the viewer still belongs
    /// to, checked with Messaging (the same check `activity` makes); a conversation the
    /// viewer left keeps its private preferences but is no longer listed.
    pub async fn inbox(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        limit: i64,
        cursor: Uuid,
    ) -> Result<Value> {
        let data:Vec<Value>=rows(&self.store.db,"WITH ids AS (
              SELECT conversation_id FROM communication_conversation_preferences WHERE owner_profile_id=$1
              UNION SELECT conversation_id FROM communication_conversation_watermarks WHERE owner_profile_id=$1
              UNION SELECT conversation_id FROM communication_timeline_entries WHERE viewer_profile_id=$1)
            SELECT jsonb_build_object('conversationId',i.conversation_id,
              'notifications',COALESCE(CASE WHEN pr.muted_until IS NOT NULL AND pr.muted_until<=now() THEN 'ALL' ELSE pr.notifications END,'ALL'),
              'mutedUntil',pr.muted_until,'clearedThroughSequence',COALESCE(w.cleared_through_sequence,0),
              'latestActivity',(SELECT jsonb_build_object('entryId',e.entry_key,'kind',e.kind,'occurredAt',e.occurred_at,'missed',e.data->'missed')
                 FROM communication_timeline_entries e WHERE e.viewer_profile_id=$1 AND e.conversation_id=i.conversation_id ORDER BY e.occurred_at DESC,e.entry_key DESC LIMIT 1)) doc
            FROM ids i LEFT JOIN communication_conversation_preferences pr ON pr.owner_profile_id=$1 AND pr.conversation_id=i.conversation_id
            LEFT JOIN communication_conversation_watermarks w ON w.owner_profile_id=$1 AND w.conversation_id=i.conversation_id
            WHERE i.conversation_id>$2 ORDER BY i.conversation_id LIMIT $3",vec![p.id.into(),cursor.into(),(limit+1).into()]).await?;
        let more = data.len() > limit as usize;
        let page: Vec<Value> = data.into_iter().take(limit as usize).collect();
        // The cursor follows the scanned rows, so a filtered row never repeats or hides
        // a later one; a page may therefore hold fewer than `limit` rows.
        let next = if more {
            page.last().map(|d| d["conversationId"].clone())
        } else {
            None
        };
        let mut data = Vec::with_capacity(page.len());
        for row in page {
            let id = row["conversationId"]
                .as_str()
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or(Error::Unavailable)?;
            match self.conversation(scope, p, id, None).await {
                Ok(_) => data.push(row),
                Err(Error::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(json!({"schemaVersion":1,"data":data,"nextCursor":next,
            "membership":"Only conversations the viewer is a current member of (checked with Messaging)"}))
    }

    /// U05: allowed actions and per-user state for one conversation.
    pub async fn conversation_context(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
    ) -> Result<Value> {
        let c = self.conversation(scope, p, conversation, None).await?;
        let prefs = self
            .get_conversation_preferences(scope, p, conversation)
            .await?;
        let (mark, cleared_at) = self.watermark_full(p, conversation).await?;
        let counterpart = if c.context_class == "DIRECT" {
            match c.members.iter().find(|m| **m != p.principal_id) {
                Some(m) => Some(self.participant_view(p, *m).await?),
                None => None,
            }
        } else {
            None
        };
        let personal =
            matches!(c.context_class.as_str(), "DIRECT" | "GROUP") && c.lifecycle_state == "ACTIVE";
        let active = p.activation_state == Activation::Active;
        let direct_ok = counterpart
            .as_ref()
            .is_some_and(|v| v["relationship"] == "CONNECTED");
        let can_call = active && personal && (c.context_class == "GROUP" || direct_ok);
        let meetings:Value=one(&self.store.db,"SELECT jsonb_build_object('n',count(*)) doc FROM communication_meeting_links WHERE conversation_id=$1",vec![conversation.into()]).await?.ok_or(Error::Unavailable)?;
        Ok(
            json!({"schemaVersion":1,"conversationId":conversation,"contextClass":c.context_class,"securityProfile":c.security_profile,
            "lifecycleState":c.lifecycle_state,"lastSequence":c.last_sequence,"memberCount":c.members.len(),
            "retentionSeconds":c.retention_seconds,"role":c.raw["role"],"counterpart":counterpart,
            "preferences":prefs,"clearedThroughSequence":mark,"clearedAt":if mark>0 {json!(cleared_at)} else {Value::Null},
            "linkedMeetings":meetings["n"],
            "actions":{"voiceCall":can_call,"videoCall":can_call,"instantMeeting":active && personal,"shareMeeting":active && personal,
                "saveMessages":true,"clearForMe":true,"mute":true,"disappearingMessages":{"owner":"MESSAGING","policy":"retentionSeconds"},
                "meetingChatIsSeparate":true},
            "security":{"privateEncryption":c.security_profile=="PRIVATE_E2EE","managedFallback":false}}),
        )
    }

    /// N11: recovery information after reinstall or a lost device.
    pub async fn recovery_state(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
    ) -> Result<Value> {
        let (head, oldest) = self.sync_head(p).await?;
        let devices:Value=one(&self.store.db,"SELECT jsonb_build_object('active',count(*) FILTER (WHERE state='ACTIVE'),'revoked',count(*) FILTER (WHERE state='REVOKED')) doc FROM communication_devices WHERE profile_id=$1",vec![p.id.into()]).await?.ok_or(Error::Unavailable)?;
        let capabilities = self.capabilities(scope, p).await;
        Ok(
            json!({"schemaVersion":1,"activationState":p.activation_state,"cfMeetId":self.store.current_public_id(&self.store.db,p.id).await?,
            "devices":devices,"sync":{"latestPosition":head,"oldestRetainedPosition":oldest,"restart":"Load GET /bootstrap, then GET /sync?after=latestPosition"},
            "messaging":{"securityReady":capabilities["messaging"]["securityReady"],
                "history":"Encrypted history moves only by device-to-device transfer or the user's own encrypted backup; the server holds no keys"},
            "calls":{"history":"GET /api/v2/communications/calls/history"},
            "contacts":{"source":"SERVER","note":"Contacts, lists, favorites and preferences are restored from the server"},
            "phoneRecovery":{"status":"PROVIDER_BLOCKED","available":false}}),
        )
    }
}

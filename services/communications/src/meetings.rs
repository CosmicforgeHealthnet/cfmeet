//! Section 13 / N08–N10: Meetings referenced from an authorized conversation. Meetings
//! stays the authority for spaces, admission, guests, roles, media and meeting chat;
//! the Hub stores only a link and per-member invitation cards. Group conversations and
//! meeting chats remain separate contexts.
use crate::{
    error::{Error, Result},
    model::*,
    relationships::uuid_field,
    store::{execute, one, rows},
    State,
};
use cf_service_auth::DelegatedPlatformContext;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

fn link_view(l: &Value, viewer: &Profile) -> Value {
    json!({"linkId":l["id"],"conversationId":l["conversation_id"],"meetingSpaceId":l["meeting_space_id"],
        "meetingCode":l["meeting_code"],"origin":l["origin"],"linkedAt":l["created_at"],
        "linkedByViewer":l["linked_by_profile_id"]==json!(viewer.id),
        "join":{"owner":"MEETINGS","note":"Resolve and join with the existing Meetings API; admission rules are unchanged"}})
}

impl State {
    async fn record_meeting(
        &self,
        p: &Profile,
        c: &crate::owners::Conversation,
        space: Uuid,
        code: &str,
        origin: &str,
        request: Uuid,
    ) -> Result<Value> {
        if code.len() > 64
            || !code
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            || code.len() < 3
        {
            return Err(Error::Unavailable);
        }
        let txn = self.store.begin().await?;
        let id = Uuid::new_v4();
        let inserted:Option<Value>=one(&txn,"INSERT INTO communication_meeting_links AS l (id,conversation_id,linked_by_profile_id,meeting_space_id,meeting_code,origin,client_request_id) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING RETURNING to_jsonb(l) doc",
            vec![id.into(),c.id.into(),p.id.into(),space.into(),code.into(),origin.into(),request.into()]).await?;
        let link = match inserted {
            Some(l) => {
                self.store.audit(&txn, p, "MEETING_LINKED", Some(c.id), json!({"origin":origin})).await?;
                for member in &c.members {
                    let viewer = match self.store.principal(&txn, p.scope(), *member).await {
                        Ok(v) => v,
                        Err(Error::NotFound) => continue,
                        Err(e) => return Err(e),
                    };
                    let data = json!({"meetingCode":code,"origin":origin,"anchorSequence":c.last_sequence,"sharedByViewer":viewer.id==p.id,"terminal":true});
                    execute(&txn,"INSERT INTO communication_timeline_entries(viewer_profile_id,conversation_id,entry_key,kind,source,source_id,occurred_at,data) VALUES($1,$2,$3,'MEETING_INVITATION','MEETINGS',$4,now(),$5) ON CONFLICT DO NOTHING",
                        vec![viewer.id.into(),c.id.into(),format!("meeting:{id}").into(),space.into(),data.into()]).await?;
                    self.store.change(&txn, viewer.id, "MEETING_LINK", Some(c.id), None).await?;
                }
                // MSG-009: members are told in realtime; members without a profile get the
                // card when they open the timeline (`materialize_meeting_cards`).
                self.timeline_nudge(&txn, p.scope(), c.id, &format!("meeting:{id}"), "MEETING_SHARED")
                    .await?;
                l
            }
            // Replay of the same request, or this meeting is already linked here.
            None => one(&txn,"SELECT to_jsonb(l) doc FROM communication_meeting_links l WHERE (linked_by_profile_id=$1 AND client_request_id=$2) OR (conversation_id=$3 AND meeting_space_id=$4) ORDER BY created_at LIMIT 1",
                vec![p.id.into(),request.into(),c.id.into(),space.into()]).await?.ok_or(Error::Unavailable)?,
        };
        if uuid_field(&link, "conversation_id")? != c.id
            || uuid_field(&link, "meeting_space_id")? != space
        {
            return Err(Error::Idempotency);
        }
        txn.commit().await?;
        let mut view = link_view(&link, p);
        view["schemaVersion"] = json!(1);
        Ok(view)
    }
    async fn meeting_conversation(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
    ) -> Result<crate::owners::Conversation> {
        if p.activation_state != Activation::Active {
            return Err(Error::NotActivated);
        }
        let c = self.conversation(scope, p, conversation, None).await?;
        if !matches!(c.context_class.as_str(), "DIRECT" | "GROUP") || c.lifecycle_state != "ACTIVE"
        {
            return Err(Error::ActionNotAllowed);
        }
        Ok(c)
    }

    /// N08: create an instant meeting owned by the caller and share it here.
    pub async fn instant_meeting(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        body: Value,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Instant {
            client_request_id: Uuid,
            title: Option<String>,
        }
        let input: Instant = serde_json::from_value(body).map_err(|_| Error::Validation)?;
        if input.title.as_deref().is_some_and(|t| {
            t.trim().is_empty() || t.chars().count() > 120 || t.chars().any(char::is_control)
        }) {
            return Err(Error::Validation);
        }
        let c = self.meeting_conversation(scope, p, conversation).await?;
        self.limit_attempt(p, "meeting.instant", 30).await?;
        let mut request = json!({"principalId":p.principal_id,"accountReference":p.account_reference,"idempotencyKey":input.client_request_id});
        if let Some(t) = &input.title {
            request["title"] = json!(t.trim());
        }
        let m = self
            .meetings(scope)
            .call("/internal/v1/communications/meetings/instant", &request)
            .await?;
        let space = uuid_field(&m, "meetingSpaceId")?;
        let code = m["meetingCode"]
            .as_str()
            .ok_or(Error::Unavailable)?
            .to_owned();
        let mut view = self
            .record_meeting(p, &c, space, &code, "INSTANT", input.client_request_id)
            .await?;
        view["conferenceId"] = m["conferenceId"].clone();
        Ok(view)
    }

    /// N09: share an existing meeting the caller owns or hosts.
    pub async fn share_meeting(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        body: Value,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Share {
            client_request_id: Uuid,
            meeting_code: String,
        }
        let input: Share = serde_json::from_value(body).map_err(|_| Error::Validation)?;
        if input.meeting_code.len() > 64
            || input.meeting_code.len() < 3
            || !input
                .meeting_code
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            return Err(Error::Validation);
        }
        let c = self.meeting_conversation(scope, p, conversation).await?;
        let m = self
            .meetings(scope)
            .call(
                "/internal/v1/communications/meetings/resolve",
                &json!({"principalId":p.principal_id,"accountReference":p.account_reference,"meetingCode":input.meeting_code}),
            )
            .await?;
        if m["shareable"] != true {
            return Err(Error::ActionNotAllowed);
        }
        let space = uuid_field(&m, "meetingSpaceId")?;
        let code = m["meetingCode"]
            .as_str()
            .ok_or(Error::Unavailable)?
            .to_owned();
        self.record_meeting(p, &c, space, &code, "SHARED", input.client_request_id)
            .await
    }

    /// N10: meetings linked into a conversation the caller still belongs to.
    pub async fn conversation_meetings(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        limit: i64,
        cursor: Uuid,
    ) -> Result<Value> {
        self.conversation(scope, p, conversation, None).await?;
        let links:Vec<Value>=rows(&self.store.db,"SELECT to_jsonb(l) doc FROM communication_meeting_links l WHERE conversation_id=$1 AND ($2='00000000-0000-0000-0000-000000000000'::uuid OR (created_at,id)<(SELECT created_at,id FROM communication_meeting_links WHERE id=$2 AND conversation_id=$1)) ORDER BY created_at DESC,id DESC LIMIT $3",
            vec![conversation.into(),cursor.into(),(limit+1).into()]).await?;
        let more = links.len() > limit as usize;
        let data: Vec<Value> = links
            .iter()
            .take(limit as usize)
            .map(|l| link_view(l, p))
            .collect();
        let next = if more {
            links.get(limit as usize - 1).map(|l| l["id"].clone())
        } else {
            None
        };
        Ok(json!({"schemaVersion":1,"conversationId":conversation,"data":data,"nextCursor":next}))
    }
}

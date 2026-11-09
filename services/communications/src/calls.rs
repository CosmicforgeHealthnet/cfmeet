//! Section 10: call launch from Contacts/Conversations, call history, call context and
//! the per-viewer call cards of the unified timeline. Calling owns every call, state,
//! offer, outcome and media credential; the Hub keeps only safe links and references.
use crate::{
    error::{Error, Result},
    model::*,
    relationships::uuid_field,
    store::{execute, one, rows, scoped},
    State,
};
use cf_communications_policy::{Action, PolicyCheck};
use cf_service_auth::DelegatedPlatformContext;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Non-terminal call cards older than this are refreshed from Calling on read.
pub const CALL_REFRESH_SECONDS: i64 = 5;
const LAUNCH_LIMIT_PER_HOUR: i64 = 120;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Launch {
    pub kind: String,
    pub client_request_id: Uuid,
    pub endpoint_id: Uuid,
}
impl Launch {
    fn parse(body: Value) -> Result<Self> {
        let l: Self = serde_json::from_value(body).map_err(|_| Error::Validation)?;
        if !matches!(l.kind.as_str(), "VOICE" | "VIDEO") {
            return Err(Error::Validation);
        }
        Ok(l)
    }
}

impl State {
    /// Consumer-safe identity for a participant principal: never the principal id.
    pub async fn participant_view(&self, viewer: &Profile, principal: Uuid) -> Result<Value> {
        if principal == viewer.principal_id {
            return Ok(json!({"self":true}));
        }
        let other = match self
            .store
            .principal(&self.store.db, viewer.scope(), principal)
            .await
        {
            Ok(o) => o,
            Err(Error::NotFound) => return Ok(json!({"self":false,"known":false})),
            Err(e) => return Err(e),
        };
        let relationship = self
            .store
            .relationship(&self.store.db, viewer, &other)
            .await?;
        let contact:Option<Value>=one(&self.store.db,"SELECT jsonb_build_object('id',a.id,'alias',a.alias) doc FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND c.state='CONNECTED' AND ((c.left_profile_id=$1 AND c.right_profile_id=$2) OR (c.right_profile_id=$1 AND c.left_profile_id=$2))",vec![viewer.id.into(),other.id.into()]).await?;
        Ok(
            json!({"self":false,"known":true,"cfMeetId":self.store.current_public_id(&self.store.db,other.id).await?,
            "displayName":other.display_name,"relationship":relationship,
            "contactId":contact.as_ref().map(|c| c["id"].clone()),"alias":contact.as_ref().map(|c| c["alias"].clone())}),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn launch(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        input: &Launch,
        targets: Vec<Uuid>,
        origin: &str,
        conversation: Option<&crate::owners::Conversation>,
        contact: Option<Uuid>,
    ) -> Result<Value> {
        if p.activation_state != Activation::Active {
            return Err(Error::NotActivated);
        }
        if targets.is_empty() || targets.contains(&p.principal_id) {
            return Err(Error::Validation);
        }
        // A replay of an existing link answers from the canonical owner, not from cache.
        let existing:Option<Value>=one(&self.store.db,"SELECT to_jsonb(l) doc FROM communication_call_links l WHERE initiator_profile_id=$1 AND client_request_id=$2",vec![p.id.into(),input.client_request_id.into()]).await?;
        if existing.is_none() {
            self.limit_attempt(p, "call.launch", LAUNCH_LIMIT_PER_HOUR)
                .await?;
        }
        let created = self
            .calling(scope)?
            .call(
                "/internal/v1/communications/calls",
                &json!({"idempotencyKey":input.client_request_id.to_string(),"initiatorPrincipalId":p.principal_id,
                    "initiatorEndpointId":input.endpoint_id,"targetPrincipalIds":targets,"kind":input.kind}),
            )
            .await?;
        let call = uuid_field(&created, "callId")?;
        if let Some(e) = &existing {
            if uuid_field(e, "call_id")? != call {
                return Err(Error::Idempotency);
            }
        }
        let txn = self.store.begin().await?;
        let mut v = vec![Uuid::new_v4().into()];
        v.extend(scoped(p.scope()));
        v.extend([
            p.id.into(),
            call.into(),
            origin.into(),
            conversation.map(|c| c.id).into(),
            contact.into(),
            input.kind.clone().into(),
            input.client_request_id.into(),
        ]);
        let inserted=execute(&txn,"INSERT INTO communication_call_links(id,tenant_id,application_id,environment_id,initiator_profile_id,call_id,origin,conversation_id,contact_id,kind,client_request_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT DO NOTHING",v).await?;
        if inserted == 0 {
            // Same request replayed, or the call id is already linked by this initiator.
            let link: Value = one(
                &txn,
                "SELECT to_jsonb(l) doc FROM communication_call_links l WHERE call_id=$1",
                vec![call.into()],
            )
            .await?
            .ok_or(Error::Idempotency)?;
            if uuid_field(&link, "initiator_profile_id")? != p.id {
                return Err(Error::Idempotency);
            }
        } else {
            self.store
                .audit(
                    &txn,
                    p,
                    "CALL_LAUNCHED",
                    Some(call),
                    json!({"origin":origin,"kind":input.kind}),
                )
                .await?;
            if let Some(c) = conversation {
                // One card per call party (every member: Calling rang them all); never for
                // non-members. A party without a Hub profile yet gets the card owed to it
                // when it first reads its timeline or activity (MSG-009).
                for member in &c.members {
                    let viewer = match self.store.principal(&txn, p.scope(), *member).await {
                        Ok(v) => Some(v),
                        Err(Error::NotFound) => None,
                        Err(e) => return Err(e),
                    };
                    let data = json!({"kind":input.kind,"origin":origin,"anchorSequence":c.last_sequence,
                        "initiatorIsViewer":viewer.as_ref().is_some_and(|v| v.id==p.id),"state":created["state"],"terminal":false});
                    let Some(viewer) = viewer else {
                        let mut v = scoped(p.scope());
                        v.extend([
                            (*member).into(),
                            c.id.into(),
                            format!("call:{call}").into(),
                            call.into(),
                            created["createdAt"].as_str().map(str::to_owned).into(),
                            data.into(),
                        ]);
                        execute(&txn,"INSERT INTO communication_pending_call_cards(tenant_id,application_id,environment_id,principal_id,conversation_id,entry_key,call_id,occurred_at,data) VALUES($1,$2,$3,$4,$5,$6,$7,COALESCE($8::timestamptz,now()),$9) ON CONFLICT DO NOTHING",v).await?;
                        continue;
                    };
                    execute(&txn,"INSERT INTO communication_timeline_entries(viewer_profile_id,conversation_id,entry_key,kind,source,source_id,occurred_at,source_version,data,refreshed_at) VALUES($1,$2,$3,'CALL','CALLING',$4,COALESCE($5::timestamptz,now()),0,$6,'epoch') ON CONFLICT DO NOTHING",
                        vec![viewer.id.into(),c.id.into(),format!("call:{call}").into(),call.into(),created["createdAt"].as_str().map(str::to_owned).into(),data.into()]).await?;
                    self.store
                        .change(&txn, viewer.id, "TIMELINE", Some(c.id), None)
                        .await?;
                }
                self.timeline_nudge(
                    &txn,
                    p.scope(),
                    c.id,
                    &format!("call:{call}:started"),
                    "CALL_STARTED",
                )
                .await?;
            }
        }
        txn.commit().await?;
        Ok(
            json!({"schemaVersion":1,"callId":call,"kind":input.kind,"state":created["state"],"version":created["version"],
            "origin":origin,"conversationId":conversation.map(|c| c.id),"contactId":contact,
            "replayed":created["replayed"]==true || inserted==0,
            "next":"Use the Calling signaling session for ringing, answer and media; the Hub never proxies media"}),
        )
    }

    /// U08: voice/video from an accepted contact.
    pub async fn call_contact(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        contact: Uuid,
        body: Value,
    ) -> Result<Value> {
        let input = Launch::parse(body)?;
        let (_, pair, other) = self.contact(p, contact).await?;
        if pair.state != "CONNECTED" {
            return Err(Error::ActionNotAllowed);
        }
        // Full personal policy (blocks, privacy, account state). Calling re-checks it.
        let decision = self
            .check_policy(
                scope,
                &PolicyCheck {
                    action: Action::DirectCall,
                    actor_principal_id: p.principal_id,
                    target_principal_id: other.principal_id,
                    credential_issued_at: None,
                    credential_principal_id: None,
                    conversation_id: None,
                },
            )
            .await?;
        if !decision.allowed {
            return Err(Error::ActionNotAllowed);
        }
        self.launch(
            scope,
            p,
            &input,
            vec![other.principal_id],
            "CONTACT",
            None,
            Some(contact),
        )
        .await
    }

    /// U09: voice/video from a DIRECT or GROUP conversation the caller belongs to.
    pub async fn call_conversation(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        body: Value,
    ) -> Result<Value> {
        let input = Launch::parse(body)?;
        let c = self.conversation(scope, p, conversation, None).await?;
        if !matches!(c.context_class.as_str(), "DIRECT" | "GROUP") || c.lifecycle_state != "ACTIVE"
        {
            return Err(Error::ActionNotAllowed);
        }
        let targets: Vec<Uuid> = c
            .members
            .iter()
            .copied()
            .filter(|m| *m != p.principal_id)
            .collect();
        if targets.is_empty() {
            return Err(Error::ActionNotAllowed);
        }
        // Group-call authorization stays canonical in Calling (per-target policy, size).
        self.launch(scope, p, &input, targets, "CONVERSATION", Some(&c), None)
            .await
    }

    async fn entry_view(&self, viewer: &Profile, call: &Value) -> Result<Value> {
        let mut counterparts = Vec::new();
        for c in call["counterpartPrincipalIds"]
            .as_array()
            .ok_or(Error::Unavailable)?
        {
            let id = c
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(Error::Unavailable)?;
            counterparts.push(self.participant_view(viewer, id).await?);
        }
        let call_id = uuid_field(call, "callId")?;
        let link:Option<Value>=one(&self.store.db,"SELECT jsonb_build_object('conversationId',conversation_id,'origin',origin) doc FROM communication_call_links WHERE call_id=$1",vec![call_id.into()]).await?;
        let callback = counterparts.len() == 1 && counterparts[0]["contactId"].is_string();
        Ok(
            json!({"callId":call_id,"kind":call["kind"],"state":call["state"],"terminal":call["terminal"],
            "direction":call["direction"],"outcome":call["outcome"],"missed":call["missed"],
            "createdAt":call["createdAt"],"answeredAt":call["answeredAt"],"endedAt":call["endedAt"],"durationMs":call["durationMs"],
            "counterparts":counterparts,"conversationId":link.as_ref().map(|l| l["conversationId"].clone()),
            "origin":link.as_ref().map(|l| l["origin"].clone()).unwrap_or(json!("CALLING")),
            "actions":{"callBack":callback,"callBackContactId":if callback {counterparts[0]["contactId"].clone()} else {Value::Null}}}),
        )
    }

    /// U10: the viewer's canonical call history with safe counterpart identities.
    pub async fn call_history(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        query: &BTreeMap<String, String>,
    ) -> Result<Value> {
        if query
            .keys()
            .any(|k| !matches!(k.as_str(), "limit" | "cursor" | "missed"))
        {
            return Err(Error::Validation);
        }
        let limit = query
            .get("limit")
            .map(|v| v.parse::<i64>())
            .transpose()
            .map_err(|_| Error::Validation)?
            .unwrap_or(25);
        if !(1..=100).contains(&limit) {
            return Err(Error::Validation);
        }
        let mut body = json!({"principalId":p.principal_id,"limit":limit});
        if let Some(c) = query.get("cursor") {
            if c.is_empty() || c.len() > 256 || !c.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(Error::Validation);
            }
            body["cursor"] = json!(c);
        }
        match query.get("missed").map(String::as_str) {
            None => {}
            Some("true") => body["missed"] = json!(true),
            Some("false") => body["missed"] = json!(false),
            _ => return Err(Error::Validation),
        }
        let v = self
            .calling(scope)?
            .call("/internal/v1/communications/calls/history", &body)
            .await?;
        let mut data = Vec::new();
        for call in v["data"].as_array().ok_or(Error::Unavailable)? {
            data.push(self.entry_view(p, call).await?);
        }
        Ok(json!({"schemaVersion":1,"data":data,"nextCursor":v["nextCursor"]}))
    }

    /// U11: one call, for a participant only.
    pub async fn call_context(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        call: Uuid,
    ) -> Result<Value> {
        let v = self
            .calling(scope)?
            .call(
                "/internal/v1/communications/calls/participation",
                &json!({"principalId":p.principal_id,"callIds":[call]}),
            )
            .await?;
        let entry = v["calls"]
            .as_array()
            .ok_or(Error::Unavailable)?
            .iter()
            .find(|c| c["callId"] == json!(call))
            .ok_or(Error::NotFound)?
            .clone();
        let mut view = self.entry_view(p, &entry).await?;
        view["schemaVersion"] = json!(1);
        if let Some(conversation) = view["conversationId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            // Only reveal the conversation link to someone who is still a member.
            let member = match self.conversation(scope, p, conversation, None).await {
                Ok(_) => true,
                Err(Error::NotFound) => false,
                Err(e) => return Err(e),
            };
            if !member {
                view["conversationId"] = Value::Null;
            }
            view["actions"]["openConversation"] = json!(member);
        }
        Ok(view)
    }

    /// Refresh stale non-terminal call cards for one viewer from Calling (bounded).
    /// Returns false when Calling could not be reached (the page is then marked stale).
    pub async fn refresh_call_cards(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        entries: &[Value],
    ) -> Result<bool> {
        let due: Vec<Uuid> = entries
            .iter()
            .filter(|e| e["kind"] == "CALL" && e["data"]["terminal"] != true)
            .filter_map(|e| uuid_field(e, "source_id").ok())
            .take(50)
            .collect();
        if due.is_empty() {
            return Ok(true);
        }
        let response = match self.calling(scope) {
            Ok(c) => {
                c.call(
                    "/internal/v1/communications/calls/participation",
                    &json!({"principalId":p.principal_id,"callIds":due}),
                )
                .await
            }
            Err(e) => Err(e),
        };
        let v = match response {
            Ok(v) => v,
            Err(Error::Database(e)) => return Err(Error::Database(e)),
            Err(_) => return Ok(false),
        };
        for call in v["calls"].as_array().ok_or(Error::Unavailable)? {
            let id = uuid_field(call, "callId")?;
            let version = call["version"].as_i64().unwrap_or(0);
            let patch = json!({"state":call["state"],"terminal":call["terminal"],"outcome":call["outcome"],"missed":call["missed"],
                "direction":call["direction"],"durationMs":call["durationMs"],"answeredAt":call["answeredAt"],"endedAt":call["endedAt"]});
            // Out-of-order safe: a lower owner version never overwrites a newer outcome.
            // A card whose content changed tells the viewer's other devices (TIMELINE).
            let txn = self.store.begin().await?;
            let changed:Vec<Value>=rows(&txn,"WITH old AS (SELECT conversation_id,entry_key,data FROM communication_timeline_entries WHERE viewer_profile_id=$1 AND source='CALLING' AND source_id=$2 AND source_version<=$4 FOR UPDATE) \
                UPDATE communication_timeline_entries e SET data=e.data||$3,source_version=$4,refreshed_at=now() FROM old WHERE e.viewer_profile_id=$1 AND e.conversation_id=old.conversation_id AND e.entry_key=old.entry_key \
                RETURNING jsonb_build_object('conversation',e.conversation_id,'changed',old.data IS DISTINCT FROM old.data||$3) doc",
                vec![p.id.into(),id.into(),patch.into(),version.into()]).await?;
            for c in changed.iter().filter(|c| c["changed"] == true) {
                let conversation = uuid_field(c, "conversation")?;
                self.store
                    .change(&txn, p.id, "TIMELINE", Some(conversation), Some(version))
                    .await?;
                // MSG-009: the call's state moved. The other parties' cards of this call
                // are re-read from Calling under their own delegation at their next read or
                // sweep (outcomes are per party), and the conversation's members are nudged
                // once per Calling version, however many viewers observe it.
                execute(&txn,"UPDATE communication_timeline_entries SET refreshed_at='epoch' WHERE source='CALLING' AND source_id=$1 AND conversation_id=$2 AND viewer_profile_id<>$3 AND source_version<$4",
                    vec![id.into(),conversation.into(),p.id.into(),version.into()]).await?;
                self.timeline_nudge(
                    &txn,
                    p.scope(),
                    conversation,
                    &format!("call:{id}:v{version}"),
                    "CALL_UPDATED",
                )
                .await?;
            }
            txn.commit().await?;
        }
        // Calls no longer visible to this viewer are not refreshed again on every read.
        execute(&self.store.db,"UPDATE communication_timeline_entries SET refreshed_at=now() WHERE viewer_profile_id=$1 AND source='CALLING' AND source_id IN (SELECT jsonb_array_elements_text($2::jsonb)::uuid)",
            vec![p.id.into(),json!(due).into()]).await?;
        Ok(true)
    }

    /// Section 17 `call.outcome_changed` without a Calling producer: every non-terminal
    /// call card older than CALL_REFRESH_SECONDS (and younger than a day) is re-read
    /// from Calling under its viewer's own delegated context, oldest first and bounded.
    /// A changed card writes a TIMELINE feed entry, so all of the viewer's devices
    /// converge even when none is reading that conversation. Calling stays canonical:
    /// an unreachable Calling leaves cards unchanged and they are retried next sweep.
    pub async fn reconcile_call_cards(&self, viewers: i64) -> Result<usize> {
        const DUE: &str =
            "kind='CALL' AND source='CALLING' AND COALESCE(data->>'terminal','false')<>'true' \
            AND refreshed_at<now()-make_interval(secs=>$1) AND occurred_at>now()-interval '1 day'";
        let due:Vec<Value>=rows(&self.store.db,&format!("SELECT jsonb_build_object('profile',viewer_profile_id) doc FROM communication_timeline_entries WHERE {DUE} GROUP BY viewer_profile_id ORDER BY min(refreshed_at) LIMIT $2"),
            vec![(CALL_REFRESH_SECONDS as f64).into(),viewers.into()]).await?;
        let mut refreshed = 0;
        for d in due {
            let Some(p): Option<Profile> = one(&self.store.db,"SELECT to_jsonb(p) doc FROM communication_profiles p WHERE id=$1 AND activation_state='ACTIVE'",
                vec![uuid_field(&d, "profile")?.into()]).await? else { continue };
            let Some(a): Option<Value> = one(&self.store.db,"SELECT jsonb_build_object('serviceAccountId',service_account_id,'environmentKind',environment_kind) doc FROM communication_scope_authorities WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3",
                vec![p.tenant_id.into(),p.application_id.into(),p.environment_id.into()]).await? else { continue };
            let kind: cf_service_auth::ApplicationEnvironmentKind =
                serde_json::from_value(a["environmentKind"].clone())
                    .map_err(|_| Error::Unavailable)?;
            let scope = DelegatedPlatformContext::new(
                p.tenant_id,
                p.application_id,
                p.environment_id,
                kind,
                uuid_field(&a, "serviceAccountId")?,
            )
            .with_initiating_principal(
                p.principal_id,
                cf_service_auth::DelegatedPrincipalKind::NativeCfUser,
            );
            let entries:Vec<Value>=rows(&self.store.db,&format!("SELECT to_jsonb(e) doc FROM communication_timeline_entries e WHERE viewer_profile_id=$2 AND {DUE} ORDER BY refreshed_at LIMIT 50"),
                vec![(CALL_REFRESH_SECONDS as f64).into(),p.id.into()]).await?;
            if self.refresh_call_cards(&scope, &p, &entries).await? {
                refreshed += entries.len();
            }
        }
        Ok(refreshed)
    }

    /// U04: per-viewer call/meeting cards of one conversation, anchored to message
    /// sequences so clients merge them with the Messaging history they already page.
    pub async fn timeline(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        query: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let (limit, cursor) = timeline_page(query)?;
        let c = self.conversation(scope, p, conversation, None).await?;
        self.materialize_call_cards(p).await?;
        self.materialize_meeting_cards(p, &c).await?;
        let (mark, cleared_at) = self.watermark_full(p, conversation).await?;
        let (after_at, after_key) = cursor.unwrap_or_else(|| ("-infinity".into(), String::new()));
        let mut entries:Vec<Value>=rows(&self.store.db,"SELECT to_jsonb(e) doc FROM communication_timeline_entries e WHERE viewer_profile_id=$1 AND conversation_id=$2 AND (occurred_at,entry_key)>($3::timestamptz,$4) AND NOT (COALESCE((data->>'anchorSequence')::bigint,0)<$5 OR (COALESCE((data->>'anchorSequence')::bigint,0)=$5 AND occurred_at<=$6::timestamptz)) ORDER BY occurred_at,entry_key LIMIT $7",
            vec![p.id.into(),conversation.into(),after_at.into(),after_key.into(),mark.into(),cleared_at.into(),(limit+1).into()]).await?;
        let more = entries.len() > limit as usize;
        entries.truncate(limit as usize);
        let stale_due: Vec<Value> = entries.iter().filter(|e| refresh_due(e)).cloned().collect();
        let fresh = self.refresh_call_cards(scope, p, &stale_due).await?;
        if !stale_due.is_empty() && fresh {
            let keys: Vec<String> = entries
                .iter()
                .filter_map(|e| e["entry_key"].as_str().map(str::to_owned))
                .collect();
            entries = rows(&self.store.db,"SELECT to_jsonb(e) doc FROM communication_timeline_entries e WHERE viewer_profile_id=$1 AND conversation_id=$2 AND entry_key IN (SELECT jsonb_array_elements_text($3::jsonb)) ORDER BY occurred_at,entry_key",
                vec![p.id.into(),conversation.into(),json!(keys).into()]).await?;
        }
        let next = if more {
            entries.last().map(|e| {
                encode_cursor(
                    e["occurred_at"].as_str().unwrap_or(""),
                    e["entry_key"].as_str().unwrap_or(""),
                )
            })
        } else {
            None
        };
        let data: Vec<Value> = entries.iter().map(timeline_view).collect();
        Ok(
            json!({"schemaVersion":1,"projectionVersion":1,"conversationId":conversation,"clearedThroughSequence":mark,
            "stale":!stale_due.is_empty() && !fresh,"data":data,"nextCursor":next,
            "merge":"Order with Messaging history by anchorSequence, then occurredAt, then entryId"}),
        )
    }

    /// Section 14 (MSG-009): a meeting shared into a conversation is visible to every
    /// member, but cards were only written for members who had a Hub profile at that
    /// moment. A current member opening the timeline gets the missing invitation cards,
    /// copied from the conversation's existing cards, unless the share predates the
    /// history Messaging lets this member see (`visibleFromSequence`). Call cards are
    /// per call party (Calling decides) and are not copied.
    async fn materialize_meeting_cards(
        &self,
        p: &Profile,
        c: &crate::owners::Conversation,
    ) -> Result<()> {
        let visible_from = c.raw["visibleFromSequence"].as_i64().unwrap_or(0);
        let added = execute(&self.store.db,"INSERT INTO communication_timeline_entries(viewer_profile_id,conversation_id,entry_key,kind,source,source_id,occurred_at,source_version,data)
            SELECT DISTINCT ON (e.entry_key) $1,e.conversation_id,e.entry_key,e.kind,e.source,e.source_id,e.occurred_at,e.source_version,e.data||jsonb_build_object('sharedByViewer',false)
            FROM communication_timeline_entries e
            WHERE e.conversation_id=$2 AND e.kind='MEETING_INVITATION' AND e.viewer_profile_id<>$1
              AND COALESCE((e.data->>'anchorSequence')::bigint,0)+1>=$3
              AND NOT EXISTS (SELECT 1 FROM communication_timeline_entries x WHERE x.viewer_profile_id=$1 AND x.conversation_id=$2 AND x.entry_key=e.entry_key)
            ORDER BY e.entry_key,e.occurred_at
            ON CONFLICT DO NOTHING",
            vec![p.id.into(),c.id.into(),visible_from.into()]).await?;
        if added > 0 {
            self.store
                .change(&self.store.db, p.id, "MEETING_LINK", Some(c.id), None)
                .await?;
        }
        Ok(())
    }

    /// MSG-009: call cards owed to this viewer because it was a party to a conversation
    /// call launched before it had a Hub profile. Written once (moved out of the pending
    /// table in the same statement), due for an immediate refresh from Calling under the
    /// viewer's own delegation, and announced to the viewer's devices through its feed.
    /// Only the viewer's own principal is matched; membership is checked by every read
    /// that shows a card (timeline: the conversation; activity: each card's conversation).
    pub async fn materialize_call_cards(&self, p: &Profile) -> Result<u64> {
        let mut v = scoped(p.scope());
        v.extend([p.principal_id.into(), p.id.into()]);
        let txn = self.store.begin().await?;
        let added: Vec<Value> = rows(&txn,"WITH moved AS (DELETE FROM communication_pending_call_cards WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND principal_id=$4 RETURNING conversation_id,entry_key,call_id,occurred_at,data)
            INSERT INTO communication_timeline_entries(viewer_profile_id,conversation_id,entry_key,kind,source,source_id,occurred_at,source_version,data,refreshed_at)
            SELECT $5,conversation_id,entry_key,'CALL','CALLING',call_id,occurred_at,0,data,'epoch' FROM moved
            ON CONFLICT DO NOTHING RETURNING jsonb_build_object('conversation',conversation_id) doc",v).await?;
        let mut announced = std::collections::BTreeSet::new();
        for a in &added {
            let conversation = uuid_field(a, "conversation")?;
            if announced.insert(conversation) {
                self.store
                    .change(&txn, p.id, "TIMELINE", Some(conversation), None)
                    .await?;
            }
        }
        txn.commit().await?;
        Ok(added.len() as u64)
    }

    /// MSG-009: tells Messaging that a conversation's call / meeting timeline changed, so
    /// it publishes `timeline.changed` to the conversation's members (realtime and
    /// replay). `key` names one distinct change (a call started, a Calling version, a
    /// meeting shared): repeated observations of the same change are deduplicated, and
    /// each new one bumps the conversation's monotonic timeline version once, in the
    /// caller's transaction. Without a Messaging owner nothing is queued.
    pub(crate) async fn timeline_nudge<C: sea_orm::ConnectionTrait>(
        &self,
        db: &C,
        scope: cf_communications_policy::Scope,
        conversation: Uuid,
        key: &str,
        reason: &str,
    ) -> Result<bool> {
        if !self.config.messaging_configured {
            return Ok(false);
        }
        let fresh = execute(db,"INSERT INTO communication_timeline_nudges(conversation_id,nudge_key) VALUES($1,$2) ON CONFLICT DO NOTHING",
            vec![conversation.into(),key.into()]).await?;
        if fresh == 0 {
            return Ok(false);
        }
        let mut v = vec![conversation.into()];
        v.extend(scoped(scope));
        let version: Value = one(db,"INSERT INTO communication_timeline_versions AS t (conversation_id,tenant_id,application_id,environment_id,version) VALUES($1,$2,$3,$4,1) ON CONFLICT (conversation_id) DO UPDATE SET version=t.version+1,updated_at=now() RETURNING jsonb_build_object('version',t.version) doc",v)
            .await?.ok_or(Error::Unavailable)?;
        let version = version["version"].as_i64().ok_or(Error::Unavailable)?;
        self.store
            .messaging_event(
                db,
                scope,
                "communications.timeline.changed",
                conversation,
                version,
                json!({"conversationId":conversation,"timelineVersion":version,"reason":reason}),
            )
            .await?;
        Ok(true)
    }

    /// U03: the viewer's recent activity cards across conversations they still belong to.
    pub async fn activity(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        query: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let (limit, cursor) = timeline_page(query)?;
        let (before_at, before_key) = cursor.unwrap_or_else(|| ("infinity".into(), "~".into()));
        self.materialize_call_cards(p).await?;
        const PAGE: &str = "SELECT to_jsonb(e) doc FROM communication_timeline_entries e WHERE viewer_profile_id=$1 AND (occurred_at,entry_key)<($2::timestamptz,$3) ORDER BY occurred_at DESC,entry_key DESC LIMIT $4";
        let page = || {
            vec![
                p.id.into(),
                before_at.clone().into(),
                before_key.clone().into(),
                (limit + 1).into(),
            ]
        };
        let mut entries: Vec<Value> = rows(&self.store.db, PAGE, page()).await?;
        // Activity shows the same canonical outcomes as the conversation timeline.
        let due: Vec<Value> = entries.iter().filter(|e| refresh_due(e)).cloned().collect();
        if !due.is_empty() && self.refresh_call_cards(scope, p, &due).await? {
            entries = rows(&self.store.db, PAGE, page()).await?;
        }
        let more = entries.len() > limit as usize;
        let mut membership: BTreeMap<Uuid, (bool, i64, String)> = BTreeMap::new();
        let mut data = Vec::new();
        let mut last = None;
        for e in entries.into_iter().take(limit as usize) {
            last = Some(encode_cursor(
                e["occurred_at"].as_str().unwrap_or(""),
                e["entry_key"].as_str().unwrap_or(""),
            ));
            let conversation = uuid_field(&e, "conversation_id")?;
            if let std::collections::btree_map::Entry::Vacant(slot) = membership.entry(conversation)
            {
                let member = match self.conversation(scope, p, conversation, None).await {
                    Ok(_) => true,
                    Err(Error::NotFound) => false,
                    Err(e) => return Err(e),
                };
                let (mark, at) = self.watermark_full(p, conversation).await?;
                slot.insert((member, mark, at));
            }
            let (member, mark, cleared_at) = membership[&conversation].clone();
            if member && !cleared(&e, mark, &cleared_at) {
                data.push(timeline_view(&e));
            }
        }
        Ok(
            json!({"schemaVersion":1,"projectionVersion":1,"data":data,"nextCursor":if more {last} else {None}}),
        )
    }
}

/// A non-terminal card last refreshed CALL_REFRESH_SECONDS or more ago.
fn refresh_due(e: &Value) -> bool {
    e["data"]["terminal"] != true
        && e["refreshed_at"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_none_or(|t| {
                chrono::Utc::now().signed_duration_since(t).num_seconds() >= CALL_REFRESH_SECONDS
            })
}

/// A card is cleared when anchored before the watermark, or at it but not after the clear.
fn cleared(e: &Value, mark: i64, cleared_at: &str) -> bool {
    let anchor = e["data"]["anchorSequence"].as_i64().unwrap_or(0);
    let at = |t: &str| chrono::DateTime::parse_from_rfc3339(t).ok();
    anchor < mark
        || anchor == mark
            && match (at(e["occurred_at"].as_str().unwrap_or("")), at(cleared_at)) {
                (Some(o), Some(c)) => o <= c,
                (_, None) => false,
                _ => true,
            }
}
pub fn timeline_view(e: &Value) -> Value {
    let d = &e["data"];
    json!({"entryId":e["entry_key"],"kind":e["kind"],"source":e["source"],"sourceId":e["source_id"],"conversationId":e["conversation_id"],
        "occurredAt":e["occurred_at"],"anchorSequence":d["anchorSequence"],"sourceVersion":e["source_version"],"refreshedAt":e["refreshed_at"],
        "call":if e["kind"]=="CALL" {json!({"kind":d["kind"],"state":d["state"],"terminal":d["terminal"],"outcome":d["outcome"],
            "missed":d["missed"],"direction":d["direction"],"durationMs":d["durationMs"],"initiatedByViewer":d["initiatorIsViewer"]})} else {Value::Null},
        "meeting":if e["kind"]=="MEETING_INVITATION" {json!({"meetingCode":d["meetingCode"],"origin":d["origin"],"sharedByViewer":d["sharedByViewer"]})} else {Value::Null}})
}
pub fn encode_cursor(at: &str, key: &str) -> String {
    hex::encode(format!("{at}|{key}"))
}
pub fn timeline_page(query: &BTreeMap<String, String>) -> Result<(i64, Option<(String, String)>)> {
    if query
        .keys()
        .any(|k| !matches!(k.as_str(), "limit" | "cursor"))
    {
        return Err(Error::Validation);
    }
    let limit = query
        .get("limit")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| Error::Validation)?
        .unwrap_or(30);
    if !(1..=100).contains(&limit) {
        return Err(Error::Validation);
    }
    let cursor = match query.get("cursor") {
        None => None,
        Some(c) => {
            let raw = hex::decode(c)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .ok_or(Error::Validation)?;
            let (at, key) = raw.split_once('|').ok_or(Error::Validation)?;
            chrono::DateTime::parse_from_rfc3339(at).map_err(|_| Error::Validation)?;
            if key.len() > 120 {
                return Err(Error::Validation);
            }
            Some((at.to_owned(), key.to_owned()))
        }
    };
    Ok((limit, cursor))
}

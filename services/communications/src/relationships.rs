use crate::{
    error::{Error, Result},
    model::*,
    store::{execute, one, rows},
    State,
};
use cf_service_auth::{DelegatedPlatformContext, OutboundServiceRequest, ServiceId};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

impl State {
    pub async fn limit_attempt(&self, p: &Profile, operation: &str, limit: i64) -> Result<()> {
        let txn = self.store.begin().await?;
        self.store.rate(&txn, p.id, operation, limit).await?;
        txn.commit().await?;
        Ok(())
    }
    pub async fn send_request(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Send {
            cf_meet_id: String,
        }
        let request: Send = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        self.limit_attempt(p, "contact-request.attempt", 60).await?;
        let target = self
            .store
            .by_public_id(&self.store.db, p.scope(), &request.cf_meet_id)
            .await?;
        if p.id == target.id {
            return Err(Error::Validation);
        }
        let claim:Value=one(&self.store.db,"SELECT jsonb_build_object('valid',EXISTS(SELECT 1 FROM communication_invite_claims c JOIN communication_invites i ON i.id=c.invite_id WHERE c.requester_profile_id=$1 AND i.owner_profile_id=$2 AND i.revoked_at IS NULL AND i.expires_at>now())) doc",vec![p.id.into(),target.id.into()]).await?.ok_or(Error::Unavailable)?;
        self.visible_profile(scope, p, &target, claim["valid"] == true)
            .await?;
        let txn = self.store.begin().await?;
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "contact-request.create", key, &body)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let pair = self
            .store
            .lock_pair(&txn, p.scope(), p.id, target.id)
            .await?;
        // Recheck the policy while holding the same pair lock as blocking/removal.
        if self.store.blocked(&txn, p.id, target.id).await?
            || self
                .store
                .privacy(&txn, target.id)
                .await?
                .contact_request_permissions
                != RequestPermission::Everyone
        {
            return Err(Error::NotFound);
        }
        if pair.state == "CONNECTED" {
            return Err(Error::Pending);
        }
        let old:Option<ContactRequest>=one(&txn,"SELECT to_jsonb(r) doc FROM communication_contact_requests r WHERE connection_id=$1 ORDER BY created_at DESC LIMIT 1 FOR UPDATE",vec![pair.id.into()]).await?;
        if let Some(old) = old {
            if old.state == "PENDING" && old.expires_at > Utc::now() {
                if old.sender_profile_id == p.id {
                    let result =
                        request_view(&old, self.store.current_public_id(&txn, target.id).await?);
                    self.store
                        .finish_idempotency(&txn, p, "contact-request.create", key, result.clone())
                        .await?;
                    txn.commit().await?;
                    return Ok(result);
                }
                return Err(Error::Pending);
            }
            if old.state == "PENDING" {
                execute(&txn,"UPDATE communication_contact_requests SET state='EXPIRED',version=version+1,updated_at=now() WHERE id=$1",vec![old.id.into()]).await?;
            }
            let recent:Value=one(&txn,"SELECT jsonb_build_object('cooldown',EXISTS(SELECT 1 FROM communication_contact_requests WHERE connection_id=$1 AND state IN ('DECLINED','CANCELLED') AND updated_at>now()-interval '24 hours')) doc",vec![pair.id.into()]).await?.ok_or(Error::Unavailable)?;
            if recent["cooldown"] == true {
                return Err(Error::RateLimited);
            }
        }
        let id = Uuid::new_v4();
        execute(&txn,"INSERT INTO communication_contact_requests(id,connection_id,sender_profile_id,recipient_profile_id) VALUES($1,$2,$3,$4)",vec![id.into(),pair.id.into(),p.id.into(),target.id.into()]).await?;
        execute(&txn,"UPDATE communication_connections SET state='REQUEST_PENDING',version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into()]).await?;
        self.store
            .audit(&txn, p, "CONNECTION_REQUESTED", Some(id), json!({}))
            .await?;
        self.store.event(&txn,p,"communications.connection.requested",pair.id,pair.version+1,json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":target.principal_id,"state":"REQUEST_PENDING"})).await?;
        self.store
            .relationship_changed(&txn, &[p.id, target.id], pair.id, pair.version + 1)
            .await?;
        let result = json!({"schemaVersion":1,"requestId":id,"cfMeetId":request.cf_meet_id,"state":"PENDING","version":1});
        self.store
            .finish_idempotency(&txn, p, "contact-request.create", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn decide_request(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        id: Uuid,
        action: &str,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Version {
            expected_version: i64,
        }
        let request: Version =
            serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        let operation = format!("contact-request.{action}");
        let txn = self.store.begin().await?;
        let bound = json!({"requestId":id,"body":body});
        if let Some(v) = self
            .store
            .idempotency(&txn, p, &operation, key, &bound)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let r:ContactRequest=one(&txn,"SELECT to_jsonb(r) doc FROM communication_contact_requests r WHERE id=$1 AND (sender_profile_id=$2 OR recipient_profile_id=$2)",vec![id.into(),p.id.into()]).await?.ok_or(Error::NotFound)?;
        if (action == "cancel" && r.sender_profile_id != p.id)
            || (action != "cancel" && r.recipient_profile_id != p.id)
        {
            return Err(Error::NotFound);
        }
        let other_id = if r.sender_profile_id == p.id {
            r.recipient_profile_id
        } else {
            r.sender_profile_id
        };
        let other = self.store.profile(&txn, p.scope(), other_id, false).await?;
        let pair = self
            .store
            .lock_pair(&txn, p.scope(), p.id, other.id)
            .await?;
        let r: ContactRequest = one(
            &txn,
            "SELECT to_jsonb(r) doc FROM communication_contact_requests r WHERE id=$1 FOR UPDATE",
            vec![id.into()],
        )
        .await?
        .ok_or(Error::NotFound)?;
        let next = match action {
            "accept" => "ACCEPTED",
            "decline" => "DECLINED",
            "cancel" => "CANCELLED",
            _ => return Err(Error::Validation),
        };
        if r.state == next {
            let result = request_view(&r, self.store.current_public_id(&txn, other.id).await?);
            self.store
                .finish_idempotency(&txn, p, &operation, key, result.clone())
                .await?;
            txn.commit().await?;
            return Ok(result);
        }
        if r.state != "PENDING"
            || r.version != request.expected_version
            || r.expires_at <= Utc::now()
        {
            return Err(Error::Version);
        }
        if self.store.blocked(&txn, p.id, other.id).await? {
            return Err(Error::NotFound);
        }
        if action == "accept"
            && (other.activation_state != Activation::Active
                || !self.active_account(scope, &other).await?
                || self.domain_blocks(scope, p, &other).await?)
        {
            return Err(Error::NotFound);
        }
        execute(&txn,"UPDATE communication_contact_requests SET state=$2,version=version+1,updated_at=now() WHERE id=$1",vec![id.into(),next.into()]).await?;
        let relationship = if action == "accept" {
            "CONNECTED"
        } else {
            "NONE"
        };
        execute(&txn,"UPDATE communication_connections SET state=$2,connected_at=CASE WHEN $2='CONNECTED' THEN now() ELSE connected_at END,version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into(),relationship.into()]).await?;
        if action == "accept" {
            for owner in [p.id, other.id] {
                execute(&txn,"INSERT INTO communication_contact_aliases(id,owner_profile_id,connection_id) VALUES($1,$2,$3) ON CONFLICT(owner_profile_id,connection_id) DO NOTHING",vec![Uuid::new_v4().into(),owner.into(),pair.id.into()]).await?;
            }
        }
        self.store
            .audit(
                &txn,
                p,
                &format!("CONNECTION_REQUEST_{}", next),
                Some(id),
                json!({}),
            )
            .await?;
        self.store.event(&txn,p,&format!("communications.connection.{}",relationship.to_lowercase()),pair.id,pair.version+1,json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":other.principal_id,"state":relationship})).await?;
        self.store
            .relationship_changed(&txn, &[p.id, other.id], pair.id, pair.version + 1)
            .await?;
        let result = json!({"schemaVersion":1,"requestId":id,"cfMeetId":self.store.current_public_id(&txn,other.id).await?,"state":next,"version":r.version+1,"relationship":relationship});
        self.store
            .finish_idempotency(&txn, p, &operation, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn list_requests(
        &self,
        p: &Profile,
        incoming: bool,
        limit: i64,
        cursor: Uuid,
    ) -> Result<Value> {
        let column = if incoming {
            "recipient_profile_id"
        } else {
            "sender_profile_id"
        };
        let requests:Vec<ContactRequest>=rows(&self.store.db,&format!("SELECT to_jsonb(r) doc FROM communication_contact_requests r WHERE {column}=$1 AND id>$2 ORDER BY id LIMIT $3"),vec![p.id.into(),cursor.into(),(limit+1).into()]).await?;
        let more = requests.len() > limit as usize;
        let mut data = Vec::new();
        let mut next = None;
        for request in requests.into_iter().take(limit as usize) {
            next = Some(request.id);
            let other = if incoming {
                request.sender_profile_id
            } else {
                request.recipient_profile_id
            };
            // Requests are already scoped by the owner's immutable profile. A live
            // block hides the request immediately, even before event propagation.
            if self.store.blocked(&self.store.db, p.id, other).await? {
                continue;
            }
            data.push(request_view(
                &request,
                self.store.current_public_id(&self.store.db, other).await?,
            ));
            next = Some(request.id);
        }
        Ok(json!({"schemaVersion":1,"data":data,"nextCursor":if more {next} else {None}}))
    }
    pub async fn contact(&self, p: &Profile, id: Uuid) -> Result<(Value, Connection, Profile)> {
        let v:Value=one(&self.store.db,"SELECT to_jsonb(a) doc FROM communication_contact_aliases a WHERE id=$1 AND owner_profile_id=$2",vec![id.into(),p.id.into()]).await?.ok_or(Error::NotFound)?;
        let connection = uuid_field(&v, "connection_id")?;
        let pair: Connection = one(
            &self.store.db,
            "SELECT to_jsonb(c) doc FROM communication_connections c WHERE id=$1",
            vec![connection.into()],
        )
        .await?
        .ok_or(Error::NotFound)?;
        let other = self
            .store
            .profile(
                &self.store.db,
                p.scope(),
                pair.other(p.id).ok_or(Error::NotFound)?,
                false,
            )
            .await?;
        Ok((v, pair, other))
    }
    pub async fn contact_view(&self, p: &Profile, id: Uuid) -> Result<Value> {
        let (alias, pair, other) = self.contact(p, id).await?;
        let state = self.store.relationship(&self.store.db, p, &other).await?;
        Ok(
            json!({"schemaVersion":1,"contactId":id,"cfMeetId":self.store.current_public_id(&self.store.db,other.id).await?,"displayName":other.display_name,"avatarReference":other.avatar_reference,
            "relationship":state,"relationshipVersion":pair.version,"alias":alias["alias"],"favorite":alias["favorite"],"notifications":alias["notifications"],"version":alias["version"]}),
        )
    }
    pub async fn list_contacts(&self, p: &Profile, limit: i64, cursor: Uuid) -> Result<Value> {
        let values:Vec<Value>=rows(&self.store.db,"SELECT jsonb_build_object('id',a.id) doc FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND a.id>$2 AND c.state='CONNECTED' ORDER BY a.id LIMIT $3",vec![p.id.into(),cursor.into(),(limit+1).into()]).await?;
        let more = values.len() > limit as usize;
        let mut data = Vec::new();
        let mut last = None;
        for v in values.into_iter().take(limit as usize) {
            let id = uuid_field(&v, "id")?;
            data.push(self.contact_view(p, id).await?);
            last = Some(id);
        }
        Ok(json!({"schemaVersion":1,"data":data,"nextCursor":if more {last} else {None}}))
    }
    pub async fn patch_contact(
        &self,
        p: &Profile,
        id: Uuid,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Patch {
            expected_version: i64,
            alias: Option<String>,
            favorite: Option<bool>,
            notifications: Option<String>,
        }
        let update: Patch = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if update
            .alias
            .as_ref()
            .is_some_and(|a| a.chars().count() > 160 || a.chars().any(char::is_control))
            || update
                .notifications
                .as_ref()
                .is_some_and(|n| !matches!(n.as_str(), "ALL" | "MUTED" | "MENTIONS"))
        {
            return Err(Error::Validation);
        }
        let (old, pair, _) = self.contact(p, id).await?;
        let txn = self.store.begin().await?;
        let binding = json!({"contactId":id,"body":body});
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "contact.patch", key, &binding)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let alias = if body.get("alias").is_some() {
            update.alias
        } else {
            old["alias"].as_str().map(str::to_owned)
        };
        let favorite = update.favorite.unwrap_or(old["favorite"] == true);
        let notifications = update
            .notifications
            .unwrap_or_else(|| old["notifications"].as_str().unwrap_or("ALL").to_owned());
        let changed=execute(&txn,"UPDATE communication_contact_aliases SET alias=$4,favorite=$5,notifications=$6,version=version+1,updated_at=now() WHERE id=$1 AND owner_profile_id=$2 AND version=$3",vec![id.into(),p.id.into(),update.expected_version.into(),alias.clone().into(),favorite.into(),notifications.clone().into()]).await?;
        if changed != 1 {
            return Err(Error::Version);
        }
        self.store
            .audit(
                &txn,
                p,
                "CONTACT_PREFERENCES_UPDATED",
                Some(id),
                json!({"relationshipVersion":pair.version}),
            )
            .await?;
        self.store
            .change(
                &txn,
                p.id,
                "CONTACT",
                Some(id),
                Some(update.expected_version + 1),
            )
            .await?;
        let response = json!({"schemaVersion":1,"contactId":id,"alias":alias,"favorite":favorite,"notifications":notifications,"version":update.expected_version+1});
        self.store
            .finish_idempotency(&txn, p, "contact.patch", key, response.clone())
            .await?;
        txn.commit().await?;
        Ok(response)
    }
    pub async fn remove_contact(&self, p: &Profile, id: Uuid, key: Uuid) -> Result<Value> {
        let (_, _, other) = self.contact(p, id).await?;
        let txn = self.store.begin().await?;
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "contact.remove", key, &json!({"contactId":id}))
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let pair = self
            .store
            .lock_pair(&txn, p.scope(), p.id, other.id)
            .await?;
        if pair.state == "CONNECTED" {
            execute(&txn,"UPDATE communication_connections SET state='REVOKED',revoked_at=now(),version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into()]).await?;
            self.store
                .audit(&txn, p, "CONNECTION_REMOVED", Some(pair.id), json!({}))
                .await?;
            self.store.event(&txn,p,"communications.connection.revoked",pair.id,pair.version+1,json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":other.principal_id,"state":"REVOKED"})).await?;
            self.store
                .relationship_changed(&txn, &[p.id, other.id], pair.id, pair.version + 1)
                .await?;
        }
        let result = json!({"schemaVersion":1,"contactId":id,"relationship":"REVOKED"});
        self.store
            .finish_idempotency(&txn, p, "contact.remove", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn set_block(
        &self,
        p: &Profile,
        target: &Profile,
        active: bool,
        key: Uuid,
    ) -> Result<Value> {
        let txn = self.store.begin().await?;
        let body = json!({"target":target.id,"active":active});
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "block.update", key, &body)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let pair = self
            .store
            .lock_pair(&txn, p.scope(), p.id, target.id)
            .await?;
        execute(&txn,"INSERT INTO communication_block_bridge(blocker_profile_id,blocked_profile_id,source,active) VALUES($1,$2,'COMMUNICATIONS',$3) ON CONFLICT(blocker_profile_id,blocked_profile_id,source) DO UPDATE SET active=$3,source_version=communication_block_bridge.source_version+1,updated_at=now()",vec![p.id.into(),target.id.into(),active.into()]).await?;
        let state = if active { "BLOCKED" } else { "REVOKED" };
        execute(&txn,"UPDATE communication_connections SET state=$2,version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into(),state.into()]).await?;
        if active {
            execute(&txn,"UPDATE communication_contact_requests SET state='CANCELLED',version=version+1,updated_at=now() WHERE connection_id=$1 AND state='PENDING'",vec![pair.id.into()]).await?;
        }
        self.store
            .audit(
                &txn,
                p,
                if active {
                    "PERSON_BLOCKED"
                } else {
                    "PERSON_UNBLOCKED"
                },
                Some(pair.id),
                json!({}),
            )
            .await?;
        self.store.event(&txn,p,"communications.block.changed",pair.id,pair.version+1,json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":target.principal_id,"blocked":active})).await?;
        // The blocker's devices refresh their block list; the other person's devices
        // refresh the relationship (no block state is disclosed to them).
        self.store
            .change(&txn, p.id, "BLOCK", Some(pair.id), Some(pair.version + 1))
            .await?;
        self.store
            .change(
                &txn,
                target.id,
                "CONTACT",
                Some(pair.id),
                Some(pair.version + 1),
            )
            .await?;
        let result = json!({"schemaVersion":1,"cfMeetId":self.store.current_public_id(&txn,target.id).await?,"blocked":active,"relationship":state});
        self.store
            .finish_idempotency(&txn, p, "block.update", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn common_groups(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        id: Uuid,
    ) -> Result<Value> {
        let (_, pair, other) = self.contact(p, id).await?;
        if pair.state != "CONNECTED"
            || self.store.blocked(&self.store.db, p.id, other.id).await?
            || self.domain_blocks(scope, p, &other).await?
        {
            return Err(Error::NotFound);
        }
        let req = OutboundServiceRequest::new_operation(ServiceId::messaging())
            .with_delegated_context(scope.clone());
        let r = self
            .config
            .client
            .post_json(
                &req,
                "/internal/v1/communications/common-groups",
                &json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":other.principal_id}),
            )
            .await
            .map_err(|_| Error::Unavailable)?;
        if r.status() != 200 {
            return Err(Error::Unavailable);
        }
        let value: Value = r.json().map_err(|_| Error::Unavailable)?;
        // The Messaging adapter emits only canonical conversation ids and safe names.
        let groups = value["groups"]
            .as_array()
            .filter(|a| a.len() <= 100)
            .ok_or(Error::Unavailable)?;
        let mut safe = Vec::new();
        for g in groups {
            let id = uuid_field(g, "conversationId")?;
            if g["contextClass"] != "GROUP" {
                return Err(Error::Unavailable);
            }
            safe.push(json!({"conversationId":id,"contextClass":"GROUP"}));
        }
        Ok(json!({"schemaVersion":1,"groups":safe}))
    }
}
pub fn uuid_field(v: &Value, key: &str) -> Result<Uuid> {
    v[key]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(Error::Unavailable)
}
fn request_view(r: &ContactRequest, cf_id: String) -> Value {
    json!({"schemaVersion":1,"requestId":r.id,"cfMeetId":cf_id,"state":if r.state=="PENDING" && r.expires_at<=Utc::now() {"EXPIRED"} else {&r.state},"version":r.version,"expiresAt":r.expires_at})
}

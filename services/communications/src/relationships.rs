use crate::{
    error::{Error, Result},
    model::*,
    notification_publications::{self, ConnectionNotice, Notice},
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
        // K2: the other person already asked us. Adding them back accepts their request
        // (both connected), so their discoverability does not stand in the way: we hold
        // their request. The accept checks below apply.
        let incoming: Value = one(&self.store.db,"SELECT jsonb_build_object('pending',EXISTS(SELECT 1 FROM communication_contact_requests WHERE sender_profile_id=$1 AND recipient_profile_id=$2 AND state='PENDING' AND expires_at>now())) doc",vec![target.id.into(),p.id.into()]).await?.ok_or(Error::Unavailable)?;
        if incoming["pending"] != true {
            let claim = self
                .store
                .invite_claim(&self.store.db, p.id, target.id)
                .await?;
            self.visible_profile(scope, p, &target, claim).await?;
        }
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
        if self.store.blocked(&txn, p.id, target.id).await? {
            return Err(Error::NotFound);
        }
        if pair.state == "CONNECTED" {
            return Err(Error::Pending);
        }
        let old:Option<ContactRequest>=one(&txn,"SELECT to_jsonb(r) doc FROM communication_contact_requests r WHERE connection_id=$1 ORDER BY created_at DESC LIMIT 1 FOR UPDATE",vec![pair.id.into()]).await?;
        if let Some(old) = old
            .as_ref()
            .filter(|o| o.state == "PENDING" && o.expires_at > Utc::now())
        {
            let result = if old.sender_profile_id == p.id {
                request_view(old, self.store.current_public_id(&txn, target.id).await?)
            } else {
                // K2: accept their request; the same checks as accepting it directly.
                if target.activation_state != Activation::Active
                    || !self.active_account(scope, &target).await?
                    || self.domain_blocks(scope, p, &target).await?
                {
                    return Err(Error::NotFound);
                }
                self.accept_request(&txn, p, &target, &pair, old.id, old.version)
                    .await?
            };
            self.store
                .finish_idempotency(&txn, p, "contact-request.create", key, result.clone())
                .await?;
            txn.commit().await?;
            return Ok(result);
        }
        // K4: "who can send me requests: Nobody" still lets in someone holding a valid,
        // unexpired invite (link or QR) from this person: they chose to share it.
        if self
            .store
            .privacy(&txn, target.id)
            .await?
            .contact_request_permissions
            != RequestPermission::Everyone
            && !self.store.invite_claim(&txn, p.id, target.id).await?
        {
            return Err(Error::NotFound);
        }
        if let Some(old) = old {
            if old.state == "PENDING" {
                execute(&txn,"UPDATE communication_contact_requests SET state='EXPIRED',version=version+1,updated_at=now() WHERE id=$1",vec![old.id.into()]).await?;
            }
            // K3: the 24-hour wait applies only to the person whose own request was
            // declined or cancelled, never to the person who declined it.
            let recent:Value=one(&txn,"SELECT jsonb_build_object('cooldown',EXISTS(SELECT 1 FROM communication_contact_requests WHERE connection_id=$1 AND sender_profile_id=$2 AND state IN ('DECLINED','CANCELLED') AND updated_at>now()-interval '24 hours')) doc",vec![pair.id.into(),p.id.into()]).await?.ok_or(Error::Unavailable)?;
            if recent["cooldown"] == true {
                return Err(Error::RateLimited);
            }
        }
        let id = Uuid::new_v4();
        // Direct contact (owner decision 2026-11-19): a person whose contact approval is
        // AUTOMATIC gave their consent by sharing their CF Meet ID, so adding them connects
        // straight away, and chat and calls (CONNECTIONS policies) work at once. The same
        // checks as accepting a request apply (active person and account, no domain block).
        let automatic = self
            .store
            .privacy(&txn, target.id)
            .await?
            .contact_approval
            .unwrap_or(ContactApproval::Automatic)
            == ContactApproval::Automatic
            && target.activation_state == Activation::Active
            && self.active_account(scope, &target).await?
            && !self.domain_blocks(scope, &target, p).await?;
        if automatic {
            execute(&txn,"INSERT INTO communication_contact_requests(id,connection_id,sender_profile_id,recipient_profile_id,state) VALUES($1,$2,$3,$4,'ACCEPTED')",vec![id.into(),pair.id.into(),p.id.into(),target.id.into()]).await?;
            execute(&txn,"UPDATE communication_connections SET state='CONNECTED',connected_at=now(),connected_before_block=false,version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into()]).await?;
            for owner in [p.id, target.id] {
                execute(&txn,"INSERT INTO communication_contact_aliases(id,owner_profile_id,connection_id) VALUES($1,$2,$3) ON CONFLICT(owner_profile_id,connection_id) DO NOTHING",vec![Uuid::new_v4().into(),owner.into(),pair.id.into()]).await?;
            }
            self.store
                .audit(&txn, p, "CONNECTION_ADDED_DIRECTLY", Some(id), json!({}))
                .await?;
            self.store.event(&txn,p,"communications.connection.connected",pair.id,pair.version+1,json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":target.principal_id,"state":"CONNECTED"})).await?;
            // The added person learns they have a new contact ("You are now connected").
            notification_publications::enqueue(
                self,
                &txn,
                Notice {
                    notice: ConnectionNotice::Connected,
                    request_id: id,
                    connection_id: pair.id,
                    connection_version: pair.version + 1,
                    actor: p,
                    recipient: &target,
                },
            )
            .await?;
            self.store
                .relationship_changed(&txn, &[p.id, target.id], pair.id, pair.version + 1)
                .await?;
            let result = json!({"schemaVersion":1,"requestId":id,"cfMeetId":self.store.current_public_id(&txn,target.id).await?,"state":"ACCEPTED","version":1,"relationship":"CONNECTED"});
            self.store
                .finish_idempotency(&txn, p, "contact-request.create", key, result.clone())
                .await?;
            txn.commit().await?;
            return Ok(result);
        }
        execute(&txn,"INSERT INTO communication_contact_requests(id,connection_id,sender_profile_id,recipient_profile_id) VALUES($1,$2,$3,$4)",vec![id.into(),pair.id.into(),p.id.into(),target.id.into()]).await?;
        execute(&txn,"UPDATE communication_connections SET state='REQUEST_PENDING',version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into()]).await?;
        self.store
            .audit(&txn, p, "CONNECTION_REQUESTED", Some(id), json!({}))
            .await?;
        self.store.event(&txn,p,"communications.connection.requested",pair.id,pair.version+1,json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":target.principal_id,"state":"REQUEST_PENDING"})).await?;
        // Roadmap N7: the addressee's notification, in this transaction (only with
        // CF_NOTIFICATIONS_BASE_URL; blocked / privacy-refused requests never get here).
        notification_publications::enqueue(
            self,
            &txn,
            Notice {
                notice: ConnectionNotice::Requested,
                request_id: id,
                connection_id: pair.id,
                connection_version: pair.version + 1,
                actor: p,
                recipient: &target,
            },
        )
        .await?;
        self.store
            .relationship_changed(&txn, &[p.id, target.id], pair.id, pair.version + 1)
            .await?;
        let result = json!({"schemaVersion":1,"requestId":id,"cfMeetId":self.store.current_public_id(&txn,target.id).await?,"state":"PENDING","version":1,"relationship":"REQUEST_PENDING"});
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
        if action == "accept" {
            let result = self
                .accept_request(&txn, p, &other, &pair, id, r.version)
                .await?;
            self.store
                .finish_idempotency(&txn, p, &operation, key, result.clone())
                .await?;
            txn.commit().await?;
            return Ok(result);
        }
        execute(&txn,"UPDATE communication_contact_requests SET state=$2,version=version+1,updated_at=now() WHERE id=$1",vec![id.into(),next.into()]).await?;
        let relationship = "NONE";
        execute(&txn,"UPDATE communication_connections SET state=$2,version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into(),relationship.into()]).await?;
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
    /// Accept `request` (sent by `other` to `p`) under the pair lock the caller holds:
    /// both people become connected. Used by accepting a request and by K2 (adding back
    /// someone whose request to you is pending). The caller made the accept checks.
    async fn accept_request(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        p: &Profile,
        other: &Profile,
        pair: &Connection,
        request: Uuid,
        request_version: i64,
    ) -> Result<Value> {
        execute(txn,"UPDATE communication_contact_requests SET state='ACCEPTED',version=version+1,updated_at=now() WHERE id=$1",vec![request.into()]).await?;
        execute(txn,"UPDATE communication_connections SET state='CONNECTED',connected_at=now(),connected_before_block=false,version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into()]).await?;
        for owner in [p.id, other.id] {
            execute(txn,"INSERT INTO communication_contact_aliases(id,owner_profile_id,connection_id) VALUES($1,$2,$3) ON CONFLICT(owner_profile_id,connection_id) DO NOTHING",vec![Uuid::new_v4().into(),owner.into(),pair.id.into()]).await?;
        }
        self.store
            .audit(
                txn,
                p,
                "CONNECTION_REQUEST_ACCEPTED",
                Some(request),
                json!({}),
            )
            .await?;
        self.store.event(txn,p,"communications.connection.connected",pair.id,pair.version+1,json!({"actorPrincipalId":p.principal_id,"targetPrincipalId":other.principal_id,"state":"CONNECTED"})).await?;
        // Roadmap N7: the requester learns the addressee accepted (only with
        // CF_NOTIFICATIONS_BASE_URL; a blocked pair was refused by the caller).
        notification_publications::enqueue(
            self,
            txn,
            Notice {
                notice: ConnectionNotice::Connected,
                request_id: request,
                connection_id: pair.id,
                connection_version: pair.version + 1,
                actor: p,
                recipient: other,
            },
        )
        .await?;
        self.store
            .relationship_changed(txn, &[p.id, other.id], pair.id, pair.version + 1)
            .await?;
        Ok(
            json!({"schemaVersion":1,"requestId":request,"cfMeetId":self.store.current_public_id(txn,other.id).await?,"state":"ACCEPTED","version":request_version+1,"relationship":"CONNECTED"}),
        )
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
        // One query for the page. Requests are already scoped by the owner's immutable
        // profile; the other person's current CF Meet ID and a live block (either way,
        // which hides the request immediately, even before event propagation) come joined.
        let other = if incoming {
            "r.sender_profile_id"
        } else {
            "r.recipient_profile_id"
        };
        let requests:Vec<Value>=rows(&self.store.db,&format!("SELECT jsonb_build_object('request',to_jsonb(r),'cfMeetId',i.public_id,'blocked',EXISTS(SELECT 1 FROM communication_block_bridge b WHERE b.active AND ((b.blocker_profile_id=$1 AND b.blocked_profile_id={other}) OR (b.blocker_profile_id={other} AND b.blocked_profile_id=$1)))) doc FROM communication_contact_requests r LEFT JOIN communication_public_ids i ON i.profile_id={other} AND i.current WHERE r.{column}=$1 AND r.id>$2 ORDER BY r.id LIMIT $3"),vec![p.id.into(),cursor.into(),(limit+1).into()]).await?;
        let more = requests.len() > limit as usize;
        let mut data = Vec::new();
        let mut next = None;
        for row in requests.into_iter().take(limit as usize) {
            let request: ContactRequest =
                serde_json::from_value(row["request"].clone()).map_err(|_| Error::Unavailable)?;
            next = Some(request.id);
            if row["blocked"] == true {
                continue;
            }
            let cf_id = row["cfMeetId"].as_str().ok_or(Error::NotFound)?.to_owned();
            data.push(request_view(&request, cf_id));
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
        self.contact_views(p, "a.id=$5", "", vec![id.into()], false)
            .await?
            .into_iter()
            .next()
            .ok_or(Error::NotFound)
    }
    /// K1: contact views of `p` in ONE joined query (alias, connection, the other person's
    /// profile, their current CF Meet ID and a live block either way), never several reads
    /// per contact. `filter` is ANDed to the owner condition and may use `$5`, `$6`…
    /// (`extra`); `tail` is the ORDER BY / LIMIT. `$1` is the owner, `$2..$4` its scope.
    /// `with_rank` adds `favoriteRank` (the favorites list).
    pub(crate) async fn contact_views(
        &self,
        p: &Profile,
        filter: &str,
        tail: &str,
        extra: Vec<sea_orm::Value>,
        with_rank: bool,
    ) -> Result<Vec<Value>> {
        let mut values = vec![
            p.id.into(),
            p.tenant_id.into(),
            p.application_id.into(),
            p.environment_id.into(),
        ];
        values.extend(extra);
        let sql = format!(
            "SELECT jsonb_build_object('alias',to_jsonb(a),'state',c.state,'relationshipVersion',c.version,\
             'cfMeetId',i.public_id,'displayName',o.display_name,'avatarReference',o.avatar_reference,\
             'principalId',o.principal_id,'about',o.about,\
             'blocked',EXISTS(SELECT 1 FROM communication_block_bridge b WHERE b.active \
                AND ((b.blocker_profile_id=$1 AND b.blocked_profile_id=o.id) OR (b.blocker_profile_id=o.id AND b.blocked_profile_id=$1)))) doc \
             FROM communication_contact_aliases a \
             JOIN communication_connections c ON c.id=a.connection_id AND (c.left_profile_id=$1 OR c.right_profile_id=$1) \
             JOIN communication_profiles o ON o.id=CASE WHEN c.left_profile_id=$1 THEN c.right_profile_id ELSE c.left_profile_id END \
                AND o.tenant_id=$2 AND o.application_id=$3 AND o.environment_id=$4 \
             JOIN communication_public_ids i ON i.profile_id=o.id AND i.current \
             WHERE a.owner_profile_id=$1 AND {filter} {tail}"
        );
        let found: Vec<Value> = rows(&self.store.db, &sql, values).await?;
        found
            .iter()
            .map(|r| {
                let mut view = contact_row_view(r)?;
                if with_rank {
                    view["favoriteRank"] = r["alias"]["favorite_rank"].clone();
                }
                Ok(view)
            })
            .collect()
    }
    pub async fn list_contacts(&self, p: &Profile, limit: i64, cursor: Uuid) -> Result<Value> {
        let mut data = self
            .contact_views(
                p,
                "a.id>$5 AND c.state='CONNECTED'",
                "ORDER BY a.id LIMIT $6",
                vec![cursor.into(), (limit + 1).into()],
                false,
            )
            .await?;
        let more = data.len() > limit as usize;
        data.truncate(limit as usize);
        let last = data.last().map(|v| v["contactId"].clone());
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
            || update.notifications.as_ref().is_some_and(|n| {
                // K11: a contact's level governs private (one-to-one, end-to-end
                // encrypted) chats, where mentions cannot be read; offered again once
                // they can (crate::policy::MENTIONS_IN_PRIVATE_CHATS).
                !(matches!(n.as_str(), "ALL" | "MUTED")
                    || (n == "MENTIONS" && crate::policy::MENTIONS_IN_PRIVATE_CHATS))
            })
        {
            return Err(Error::Validation);
        }
        let (old, pair, other) = self.contact(p, id).await?;
        let txn = self.store.begin().await?;
        // Roadmap N10: the contact notification level moves to the Notification Service.
        let authority = if update.notifications.is_some() {
            self.preference_authority(&txn, true).await?
        } else {
            crate::preference_authority::Authority::Hub
        };
        if authority == crate::preference_authority::Authority::Freeze {
            return Err(Error::PreferencesFrozen);
        }
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
        if authority == crate::preference_authority::Authority::Service {
            // Forwarded to the single preference writer; the Hub row (alias, favorite and
            // the mirrored level) commits only once the service accepted the level.
            self.service_update_entry(
                p,
                json!({"contacts":{other.account_reference.clone():{"notifications":notifications}}}),
                key,
            )
            .await?;
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
        } else if pair.state == "BLOCKED" && pair.connected_before_block {
            // K5: removed while blocked (by either person): unblocking no longer brings
            // the contact back.
            execute(&txn,"UPDATE communication_connections SET connected_before_block=false,revoked_at=now(),version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into()]).await?;
            self.store
                .audit(
                    &txn,
                    p,
                    "CONNECTION_REMOVED",
                    Some(pair.id),
                    json!({"blocked":true}),
                )
                .await?;
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
        // K5: blocking remembers whether the pair was connected; unblocking (once no block
        // is left either way) restores the contact, unless someone removed it meanwhile.
        let (state, remembered) = if active {
            let remembered = match pair.state.as_str() {
                "CONNECTED" => true,
                "BLOCKED" => pair.connected_before_block,
                _ => false,
            };
            ("BLOCKED", remembered)
        } else if self.store.blocked(&txn, p.id, target.id).await? {
            ("BLOCKED", pair.connected_before_block)
        } else if pair.state == "BLOCKED" && pair.connected_before_block {
            ("CONNECTED", false)
        } else if pair.state == "BLOCKED" {
            ("REVOKED", false)
        } else {
            // Nothing of ours was blocking: the relationship stays as it is.
            (pair.state.as_str(), pair.connected_before_block)
        };
        execute(&txn,"UPDATE communication_connections SET state=$2,connected_before_block=$3,version=version+1,updated_at=now() WHERE id=$1",vec![pair.id.into(),state.into(),remembered.into()]).await?;
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
        // refresh the relationship (no block state is disclosed to them). After an unblock
        // the blocker's contact list may change too (K5). Entries are written in profile
        // order (K8: feed counters are locked in insertion order at commit).
        let mut entries = vec![(target.id, "CONTACT"), (p.id, "BLOCK")];
        if !active {
            entries.push((p.id, "CONTACT"));
        }
        entries.sort_by_key(|(profile, _)| *profile);
        for (profile, domain) in entries {
            self.store
                .change(&txn, profile, domain, Some(pair.id), Some(pair.version + 1))
                .await?;
        }
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
            safe.push(common_group_view(g)?);
        }
        Ok(json!({"schemaVersion":1,"groups":safe}))
    }
}

/// M2: one group in common, as Messaging (the owner) reported it for the viewer — only
/// groups BOTH principals are current members of. The display metadata is what Messaging
/// legitimately holds: the title / avatar reference of a MANAGED group (null for a
/// PRIVATE_E2EE group, whose name stays inside its encrypted group state) and the current
/// member count. Values outside the owner's vocabulary or bounds are dropped (an
/// unexpected profile fails the whole read), never invented.
pub fn common_group_view(g: &Value) -> Result<Value> {
    let id = uuid_field(g, "conversationId")?;
    if g["contextClass"] != "GROUP" {
        return Err(Error::Unavailable);
    }
    // An older Messaging image reports ids only: keep the 1.x shape (metadata null).
    let profile = match g.get("securityProfile") {
        None | Some(Value::Null) => None,
        Some(p) => Some(
            p.as_str()
                .filter(|p| ["MANAGED", "PRIVATE_E2EE"].contains(p))
                .ok_or(Error::Unavailable)?,
        ),
    };
    let managed = profile == Some("MANAGED");
    let text = |key: &str, max: usize| {
        g[key]
            .as_str()
            .map(str::trim)
            .filter(|t| managed && !t.is_empty() && t.chars().count() <= max)
            .filter(|t| !t.chars().any(char::is_control))
            .map(str::to_string)
    };
    Ok(
        json!({"conversationId":id,"contextClass":"GROUP","securityProfile":profile,
        "title":text("title", 120),"avatarRef":text("avatarRef", 256),
        "memberCount":g["memberCount"].as_i64().filter(|n| *n >= 0)}),
    )
}
/// One contact view from a [`State::contact_views`] row.
fn contact_row_view(r: &Value) -> Result<Value> {
    let alias = &r["alias"];
    let state = if r["blocked"] == true {
        "BLOCKED"
    } else {
        r["state"].as_str().ok_or(Error::Unavailable)?
    };
    let connected = state == "CONNECTED";
    // The contact's calling principal, only while CONNECTED: lets a client match the
    // contact to a call participant (for example to hide people already in a call from
    // "Add people"). Never for blocked or ended relationships. The same for the "About"
    // line (KF2): contacts see it, nobody else.
    let only_connected = |v: &Value| if connected { v.clone() } else { Value::Null };
    Ok(
        json!({"schemaVersion":1,"contactId":alias["id"],"cfMeetId":r["cfMeetId"],"displayName":r["displayName"],"avatarReference":r["avatarReference"],
        "relationship":state,"relationshipVersion":r["relationshipVersion"],"alias":alias["alias"],"favorite":alias["favorite"],"notifications":alias["notifications"],"version":alias["version"],
        "principalId":only_connected(&r["principalId"]),"about":only_connected(&r["about"])}),
    )
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

//! Section 12.1–12.3 / N01–N07: a reference registry over canonical Messaging devices
//! and Calling endpoints. Each link is verified with its owner for the same principal;
//! guessing another user's id gives 404. No device keys or push tokens are stored here.
use crate::{
    error::{Error, Result},
    model::*,
    relationships::uuid_field,
    store::{one, rows},
    State,
};
use cf_service_auth::DelegatedPlatformContext;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub const MAX_DEVICES: i64 = 20;

fn device_view(d: &Value) -> Value {
    json!({"deviceId":d["id"],"platform":d["platform"],"label":d["label"],"state":d["state"],
        "chatDeviceId":d["chat_device_ref"],"callEndpointId":d["call_endpoint_ref"],
        "push":{"registered":d["push_registered_at"].is_string(),"platform":d["push_platform"],"registeredAt":d["push_registered_at"]},
        "acknowledgedSyncPosition":d["acknowledged_sync_position"],"revokedAt":d["revoked_at"],"revocationReason":d["revocation_reason"],
        "version":d["version"],"createdAt":d["created_at"]})
}

impl State {
    async fn owned_device<C: sea_orm::ConnectionTrait>(
        &self,
        db: &C,
        p: &Profile,
        id: Uuid,
        lock: bool,
    ) -> Result<Value> {
        one(db,&format!("SELECT to_jsonb(d) doc FROM communication_devices d WHERE id=$1 AND profile_id=$2{}",if lock {" FOR UPDATE"} else {""}),vec![id.into(),p.id.into()]).await?.ok_or(Error::NotFound)
    }
    pub async fn list_devices(&self, p: &Profile) -> Result<Value> {
        let devices:Vec<Value>=rows(&self.store.db,"SELECT to_jsonb(d) doc FROM communication_devices d WHERE profile_id=$1 ORDER BY created_at,id LIMIT 100",vec![p.id.into()]).await?;
        Ok(
            json!({"schemaVersion":1,"data":devices.iter().map(device_view).collect::<Vec<_>>(),"limit":MAX_DEVICES}),
        )
    }

    /// N02: link an existing canonical device and/or endpoint owned by the caller.
    pub async fn link_device(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Link {
            platform: String,
            label: Option<String>,
            chat_device_id: Option<Uuid>,
            call_endpoint_id: Option<Uuid>,
        }
        let input: Link = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if !matches!(
            input.platform.as_str(),
            "IOS" | "ANDROID" | "WEB" | "DESKTOP"
        ) || (input.chat_device_id.is_none() && input.call_endpoint_id.is_none())
            || input
                .label
                .as_deref()
                .is_some_and(|l| l.chars().count() > 80 || l.chars().any(char::is_control))
        {
            return Err(Error::Validation);
        }
        if p.activation_state != Activation::Active {
            return Err(Error::NotActivated);
        }
        // Ownership proof comes from the canonical owners, under the caller's principal.
        let mut push = (None::<String>, false);
        if let Some(d) = input.chat_device_id {
            let v = self
                .messaging(scope)?
                .call(
                    "/internal/v1/communications/devices/verify",
                    &json!({"principalId":p.principal_id,"deviceId":d}),
                )
                .await?;
            if v["owned"] != true || v["status"] != "ACTIVE" {
                return Err(Error::NotFound);
            }
            push = (
                v["pushPlatform"].as_str().map(str::to_owned),
                v["pushRegistered"] == true,
            );
        }
        if let Some(e) = input.call_endpoint_id {
            let v = self
                .calling(scope)?
                .call(
                    "/internal/v1/communications/endpoints/verify",
                    &json!({"principalId":p.principal_id,"endpointId":e}),
                )
                .await?;
            if v["owned"] != true || v["status"] != "ACTIVE" {
                return Err(Error::NotFound);
            }
        }
        let txn = self.store.begin().await?;
        let op = "device.link";
        if let Some(v) = self.store.idempotency(&txn, p, op, key, &body).await? {
            txn.commit().await?;
            return Ok(v);
        }
        self.store.profile(&txn, p.scope(), p.id, true).await?;
        let existing:Option<Value>=one(&txn,"SELECT to_jsonb(d) doc FROM communication_devices d WHERE (chat_device_ref=$1 AND $1 IS NOT NULL) OR (call_endpoint_ref=$2 AND $2 IS NOT NULL) LIMIT 1",vec![input.chat_device_id.into(),input.call_endpoint_id.into()]).await?;
        if let Some(d) = existing {
            // Owners already proved ownership, so a row for another profile cannot exist
            // for a live reference; never reveal or reuse it.
            if uuid_field(&d, "profile_id")? != p.id || d["state"] != "ACTIVE" {
                return Err(Error::NotFound);
            }
            let result = json!({"schemaVersion":1,"device":device_view(&d),"linked":false});
            self.store
                .finish_idempotency(&txn, p, op, key, result.clone())
                .await?;
            txn.commit().await?;
            return Ok(result);
        }
        let count:Value=one(&txn,"SELECT jsonb_build_object('n',count(*)) doc FROM communication_devices WHERE profile_id=$1 AND state='ACTIVE'",vec![p.id.into()]).await?.ok_or(Error::Unavailable)?;
        if count["n"].as_i64().unwrap_or(MAX_DEVICES) >= MAX_DEVICES {
            return Err(Error::PolicyValidation);
        }
        let id = Uuid::new_v4();
        let d:Value=one(&txn,"INSERT INTO communication_devices AS d (id,profile_id,platform,label,chat_device_ref,call_endpoint_ref,push_platform,push_registered_at) VALUES($1,$2,$3,$4,$5,$6,$7,CASE WHEN $8 THEN now() END) RETURNING to_jsonb(d) doc",
            vec![id.into(),p.id.into(),input.platform.into(),input.label.into(),input.chat_device_id.into(),input.call_endpoint_id.into(),push.0.filter(|_| push.1).into(),push.1.into()]).await?.ok_or(Error::Unavailable)?;
        self.store
            .audit(&txn, p, "DEVICE_LINKED", Some(id), json!({}))
            .await?;
        self.store
            .change(&txn, p.id, "DEVICE", Some(id), Some(1))
            .await?;
        let result = json!({"schemaVersion":1,"device":device_view(&d),"linked":true});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    /// N03/N04: register, rotate or remove the push target through Messaging's adapters.
    pub async fn device_push(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        id: Uuid,
        registration: Option<Value>,
        key: Uuid,
    ) -> Result<Value> {
        let register = registration.is_some();
        if registration.as_ref().is_some_and(|r| !r.is_object()) {
            return Err(Error::Validation);
        }
        let d = self.owned_device(&self.store.db, p, id, false).await?;
        if d["state"] != "ACTIVE" {
            return Err(Error::NotFound);
        }
        let chat = d["chat_device_ref"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(Error::ActionNotAllowed)?;
        let mut request = json!({"principalId":p.principal_id,"deviceId":chat,"action":if register {"REGISTER"} else {"REMOVE"}});
        if let Some(r) = registration {
            request["registration"] = r;
        }
        // The push token passes through to Messaging and is never persisted or echoed here.
        let v = self
            .messaging(scope)?
            .call("/internal/v1/communications/devices/push", &request)
            .await?;
        let platform = v["platform"].as_str().map(str::to_owned);
        let txn = self.store.begin().await?;
        let op = if register {
            "device.push.put"
        } else {
            "device.push.delete"
        };
        if let Some(done) = self
            .store
            .idempotency(
                &txn,
                p,
                op,
                key,
                &json!({"deviceId":id,"register":register}),
            )
            .await?
        {
            txn.commit().await?;
            return Ok(done);
        }
        let row:Value=one(&txn,"UPDATE communication_devices d SET push_platform=$3,push_registered_at=CASE WHEN $4 THEN now() END,version=d.version+1,updated_at=now() WHERE id=$1 AND profile_id=$2 AND state='ACTIVE' RETURNING to_jsonb(d) doc",
            vec![id.into(),p.id.into(),platform.filter(|_| v["pushRegistered"]==true).into(),(v["pushRegistered"]==true).into()]).await?.ok_or(Error::NotFound)?;
        self.store
            .change(&txn, p.id, "DEVICE", Some(id), row["version"].as_i64())
            .await?;
        let result = json!({"schemaVersion":1,"device":device_view(&row)});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    /// N05: revoke at every canonical owner first, then mark the reference revoked.
    pub async fn revoke_device(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        id: Uuid,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Revoke {
            reason: String,
        }
        let input: Revoke = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if !matches!(input.reason.as_str(), "USER_REQUESTED" | "COMPROMISED") {
            return Err(Error::Validation);
        }
        let d = self.owned_device(&self.store.db, p, id, false).await?;
        if d["state"] == "REVOKED" {
            return Ok(json!({"schemaVersion":1,"device":device_view(&d),"alreadyRevoked":true}));
        }
        // Each owner call is idempotent, so a partial failure is completed by a retry.
        if let Some(chat) = d["chat_device_ref"].as_str() {
            self.messaging(scope)?
                .call(
                    "/internal/v1/communications/devices/revoke",
                    &json!({"principalId":p.principal_id,"deviceId":chat,"reason":input.reason}),
                )
                .await?;
        }
        if let Some(endpoint) = d["call_endpoint_ref"].as_str() {
            self.calling(scope)?
                .call(
                    "/internal/v1/communications/endpoints/revoke",
                    &json!({"principalId":p.principal_id,"endpointId":endpoint}),
                )
                .await?;
        }
        let txn = self.store.begin().await?;
        let op = "device.revoke";
        if let Some(v) = self
            .store
            .idempotency(&txn, p, op, key, &json!({"deviceId":id,"body":body}))
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let row:Value=one(&txn,"UPDATE communication_devices d SET state='REVOKED',revoked_at=now(),revocation_reason=$3,push_platform=NULL,push_registered_at=NULL,version=d.version+1,updated_at=now() WHERE id=$1 AND profile_id=$2 AND state='ACTIVE' RETURNING to_jsonb(d) doc",
            vec![id.into(),p.id.into(),input.reason.clone().into()]).await?.ok_or(Error::Version)?;
        self.store
            .audit(
                &txn,
                p,
                "DEVICE_REVOKED",
                Some(id),
                json!({"reason":input.reason}),
            )
            .await?;
        self.store
            .change(&txn, p.id, "DEVICE", Some(id), row["version"].as_i64())
            .await?;
        let result = json!({"schemaVersion":1,"device":device_view(&row),"alreadyRevoked":false,
            "effects":{"messagingDevice":row["chat_device_ref"].is_string(),"callingEndpoint":row["call_endpoint_ref"].is_string(),
                "accountSessions":"Use Account logout or revoke-all to end other sessions"}});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    /// N06 (read) and its acknowledgement companion (monotonic, never past the head).
    pub async fn device_sync_state(
        &self,
        p: &Profile,
        id: Uuid,
        ack: Option<(Value, Uuid)>,
    ) -> Result<Value> {
        let d = self.owned_device(&self.store.db, p, id, false).await?;
        let head = self.sync_head(p).await?;
        let mut position = d["acknowledged_sync_position"].as_i64().unwrap_or(0);
        if let Some((body, key)) = ack {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Ack {
                acknowledged_position: i64,
            }
            let input: Ack = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
            if input.acknowledged_position < 0
                || input.acknowledged_position > head.0
                || d["state"] != "ACTIVE"
            {
                return Err(Error::Validation);
            }
            let txn = self.store.begin().await?;
            if let Some(v) = self
                .store
                .idempotency(
                    &txn,
                    p,
                    "device.sync.ack",
                    key,
                    &json!({"deviceId":id,"body":body}),
                )
                .await?
            {
                txn.commit().await?;
                return Ok(v);
            }
            let row:Value=one(&txn,"UPDATE communication_devices d SET acknowledged_sync_position=GREATEST(d.acknowledged_sync_position,$3),version=d.version+1,updated_at=now() WHERE id=$1 AND profile_id=$2 RETURNING to_jsonb(d) doc",
                vec![id.into(),p.id.into(),input.acknowledged_position.into()]).await?.ok_or(Error::NotFound)?;
            position = row["acknowledged_sync_position"]
                .as_i64()
                .unwrap_or(position);
            let result = sync_state_view(id, position, head);
            self.store
                .finish_idempotency(&txn, p, "device.sync.ack", key, result.clone())
                .await?;
            txn.commit().await?;
            return Ok(result);
        }
        Ok(sync_state_view(id, position, head))
    }

    /// N07: truthful provider and per-device delivery state; NOT_CONFIGURED is never
    /// reported as delivered.
    pub async fn delivery_status(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
    ) -> Result<Value> {
        let v = self
            .messaging(scope)?
            .call(
                "/internal/v1/communications/push-status",
                &json!({"principalId":p.principal_id}),
            )
            .await?;
        let prefs = self.get_notification_preferences(p).await?;
        Ok(
            json!({"schemaVersion":1,"providers":v["pushServices"],"devices":v["devices"],"pendingJobs":v["pendingJobs"],"deadJobs":v["deadJobs"],
            "callRinging":{"owner":"CALLING","transport":"Calling signaling WebSocket; mobile push transport NOT_CONFIGURED"},
            "preferences":prefs,"physicalDeviceAcceptance":"NOT_RUN"}),
        )
    }
}
fn sync_state_view(id: Uuid, position: i64, head: (i64, i64)) -> Value {
    json!({"schemaVersion":1,"deviceId":id,"acknowledgedPosition":position,"latestPosition":head.0,"oldestRetainedPosition":head.1,
        "resyncRequired":position>0 && position+1<head.1})
}

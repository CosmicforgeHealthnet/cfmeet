use crate::{
    crypto,
    error::{Error, Result},
    model::*,
    store::{execute, one},
    State,
};
use cf_service_auth::DelegatedPlatformContext;
use chrono::{Duration, Utc};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateInvite {
    pub purpose: String,
    pub expires_in_seconds: i64,
    pub max_uses: i32,
}
impl CreateInvite {
    pub fn validate(&self) -> Result<()> {
        if self.purpose != "CONNECT"
            || !(60..=604800).contains(&self.expires_in_seconds)
            || !(1..=10).contains(&self.max_uses)
        {
            Err(Error::Validation)
        } else {
            Ok(())
        }
    }
}
impl State {
    pub async fn create_invite(
        &self,
        p: &Profile,
        body: Value,
        key: Uuid,
        qr: bool,
    ) -> Result<Value> {
        let request: CreateInvite =
            serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        request.validate()?;
        let txn = self.store.begin().await?;
        // Serialize token issuance with owner privacy changes and public-ID rotation.
        // Otherwise a token inserted after their revocation sweep could survive it.
        self.store.profile(&txn, p.scope(), p.id, true).await?;
        let operation = if qr { "qr.create" } else { "invites.create" };
        if let Some(v) = self
            .store
            .idempotency(&txn, p, operation, key, &body)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        if self.store.privacy(&txn, p.id).await?.invitation_permissions
            != InvitationPermission::Enabled
        {
            return Err(Error::Forbidden);
        }
        self.store.rate(&txn, p.id, "invites.create", 30).await?;
        let token = crypto::invite_token();
        let digest =
            crypto::protected_digest(self.config.invite_key.expose_secret().as_bytes(), &token);
        let id = Uuid::new_v4();
        let expires = Utc::now() + Duration::seconds(request.expires_in_seconds);
        execute(&txn,"INSERT INTO communication_invites(id,owner_profile_id,token_digest,purpose,expires_at,max_uses) VALUES($1,$2,$3,'CONNECT',$4,$5)",vec![id.into(),p.id.into(),digest.into(),expires.into(),request.max_uses.into()]).await?;
        self.store
            .audit(
                &txn,
                p,
                "INVITATION_CREATED",
                Some(id),
                json!({"purpose":"CONNECT","maxUses":request.max_uses}),
            )
            .await?;
        let mut result = json!({"schemaVersion":1,"inviteId":id,"purpose":"CONNECT","expiresAt":expires,"maxUses":request.max_uses,"uses":0,"version":1,"secretReturned":false});
        // Durable idempotency stores metadata only. The token is disclosed once; listing
        // and replay never reveal it. Request a new invite to obtain another QR payload.
        self.store
            .finish_idempotency(&txn, p, operation, key, result.clone())
            .await?;
        txn.commit().await?;
        result["secretReturned"] = json!(true);
        result["payload"] = json!(format!("{}/connect/{}", self.config.public_app_url, token));
        Ok(result)
    }
    pub async fn list_invites(&self, p: &Profile, limit: i64, cursor: Uuid) -> Result<Value> {
        let values:Vec<Invite>=crate::store::rows(&self.store.db,"SELECT to_jsonb(i)-'token_digest' doc FROM communication_invites i WHERE owner_profile_id=$1 AND id>$2 ORDER BY id LIMIT $3",vec![p.id.into(),cursor.into(),(limit+1).into()]).await?;
        let more = values.len() > limit as usize;
        let values: Vec<_> = values.into_iter().take(limit as usize).collect();
        Ok(
            json!({"schemaVersion":1,"data":values.iter().map(|v|json!({"inviteId":v.id,"purpose":v.purpose,"expiresAt":v.expires_at,"revoked":v.revoked_at.is_some(),"maxUses":v.max_uses,"uses":v.uses,"version":v.version})).collect::<Vec<_>>(),"nextCursor":if more {values.last().map(|v|v.id)} else {None}}),
        )
    }
    pub async fn revoke_invite(&self, p: &Profile, id: Uuid, key: Uuid) -> Result<Value> {
        let txn = self.store.begin().await?;
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "invites.revoke", key, &json!({"id":id}))
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let invite:Invite=one(&txn,"SELECT to_jsonb(i)-'token_digest' doc FROM communication_invites i WHERE id=$1 AND owner_profile_id=$2 FOR UPDATE",vec![id.into(),p.id.into()]).await?.ok_or(Error::NotFound)?;
        if invite.revoked_at.is_none() {
            execute(
                &txn,
                "UPDATE communication_invites SET revoked_at=now(),version=version+1 WHERE id=$1",
                vec![id.into()],
            )
            .await?;
            self.store
                .audit(&txn, p, "INVITATION_REVOKED", Some(id), json!({}))
                .await?;
        }
        let result = json!({"schemaVersion":1,"inviteId":id,"revoked":true});
        self.store
            .finish_idempotency(&txn, p, "invites.revoke", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn resolve_invite(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Resolve {
            token: String,
            purpose: String,
        }
        let request: Resolve =
            serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if request.purpose != "CONNECT"
            || request.token.len() != 64
            || !request.token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::NotFound);
        }
        let digest = crypto::protected_digest(
            self.config.invite_key.expose_secret().as_bytes(),
            &request.token,
        );
        self.limit_attempt(p, "discovery.resolve", 120).await?;
        let txn = self.store.begin().await?;
        self.store
            .idempotency(&txn, p, "invites.resolve", key, &body)
            .await?;
        let invite:Invite=one(&txn,"SELECT to_jsonb(i)-'token_digest' doc FROM communication_invites i WHERE token_digest=$1 FOR UPDATE",vec![digest.into()]).await?.ok_or(Error::NotFound)?;
        if invite.revoked_at.is_some()
            || invite.expires_at <= Utc::now()
            || invite.purpose != "CONNECT"
            || invite.owner_profile_id == p.id
        {
            return Err(Error::NotFound);
        }
        let target = self
            .store
            .profile(&txn, p.scope(), invite.owner_profile_id, false)
            .await?;
        if self
            .store
            .privacy(&txn, target.id)
            .await?
            .invitation_permissions
            != InvitationPermission::Enabled
        {
            return Err(Error::NotFound);
        }
        let old:Option<Value>=one(&txn,"SELECT jsonb_build_object('key',idempotency_key) doc FROM communication_invite_claims WHERE invite_id=$1 AND requester_profile_id=$2",vec![invite.id.into(),p.id.into()]).await?;
        if let Some(v) = old {
            if v["key"] != key.to_string() {
                return Err(Error::NotFound);
            }
        } else {
            if invite.uses >= invite.max_uses {
                return Err(Error::NotFound);
            }
            execute(&txn,"INSERT INTO communication_invite_claims(invite_id,requester_profile_id,idempotency_key) VALUES($1,$2,$3)",vec![invite.id.into(),p.id.into(),key.into()]).await?;
            execute(
                &txn,
                "UPDATE communication_invites SET uses=uses+1,version=version+1 WHERE id=$1",
                vec![invite.id.into()],
            )
            .await?;
        }
        let result = self.visible_profile(scope, p, &target, true).await?;
        self.store
            .finish_idempotency(&txn, p, "invites.resolve", key, json!({"resolved":true}))
            .await?;
        self.store
            .audit(
                &txn,
                p,
                "INVITATION_RESOLVED",
                Some(invite.id),
                json!({"connectionGranted":false}),
            )
            .await?;
        txn.commit().await?;
        Ok(result)
    }
}

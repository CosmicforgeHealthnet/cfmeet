use crate::{
    auth, crypto,
    error::{Error, Result},
    model::*,
    store::{self, execute},
    State,
};
use cf_communications_policy::Scope;
use cf_service_auth::DelegatedPlatformContext;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

impl State {
    pub async fn ensure_profile(
        &self,
        scope: &DelegatedPlatformContext,
        a: &VerifiedAccount,
    ) -> Result<Profile> {
        auth::validate_scope(self, scope).await?;
        auth::verify_principal(self, scope, a).await?;
        let status = auth::account_status(self, scope, &a.account_reference).await?;
        if status["active"] != true || status["principalId"] != a.principal_id.to_string() {
            return Err(Error::Forbidden);
        }
        let account_version = status["version"]
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or(Error::Unavailable)?;
        let account_cutoff = status["credentialsNotBefore"]
            .as_str()
            .map(|s| chrono::DateTime::parse_from_rfc3339(s).map(|t| t.with_timezone(&Utc)))
            .transpose()
            .map_err(|_| Error::Unavailable)?;
        let display = a.display_name.trim();
        if display.is_empty()
            || display.chars().count() > 160
            || display.chars().any(char::is_control)
        {
            return Err(Error::Validation);
        }
        let avatar = a
            .avatar_reference
            .as_deref()
            .filter(|v| {
                v.len() <= 512 && v.starts_with("https://") && !v.contains(['@', '\r', '\n'])
            })
            .map(str::to_owned);
        let s = Scope::of(scope);
        let txn = self.store.begin().await?;
        // Both the account reference and principal binding are unique. Concurrent
        // inserts can hit either index first; handle all uniqueness conflicts, then
        // validate the winner's immutable account/principal mapping below.
        let mut v = vec![Uuid::new_v4().into()];
        v.extend(store::scoped(s));
        v.extend([
            a.account_reference.clone().into(),
            a.principal_id.into(),
            display.into(),
            avatar.clone().into(),
        ]);
        let created=execute(&txn,"INSERT INTO communication_profiles(id,tenant_id,application_id,environment_id,account_reference,principal_id,display_name,avatar_reference) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING",v).await?;
        let p = self.store.principal(&txn, s, a.principal_id).await?;
        let p = self.store.profile(&txn, s, p.id, true).await?;
        if p.account_reference != a.account_reference
            || p.activation_state == Activation::Deactivated
        {
            return Err(Error::Forbidden);
        }
        let cutoff = p
            .credentials_not_before
            .into_iter()
            .chain(account_cutoff)
            .max();
        if let Some(cutoff) = cutoff {
            if a.authenticated_at.is_none_or(|at| at <= cutoff.timestamp()) {
                return Err(Error::RecentAuthentication);
            }
        }
        if created > 0 {
            execute(
                &txn,
                "INSERT INTO communication_privacy(profile_id) VALUES($1)",
                vec![p.id.into()],
            )
            .await?;
            self.new_public_id(&txn, p.id).await?;
            self.store
                .audit(
                    &txn,
                    &p,
                    "PROFILE_CREATED",
                    Some(p.id),
                    json!({"phoneRequired":false}),
                )
                .await?;
        }
        if p.activation_state != Activation::Active
            || p.display_name != display
            || p.avatar_reference != avatar
            || p.account_version != account_version
        {
            execute(&txn,"UPDATE communication_profiles SET activation_state='ACTIVE',display_name=$2,avatar_reference=$3,suspended_at=NULL,account_version=$4,credentials_not_before=$5,version=version+1,updated_at=now() WHERE id=$1",vec![p.id.into(),display.into(),avatar.into(),account_version.into(),cutoff.into()]).await?;
            if p.activation_state != Activation::Active {
                self.store
                    .audit(
                        &txn,
                        &p,
                        "PROFILE_ACTIVATED",
                        Some(p.id),
                        json!({"previousState":p.activation_state}),
                    )
                    .await?;
                self.store
                    .event(
                        &txn,
                        &p,
                        "communications.profile.activated",
                        p.id,
                        p.version + 1,
                        json!({"principalId":p.principal_id,"state":"ACTIVE"}),
                    )
                    .await?;
            }
        }
        let current = self.store.profile(&txn, s, p.id, false).await?;
        txn.commit().await?;
        Ok(current)
    }
    pub async fn new_public_id<C: sea_orm::ConnectionTrait>(
        &self,
        db: &C,
        p: Uuid,
    ) -> Result<String> {
        for _ in 0..8 {
            let id = crypto::public_id();
            if execute(db,"INSERT INTO communication_public_ids(public_id,profile_id) VALUES($1,$2) ON CONFLICT(public_id) DO NOTHING",vec![id.clone().into(),p.into()]).await?>0 {return Ok(id);}
        }
        Err(Error::Unavailable)
    }
    pub async fn me(&self, p: &Profile) -> Result<Value> {
        Ok(
            json!({"schemaVersion":1,"profileId":p.id,"cfMeetId":self.store.current_public_id(&self.store.db,p.id).await?,"displayName":p.display_name,"avatarReference":p.avatar_reference,
            "activationState":p.activation_state,"version":p.version,"createdAt":p.created_at,"updatedAt":p.updated_at,
            "phoneVerification":{"status":"PROVIDER_BLOCKED","required":false}}),
        )
    }
    pub async fn rotate_public_id(
        &self,
        p: &Profile,
        a: &VerifiedAccount,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Rotate {
            expected_version: i64,
        }
        let request: Rotate =
            serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if a.authenticated_at
            .is_none_or(|at| at > Utc::now().timestamp() + 30 || Utc::now().timestamp() - at > 900)
        {
            return Err(Error::RecentAuthentication);
        }
        let txn = self.store.begin().await?;
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "public-id.rotate", key, &body)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let current = self.store.profile(&txn, p.scope(), p.id, true).await?;
        if current.version != request.expected_version {
            return Err(Error::Version);
        }
        self.store.rate(&txn, p.id, "public-id.rotate", 1).await?;
        execute(&txn,"UPDATE communication_public_ids SET current=false,retired_at=now() WHERE profile_id=$1 AND current",vec![p.id.into()]).await?;
        let id = self.new_public_id(&txn, p.id).await?;
        execute(&txn,"UPDATE communication_invites SET revoked_at=now(),version=version+1 WHERE owner_profile_id=$1 AND revoked_at IS NULL",vec![p.id.into()]).await?;
        execute(
            &txn,
            "UPDATE communication_profiles SET version=version+1,updated_at=now() WHERE id=$1",
            vec![p.id.into()],
        )
        .await?;
        self.store
            .audit(
                &txn,
                p,
                "PUBLIC_ID_ROTATED",
                Some(p.id),
                json!({"previousIdentifiersInvalidated":true,"invitationsRevoked":true}),
            )
            .await?;
        self.store
            .change(&txn, p.id, "PROFILE", Some(p.id), Some(current.version + 1))
            .await?;
        let result = json!({"schemaVersion":1,"cfMeetId":id,"version":current.version+1});
        self.store
            .finish_idempotency(&txn, p, "public-id.rotate", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn get_privacy(&self, p: &Profile) -> Result<Value> {
        let row = self.store.privacy(&self.store.db, p.id).await?;
        Ok(json!({"schemaVersion":1,"version":row.version,"privacy":row.settings()}))
    }
    pub async fn set_privacy(&self, p: &Profile, body: Value, key: Uuid) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Update {
            expected_version: i64,
            privacy: Privacy,
        }
        let update: Update = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        let txn = self.store.begin().await?;
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "privacy.update", key, &body)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let current = self.store.privacy(&txn, p.id).await?;
        if current.version != update.expected_version {
            return Err(Error::Version);
        }
        let encoded = serde_json::to_value(&update.privacy).map_err(|_| Error::Validation)?;
        let strings = [
            "invitationPermissions",
            "contactRequestPermissions",
            "incomingCallPolicy",
            "directChatPolicy",
            "publicProfileVisibility",
            "presenceVisibility",
        ];
        let mut values = vec![
            p.id.into(),
            current.version.into(),
            update.privacy.public_id_discoverable.into(),
        ];
        values.extend(
            strings
                .iter()
                .map(|k| encoded[*k].as_str().unwrap_or("").into()),
        );
        let changed=execute(&txn,"UPDATE communication_privacy SET public_id_discoverable=$3,invitation_permissions=$4,contact_request_permissions=$5,incoming_call_policy=$6,direct_chat_policy=$7,public_profile_visibility=$8,presence_visibility=$9,version=version+1,updated_at=now() WHERE profile_id=$1 AND version=$2",values).await?;
        if changed != 1 {
            return Err(Error::Version);
        }
        if update.privacy.invitation_permissions == InvitationPermission::Disabled {
            execute(&txn,"UPDATE communication_invites SET revoked_at=now(),version=version+1 WHERE owner_profile_id=$1 AND revoked_at IS NULL",vec![p.id.into()]).await?;
        }
        self.store
            .audit(
                &txn,
                p,
                "PRIVACY_UPDATED",
                Some(p.id),
                json!({"privacyVersion":current.version+1}),
            )
            .await?;
        self.store
            .event(
                &txn,
                p,
                "communications.privacy.changed",
                p.id,
                current.version + 1,
                json!({"principalId":p.principal_id,"privacyVersion":current.version+1}),
            )
            .await?;
        self.store
            .change(&txn, p.id, "PRIVACY", Some(p.id), Some(current.version + 1))
            .await?;
        let result =
            json!({"schemaVersion":1,"version":current.version+1,"privacy":update.privacy});
        self.store
            .finish_idempotency(&txn, p, "privacy.update", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
}

use crate::{
    auth, crypto,
    error::{Error, Result},
    model::*,
    store::{self, execute},
    State,
};
use cf_communications_policy::Scope;
use cf_service_auth::{DelegatedPlatformContext, OutboundServiceRequest, ServiceId};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// KF1: email lookups one person may make per hour (enumeration guard).
pub const EMAIL_LOOKUPS_PER_HOUR: i64 = 20;

/// KF1: a lower-cased, trimmed address of a plausible shape (`local@domain.tld`, no
/// spaces or control characters, at most 254 characters), or None.
pub fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    let ok = (3..=254).contains(&email.len())
        && !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then_some(email)
}

/// KF2: an "About" line is at most 140 characters with no control characters.
pub fn valid_about(about: Option<&str>) -> bool {
    about.is_none_or(|a| a.chars().count() <= 140 && !a.chars().any(char::is_control))
}

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
            "about":p.about,"activationState":p.activation_state,"version":p.version,"createdAt":p.created_at,"updatedAt":p.updated_at,
            "phoneVerification":{"status":"PROVIDER_BLOCKED","required":false}}),
        )
    }
    /// KF2: edit the "About" line (at most 140 characters, no control characters; empty or
    /// null clears it). Only CONNECTED contacts see it (contact views), never discovery.
    pub async fn patch_me(&self, p: &Profile, body: Value, key: Uuid) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Patch {
            expected_version: i64,
            about: Option<String>,
        }
        let update: Patch = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if body.get("about").is_none() {
            return Err(Error::Validation);
        }
        let about = update
            .about
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned);
        if !valid_about(about.as_deref()) {
            return Err(Error::Validation);
        }
        let txn = self.store.begin().await?;
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "profile.patch", key, &body)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let current = self.store.profile(&txn, p.scope(), p.id, true).await?;
        if current.version != update.expected_version {
            return Err(Error::Version);
        }
        execute(
            &txn,
            "UPDATE communication_profiles SET about=$2,version=version+1,updated_at=now() WHERE id=$1",
            vec![p.id.into(), about.clone().into()],
        )
        .await?;
        self.store
            .audit(&txn, p, "PROFILE_ABOUT_UPDATED", Some(p.id), json!({}))
            .await?;
        self.store
            .change(&txn, p.id, "PROFILE", Some(p.id), Some(current.version + 1))
            .await?;
        let updated = self.store.profile(&txn, p.scope(), p.id, false).await?;
        let result = self.me(&updated).await?;
        self.store
            .finish_idempotency(&txn, p, "profile.patch", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    /// KF1: find a person by their verified email address. The Account answers which
    /// account (if any) has that verified address; the person must have turned on
    /// `emailDiscoverable`, and then exactly the CF Meet ID discovery rules apply (active
    /// account, discoverable ID, profile visibility, no block either way). Every miss is
    /// the same 404 as an unknown address. Lookups are limited per person and hour
    /// (enumeration), on top of the shared discovery budget.
    pub async fn resolve_email(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        body: Value,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Resolve {
            email: String,
        }
        let request: Resolve = serde_json::from_value(body).map_err(|_| Error::Validation)?;
        let email = normalize_email(&request.email).ok_or(Error::Validation)?;
        self.limit_attempt(p, "discovery.email", EMAIL_LOOKUPS_PER_HOUR)
            .await?;
        self.limit_attempt(p, "discovery.resolve", 120).await?;
        let req = OutboundServiceRequest::new_operation(ServiceId::meeting_service())
            .with_delegated_context(scope.clone());
        let response = self
            .config
            .client
            .post_json(
                &req,
                "/internal/v1/accounts/communications/email-lookup",
                &json!({"email":email}),
            )
            .await
            .map_err(|_| Error::Unavailable)?;
        let answer: Value = match response.status() {
            200 => response.json().map_err(|_| Error::Unavailable)?,
            // An older Account image without the route: nobody can be found by email yet.
            404 => return Err(Error::NotFound),
            _ => return Err(Error::Unavailable),
        };
        if answer["found"] != true {
            return Err(Error::NotFound);
        }
        let reference = answer["accountReference"]
            .as_str()
            .filter(|r| r.len() <= 80)
            .ok_or(Error::Unavailable)?;
        let mut v = store::scoped(p.scope());
        v.push(reference.into());
        let target: Profile = store::one(&self.store.db,"SELECT to_jsonb(p) doc FROM communication_profiles p WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND account_reference=$4",v).await?.ok_or(Error::NotFound)?;
        if target.id != p.id
            && self
                .store
                .privacy(&self.store.db, target.id)
                .await?
                .email_discoverable
                != Some(true)
        {
            return Err(Error::NotFound);
        }
        self.visible_profile(scope, p, &target, false)
            .await
            .map_err(|e| match e {
                e @ (Error::Unavailable | Error::Database(_)) => e,
                _ => Error::NotFound,
            })
    }
    /// Verified, explicitly discoverable phone binding from Account authority, then
    /// the same Hub visibility and block checks as CF Meet ID discovery.
    pub async fn resolve_phone(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        body: Value,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Resolve {
            phone_number: String,
        }
        let request: Resolve = serde_json::from_value(body).map_err(|_| Error::Validation)?;
        let phone = request.phone_number.trim();
        if !(9..=16).contains(&phone.len())
            || !phone.starts_with('+')
            || !phone.as_bytes()[1].is_ascii_digit()
            || phone.as_bytes()[1] == b'0'
            || !phone.as_bytes()[2..].iter().all(u8::is_ascii_digit)
        {
            return Err(Error::Validation);
        }
        self.limit_attempt(p, "discovery.phone", EMAIL_LOOKUPS_PER_HOUR)
            .await?;
        self.limit_attempt(p, "discovery.resolve", 120).await?;
        let req = OutboundServiceRequest::new_operation(ServiceId::meeting_service())
            .with_delegated_context(scope.clone());
        let response = self
            .config
            .client
            .post_json(
                &req,
                "/internal/v1/accounts/communications/email-lookup",
                &json!({"phoneNumber":phone}),
            )
            .await
            .map_err(|_| Error::Unavailable)?;
        let answer: Value = match response.status() {
            200 => response.json().map_err(|_| Error::Unavailable)?,
            // An older Account image cannot resolve verified phone numbers.
            404 => return Err(Error::NotFound),
            _ => return Err(Error::Unavailable),
        };
        if answer["found"] != true {
            return Err(Error::NotFound);
        }
        let reference = answer["accountReference"]
            .as_str()
            .filter(|r| r.len() <= 80)
            .ok_or(Error::Unavailable)?;
        let mut v = store::scoped(p.scope());
        v.push(reference.into());
        let target: Profile = store::one(&self.store.db,"SELECT to_jsonb(p) doc FROM communication_profiles p WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND account_reference=$4",v).await?.ok_or(Error::NotFound)?;
        self.visible_profile(scope, p, &target, false)
            .await
            .map_err(|e| match e {
                e @ (Error::Unavailable | Error::Database(_)) => e,
                _ => Error::NotFound,
            })
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
        values.push(
            encoded["groupInvitationPolicy"]
                .as_str()
                .map(str::to_owned)
                .into(),
        );
        values.push(
            encoded["contactApproval"]
                .as_str()
                .map(str::to_owned)
                .into(),
        );
        values.push(update.privacy.email_discoverable.into());
        let changed=execute(&txn,"UPDATE communication_privacy SET public_id_discoverable=$3,invitation_permissions=$4,contact_request_permissions=$5,incoming_call_policy=$6,direct_chat_policy=$7,public_profile_visibility=$8,presence_visibility=$9,group_invitation_policy=COALESCE($10,group_invitation_policy),contact_approval=COALESCE($11,contact_approval),email_discoverable=COALESCE($12,email_discoverable),version=version+1,updated_at=now() WHERE profile_id=$1 AND version=$2",values).await?;
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
        // The stored settings: fields left out of the update (optional ones) read back
        // with their current values, never as null.
        let stored = self.store.privacy(&txn, p.id).await?.settings();
        let result = json!({"schemaVersion":1,"version":current.version+1,"privacy":stored});
        self.store
            .finish_idempotency(&txn, p, "privacy.update", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
}

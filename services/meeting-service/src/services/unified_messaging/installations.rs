//! A protected Messaging identity belongs to a trusted Account installation, not a
//! sign-in. Promotion is opt-in and requires fresh possession of the actual MLS key.
//! The service stores public identity bindings only; local history keys never arrive.
use super::{chat_error, exec, one, Acting, GatewayError, Peer, PrincipalKind, UnifiedMessaging};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    state::AppState,
};
use axum::http::Method;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine as _};
use cf_messaging_crypto::{
    binding,
    key_package::{validate_key_package, LifetimePolicy},
};
use chrono::Utc;
use rand::RngCore;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn refused() -> ApiError {
    ApiError::domain(
        ErrorCode::DeviceProofInvalid,
        "This Messaging installation could not be verified",
    )
}
fn unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::ChatUnavailable,
        "Protected Messaging installations are not available",
    )
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallationChallenge {
    pub challenge_id: Uuid,
    pub device_id: Uuid,
    pub account_device_id: Uuid,
    pub nonce: String,
    pub expires_at: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationProof {
    pub challenge_id: Uuid,
    /// Base64url RFC 9420 registration-bound KeyPackage, signed by this existing device.
    pub key_package: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallationBinding {
    pub persistent_identity: bool,
    pub device_id: Uuid,
    pub account_device_id: Uuid,
    pub protocol_version: u8,
}

/// Exact same Account installation (DECLARE proof checked at sign-in), still active
/// and trusted. Locking it serializes promotion with security/device revocation.
async fn trusted_installation<C: ConnectionTrait>(
    db: &C,
    user: Uuid,
    session: Uuid,
    lock: bool,
) -> ApiResult<Uuid> {
    let row=one(db,&format!(
        "SELECT d.id FROM account_sessions s JOIN account_devices d ON d.id=s.device_id WHERE s.id=$1 AND s.user_id=$2 AND d.user_id=$2 AND s.revoked_at IS NULL AND s.absolute_expires_at>now() AND d.state='ACTIVE' AND d.trust='TRUSTED'{}",
        if lock {" FOR UPDATE OF s,d"} else {""}),vec![session.into(),user.into()]).await?.ok_or_else(refused)?;
    Ok(row.try_get_by_index(0)?)
}

impl UnifiedMessaging {
    pub async fn installation_status(
        &self,
        user: Uuid,
        session: Uuid,
        device: Uuid,
    ) -> ApiResult<Value> {
        if !migration::messaging_installations_ready(&self.db).await? {
            return Err(unavailable());
        }
        let account_device = trusted_installation(&self.db, user, session, false).await?;
        let row=one(&self.db,"SELECT 1 FROM account_messaging_installations i WHERE i.environment_id=$1 AND i.messaging_device_id=$2 AND i.user_id=$3 AND i.account_device_id=$4 AND i.revoked_at IS NULL",vec![self.environment_id().into(),device.into(),user.into(),account_device.into()]).await?;
        let persistent = if row.is_some() {
            self.installation_device_key(user, device).await.is_ok()
        } else {
            false
        };
        Ok(
            json!({"persistentIdentity":persistent,"deviceId":device,"accountDeviceId":account_device,"protocolVersion":1}),
        )
    }
    /// Always query the authoritative active directory: a local identifier or an old
    /// proof cannot revive a terminally revoked device. The directory is bounded by
    /// the canonical active-device limit and returns at most 100 devices.
    async fn installation_device_key(&self, user: Uuid, device: Uuid) -> ApiResult<Vec<u8>> {
        let principal = self.principal_for_user(user).await?;
        let data = self
            .gateway
            .call(
                Peer::Messaging,
                Some(Acting {
                    principal_id: principal,
                    kind: PrincipalKind::NativeCfUser,
                }),
                Method::GET,
                &format!("/internal/v1/delegated/v1/messaging/principals/{principal}/devices"),
                &[],
                None,
            )
            .await
            .map_err(|e| chat_error(&e))?;
        let row = data["devices"]
            .as_array()
            .and_then(|rows| {
                rows.iter().find(|row| {
                    row["deviceId"] == device.to_string()
                        && row["principalId"] == principal.to_string()
                        && row["status"] == "ACTIVE"
                })
            })
            .ok_or_else(refused)?;
        let key = row["signaturePublicKey"]
            .as_str()
            .and_then(|v| B64.decode(v).ok())
            .filter(|k| k.len() == 32)
            .ok_or_else(refused)?;
        Ok(key)
    }

    pub async fn installation_challenge(
        &self,
        user: Uuid,
        session: Uuid,
        device: Uuid,
    ) -> ApiResult<InstallationChallenge> {
        if !migration::messaging_installations_ready(&self.db).await? {
            return Err(unavailable());
        }
        let account_device = trusted_installation(&self.db, user, session, false).await?;
        self.installation_device_key(user, device).await?;
        // One account can never silently move a Messaging identity between profiles.
        self.check_installation_identity(&self.db, user, account_device, device, None)
            .await?;
        let now = Utc::now();
        let recent=one(&self.db,"SELECT count(*) AS n FROM account_messaging_installation_challenges WHERE user_id=$1 AND created_at>now()-interval '1 hour'",vec![user.into()]).await?.ok_or_else(unavailable)?.try_get::<i64>("","n")?;
        if recent >= 120 {
            return Err(ApiError::domain(
                ErrorCode::RateLimited,
                "Too many installation challenges",
            ));
        }
        let id = Uuid::new_v4();
        let mut nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let expiry = now + chrono::Duration::minutes(5);
        exec(&self.db,"INSERT INTO account_messaging_installation_challenges(id,environment_id,messaging_device_id,user_id,account_device_id,account_session_id,nonce,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
            vec![id.into(),self.environment_id().into(),device.into(),user.into(),account_device.into(),session.into(),nonce.to_vec().into(),expiry.into()]).await?;
        Ok(InstallationChallenge {
            challenge_id: id,
            device_id: device,
            account_device_id: account_device,
            nonce: hex::encode(nonce),
            expires_at: expiry.to_rfc3339(),
        })
    }

    async fn check_installation_identity<C: ConnectionTrait>(
        &self,
        db: &C,
        user: Uuid,
        account_device: Uuid,
        device: Uuid,
        key: Option<&str>,
    ) -> ApiResult<()> {
        if let Some(row)=one(db,"SELECT user_id,account_device_id,signature_key_sha256,revoked_at FROM account_messaging_installations WHERE environment_id=$1 AND messaging_device_id=$2",vec![self.environment_id().into(),device.into()]).await? {
            if row.try_get::<Uuid>("","user_id")?!=user || row.try_get::<Uuid>("","account_device_id")?!=account_device
                || row.try_get::<Option<chrono::DateTime<Utc>>>("","revoked_at")?.is_some()
                || key.is_some_and(|expected|row.try_get::<String>("","signature_key_sha256").ok().as_deref()!=Some(expected))
            { return Err(refused()); }
        }
        Ok(())
    }

    pub async fn bind_installation(
        &self,
        state: &AppState,
        user: Uuid,
        session: Uuid,
        device: Uuid,
        proof: InstallationProof,
    ) -> ApiResult<InstallationBinding> {
        if !migration::messaging_installations_ready(&self.db).await? {
            return Err(unavailable());
        }
        if proof.key_package.len() > 24 * 1024 {
            return Err(refused());
        }
        let bytes = B64
            .decode(proof.key_package.trim_end_matches('='))
            .map_err(|_| refused())?;
        let kp = validate_key_package(
            &bytes,
            Utc::now().timestamp().max(0) as u64,
            &LifetimePolicy::default(),
        )
        .map_err(|_| refused())?;
        let key = self.installation_device_key(user, device).await?;
        kp.require_device(device, &key).map_err(|_| refused())?;
        if kp.last_resort {
            return Err(refused());
        }
        let fingerprint = hex::encode(Sha256::digest(&key));
        let txn = self.db.begin().await?;
        let account_device = trusted_installation(&txn, user, session, true).await?;
        let challenge=one(&txn,"SELECT nonce,consumed_at,expires_at FROM account_messaging_installation_challenges WHERE id=$1 AND environment_id=$2 AND messaging_device_id=$3 AND user_id=$4 AND account_device_id=$5 AND account_session_id=$6 FOR UPDATE",vec![proof.challenge_id.into(),self.environment_id().into(),device.into(),user.into(),account_device.into(),session.into()]).await?.ok_or_else(refused)?;
        let nonce: Vec<u8> = challenge.try_get("", "nonce")?;
        let nonce: [u8; 32] = nonce.try_into().map_err(|_| refused())?;
        let expires: chrono::DateTime<Utc> = challenge.try_get("", "expires_at")?;
        if expires <= Utc::now()
            || kp.application_id.as_deref()
                != Some(binding::registration_binding(proof.challenge_id, &nonce).as_slice())
        {
            return Err(refused());
        }
        self.check_installation_identity(&txn, user, account_device, device, Some(&fingerprint))
            .await?;
        // A consumed answer is idempotent only for the exact still-bound sign-in/key.
        if challenge
            .try_get::<Option<chrono::DateTime<Utc>>>("", "consumed_at")?
            .is_some()
        {
            self.require_installation_session(&txn, user, session, device)
                .await?;
            return Ok(InstallationBinding {
                persistent_identity: true,
                device_id: device,
                account_device_id: account_device,
                protocol_version: 1,
            });
        }
        // Account already verified device-key possession and local protection. If a Hub
        // is configured it must explicitly support promotion before we advertise it.
        if let Some(bridge) = state.communications.as_ref() {
            use cf_service_auth::{OutboundServiceRequest, ServiceId};
            let principal = self.principal_for_user(user).await?;
            let install_ref = hex::encode(Sha256::digest(
                format!(
                    "cfmeet-messaging-installation:v1|{}|{account_device}",
                    self.environment_id()
                )
                .as_bytes(),
            ));
            let outbound = OutboundServiceRequest::new_operation(ServiceId::communications())
                .with_delegated_context(bridge.scope.clone().with_initiating_principal(
                    principal,
                    cf_service_auth::DelegatedPrincipalKind::NativeCfUser,
                ));
            let response=bridge.client.post_json(&outbound,"/internal/v1/communications/devices/protect-installation",&json!({"accountReference":format!("cfmeet-user:{user}"),"principalId":principal,"chatDeviceId":device,"installationRef":install_ref})).await.map_err(|_|unavailable())?;
            let answer: Value = response.json().map_err(|_| unavailable())?;
            if response.status() != 200 || answer["persistentIdentity"] != true {
                return Err(unavailable());
            }
        }
        // Re-check the canonical owner after a concurrent explicit revocation/promotion.
        if self.installation_device_key(user, device).await? != key {
            return Err(refused());
        }
        exec(&txn,"INSERT INTO account_messaging_installations(environment_id,messaging_device_id,user_id,account_device_id,signature_key_sha256) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",vec![self.environment_id().into(),device.into(),user.into(),account_device.into(),fingerprint.clone().into()]).await?;
        self.check_installation_identity(&txn, user, account_device, device, Some(&fingerprint))
            .await?;
        exec(&txn,"INSERT INTO account_messaging_installation_sessions(environment_id,messaging_device_id,account_session_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",vec![self.environment_id().into(),device.into(),session.into()]).await?;
        exec(
            &txn,
            "UPDATE account_messaging_installation_challenges SET consumed_at=now() WHERE id=$1",
            vec![proof.challenge_id.into()],
        )
        .await?;
        txn.commit().await?;
        Ok(InstallationBinding {
            persistent_identity: true,
            device_id: device,
            account_device_id: account_device,
            protocol_version: 1,
        })
    }

    pub async fn require_installation_session<C: ConnectionTrait>(
        &self,
        db: &C,
        user: Uuid,
        session: Uuid,
        device: Uuid,
    ) -> ApiResult<()> {
        if !migration::messaging_installations_ready(db).await? {
            return Ok(());
        }
        let row=one(db,"SELECT user_id,account_device_id,revoked_at FROM account_messaging_installations WHERE environment_id=$1 AND messaging_device_id=$2",vec![self.environment_id().into(),device.into()]).await?;
        let Some(row) = row else { return Ok(()) }; // legacy policy is unchanged
        if row
            .try_get::<Option<chrono::DateTime<Utc>>>("", "revoked_at")?
            .is_some()
            || row.try_get::<Uuid>("", "user_id")? != user
        {
            return Err(refused());
        }
        let account_device = trusted_installation(db, user, session, false).await?;
        if row.try_get::<Uuid>("","account_device_id")?!=account_device || one(db,"SELECT 1 FROM account_messaging_installation_sessions WHERE environment_id=$1 AND messaging_device_id=$2 AND account_session_id=$3",vec![self.environment_id().into(),device.into(),session.into()]).await?.is_none(){ return Err(refused()); }
        Ok(())
    }

    /// The Account-device transaction writes this durable queue even with no open
    /// sign-ins. Only an affirmative canonical-owner answer completes a revocation.
    pub async fn process_installation_revocations(&self, limit: u64) -> ApiResult<u32> {
        if !migration::messaging_installations_ready(&self.db).await? {
            return Ok(0);
        }
        let mut done = 0;
        for _ in 0..limit {
            let txn = self.db.begin().await?;
            let Some(row)=one(&txn,"SELECT environment_id,messaging_device_id,user_id FROM account_messaging_installation_revocations WHERE environment_id=$1 AND done_at IS NULL AND next_attempt_at<=now() ORDER BY next_attempt_at LIMIT 1 FOR UPDATE SKIP LOCKED",vec![self.environment_id().into()]).await? else {break};
            let device: Uuid = row.try_get("", "messaging_device_id")?;
            let user: Uuid = row.try_get("", "user_id")?;
            let principal = self.principal_for_user(user).await?;
            let response = self
                .gateway
                .call(
                    Peer::Messaging,
                    Some(Acting {
                        principal_id: principal,
                        kind: PrincipalKind::NativeCfUser,
                    }),
                    Method::POST,
                    &format!("/internal/v1/delegated/v1/messaging/devices/{device}/revoke"),
                    &[],
                    Some(&json!({"reason":"COMPROMISED"})),
                )
                .await;
            let complete = matches!(
                response,
                Ok(_) | Err(GatewayError::Refused { status: 404, .. })
            );
            exec(&txn,"UPDATE account_messaging_installation_revocations SET done_at=CASE WHEN $3 THEN now() END,attempts=attempts+1,next_attempt_at=now()+make_interval(secs=>LEAST(300,2^LEAST(attempts,8))) WHERE environment_id=$1 AND messaging_device_id=$2",vec![self.environment_id().into(),device.into(),complete.into()]).await?;
            txn.commit().await?;
            if complete {
                done += 1
            } else {
                break;
            }
        }
        Ok(done)
    }
}

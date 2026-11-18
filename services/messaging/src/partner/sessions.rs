//! `POST /v1/messaging/user-sessions` (build contract §4): a partner device exchanges its
//! CF Meet user token for a MANAGED-only partner session device and a client token.
//!
//! 1. Platform Access verifies the token ([`super::Partner::verify`]); the effective `msg`
//!    grants decide the client scopes (`read` → `messages.read` / `attachments.read`,
//!    `send` → `messages.send` / `attachments.write`); no `msg` grant is
//!    `INSUFFICIENT_SCOPE`.
//! 2. The token's scope must be an ACTIVE, EXTERNAL application environment (Platform
//!    Access re-validates the signed delegated scope: a FIRST_PARTY tenant is refused) and
//!    the principal ACTIVE and messaging-capable in it (resolved afresh).
//! 3. A new session device bound to SHA-256("cfmeet-user-binding:v1|env|principal|
//!    installationId") is created, or the given ACTIVE one of the same binding renewed;
//!    it records the minting credential (`crd`) and service account (`sva`), and so does
//!    the client session (its `credential_id` is the minting credential).
//! 4. The client token lives at most `MESSAGING_SESSION_DEVICE_TOKEN_TTL_SECONDS` and
//!    never past the user token's own expiry; renewal goes back through this exchange.

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{binding_reference, valid_installation_id, Verification, VerifiedUserToken};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    identity::{CustomerAuthority, DelegatedScope, OwnerScope, PrincipalStatus},
    limits::{self, Window},
    realtime::{
        session_devices::{MAX_LIFETIME_HOURS, PLATFORM_USER_TOKEN},
        token::{Claims, AUDIENCE, ISSUER},
    },
    state::AppState,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExchangeRequest {
    pub installation_id: String,
    /// Renew this ACTIVE partner session device (same principal and installation).
    #[serde(default)]
    pub session_device_id: Option<Uuid>,
}

/// A verified, usable user token of an EXTERNAL environment and its trusted scope.
#[derive(Debug, Clone)]
pub struct PartnerCaller {
    pub token: VerifiedUserToken,
    pub scope: OwnerScope,
}

/// Verifies a presented user token and its authority NOW: Platform Access verification,
/// ACTIVE user, an ACTIVE EXTERNAL environment (delegated scope re-validated), the
/// principal ACTIVE and messaging-capable. Never cached beyond the verification itself.
pub async fn authenticate(state: &AppState, presented: &str) -> Result<PartnerCaller> {
    let now = state.now();
    let token = match state.partner.verify(presented, now).await {
        Ok(Verification::Valid(t)) => *t,
        Ok(Verification::Rejected(code)) => {
            return Err(Error::reason(ErrorCode::AuthenticationFailed, code))
        }
        Err(e) => {
            tracing::warn!(error = %e, "user-token verification unavailable");
            return Err(Error::code(ErrorCode::ServiceUnavailable));
        }
    };
    if token.expires_at <= now {
        return Err(Error::reason(
            ErrorCode::AuthenticationFailed,
            "USER_TOKEN_EXPIRED",
        ));
    }
    if token.user.status != "ACTIVE" {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_INACTIVE",
        ));
    }
    let scope = token.owner_scope().ok_or(Error::reason(
        ErrorCode::AuthenticationFailed,
        "USER_TOKEN_INVALID",
    ))?;
    // The environment must be ACTIVE and its tenant EXTERNAL: Platform Access re-validates
    // the token's scope as a signed delegated context (defence in depth; verification
    // already requires both).
    match state.authority.validate_delegated(&scope.delegated()).await {
        Ok(DelegatedScope::Valid {
            first_party: false,
            full_authority: true,
        }) => {}
        Ok(DelegatedScope::Valid {
            first_party: true, ..
        }) => {
            return Err(Error::reason(
                ErrorCode::PrincipalNotAuthorized,
                "PARTNER_SESSIONS_EXTERNAL_ONLY",
            ))
        }
        Ok(DelegatedScope::Valid { .. }) => return Err(Error::code(ErrorCode::AuthoritySuspended)),
        Ok(DelegatedScope::Invalid) => {
            return Err(Error::reason(
                ErrorCode::AuthenticationFailed,
                "USER_TOKEN_INVALID",
            ))
        }
        Err(e) => {
            tracing::warn!(error = %e, "identity authority unavailable");
            return Err(Error::code(ErrorCode::ServiceUnavailable));
        }
    }
    match state.principals.resolve(&scope, token.user.id).await {
        Ok(s @ PrincipalStatus::Active { .. }) if s.is_messaging_capable() => {}
        Ok(_) => {
            return Err(Error::reason(
                ErrorCode::PrincipalNotAuthorized,
                "PRINCIPAL_INACTIVE",
            ))
        }
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable");
            return Err(Error::code(ErrorCode::ServiceUnavailable));
        }
    }
    state.partner.ensure_baseline(&state.db, &scope, now).await;
    Ok(PartnerCaller { token, scope })
}

#[derive(Debug, Clone, FromQueryResult)]
struct DeviceRow {
    id: Uuid,
    expires_at: DateTime<Utc>,
}

/// `POST /v1/messaging/user-sessions`.
pub async fn exchange(state: &AppState, presented: &str, request: ExchangeRequest) -> Result<Value> {
    if !state.partner.settings.session_devices_enabled {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "PARTNER_SESSION_DEVICES_DISABLED",
        ));
    }
    let keys = state.realtime.keys.as_ref().ok_or(Error::reason(
        ErrorCode::FeatureDisabled,
        "CLIENT_TOKENS_NOT_CONFIGURED",
    ))?;
    if !valid_installation_id(&request.installation_id) {
        return Err(Error::validation(
            "installationId must be 8-128 characters of [A-Za-z0-9._-]",
        ));
    }
    let caller = authenticate(state, presented).await?;
    let scopes = caller.token.messaging_scopes();
    if scopes.is_empty() {
        return Err(Error::reason(ErrorCode::InsufficientScope, "MSG_GRANT_REQUIRED"));
    }
    let now = state.now();
    let principal = caller.token.user.id;
    let scope = caller.scope;
    let key = format!(
        "partner-session:{}:{}",
        scope.environment_id.simple(),
        principal.simple()
    );
    if let Some(retry) = limits::hit(&state.db, &key, Window::Hour, 240, now).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let ttl = state.realtime.settings.session_device_token_ttl;
    let not_after = caller.token.expires_at;
    if not_after <= now + chrono::Duration::seconds(10) {
        return Err(Error::reason(
            ErrorCode::AuthenticationFailed,
            "USER_TOKEN_EXPIRED",
        ));
    }
    let binding = binding_reference(scope.environment_id, principal, &request.installation_id);
    let credential = caller.token.credential_id;
    let scopes_text = scopes.join(" ");
    let installation = request.installation_id.clone();
    let (device, session_id, expires) = state
        .db
        .transaction::<_, (DeviceRow, Uuid, DateTime<Utc>), Error>(move |txn| {
            Box::pin(async move {
                let device: DeviceRow = match request.session_device_id {
                    Some(id) => {
                        let row: DeviceRow = db::one(
                            txn,
                            "SELECT id, expires_at FROM messaging_session_devices \
                             WHERE id = $1 AND environment_id = $2 AND principal_id = $3 AND binding_kind = $4 \
                               AND account_session_sha256 = $5 AND status = 'ACTIVE' AND expires_at > $6 \
                             FOR UPDATE",
                            vec![
                                id.into(),
                                scope.environment_id.into(),
                                principal.into(),
                                PLATFORM_USER_TOKEN.into(),
                                binding.clone().into(),
                                now.into(),
                            ],
                        )
                        .await?
                        // Unknown, foreign, revoked, expired or another installation:
                        // indistinguishable.
                        .ok_or(Error::code(ErrorCode::NotFound))?;
                        // A partner rotating its API key renews under the new minting
                        // credential (revoking the old one no longer ends this device).
                        db::exec(
                            txn,
                            "UPDATE messaging_session_devices SET minting_credential_id = $2, minting_service_account_id = $3 \
                             WHERE id = $1",
                            vec![id.into(), credential.into(), scope.service_account_id.into()],
                        )
                        .await?;
                        row
                    }
                    None => {
                        let id = Uuid::new_v4();
                        let expires_at = now + chrono::Duration::hours(MAX_LIFETIME_HOURS);
                        db::exec(
                            txn,
                            "INSERT INTO messaging_session_devices (id, tenant_id, application_id, environment_id, \
                               principal_id, account_session_sha256, created_at, expires_at, binding_kind, \
                               installation_id, minting_credential_id, minting_service_account_id) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                            vec![
                                id.into(),
                                scope.tenant_id.into(),
                                scope.application_id.into(),
                                scope.environment_id.into(),
                                principal.into(),
                                binding.clone().into(),
                                now.into(),
                                expires_at.into(),
                                PLATFORM_USER_TOKEN.into(),
                                installation.clone().into(),
                                credential.into(),
                                scope.service_account_id.into(),
                            ],
                        )
                        .await?;
                        DeviceRow { id, expires_at }
                    }
                };
                let expires = (now + ttl).min(not_after).min(device.expires_at);
                let session_id = Uuid::new_v4();
                db::exec(
                    txn,
                    "INSERT INTO messaging_client_sessions (id, tenant_id, application_id, environment_id, environment_kind, \
                       service_account_id, principal_id, device_id, credential_id, scopes, created_at, expires_at, device_kind) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'SESSION_DEVICE')",
                    vec![
                        session_id.into(),
                        scope.tenant_id.into(),
                        scope.application_id.into(),
                        scope.environment_id.into(),
                        scope.environment_kind.as_str().into(),
                        scope.service_account_id.into(),
                        principal.into(),
                        device.id.into(),
                        credential.into(),
                        scopes_text.into(),
                        now.into(),
                        expires.into(),
                    ],
                )
                .await?;
                Ok((device, session_id, expires))
            })
        })
        .await
        .map_err(Error::from)?;
    let claims = Claims {
        iss: ISSUER.into(),
        aud: AUDIENCE.into(),
        sub: principal,
        tid: scope.tenant_id,
        aid: scope.application_id,
        eid: scope.environment_id,
        did: device.id,
        sid: session_id,
        jti: Uuid::new_v4(),
        iat: now.timestamp(),
        exp: expires.timestamp(),
        scp: scopes.iter().map(|s| s.to_string()).collect(),
    };
    crate::metrics::inc("messaging_partner_session_tokens_issued_total", &[]);
    Ok(json!({
        "principalId": principal,
        "sessionDeviceId": device.id,
        "sessionDeviceExpiresAt": rfc3339(device.expires_at),
        "clientToken": keys.sign(&claims),
        "clientTokenExpiresAt": rfc3339(expires),
    }))
}

//! Session devices: MANAGED realtime without an end-to-end encrypted device (Blueprint
//! v1.0 Section 13, decision D-5, ADR 0049 amendment A1).
//!
//! A first-party relay (CF Meet's Meeting Service, acting for a VERIFIED account session)
//! may obtain a client token bound to a *session device* instead of an MLS device. The
//! session device is a separate, bounded identity:
//!
//! * it is not a `messaging_devices` row, so it can never publish KeyPackages, be claimed
//!   into an MLS group, receive Welcomes, register push, take part in a history transfer
//!   or be listed / counted as an end-to-end encrypted device;
//! * its client sessions are accepted ONLY by the realtime gateway (`POST /v1/realtime/
//!   tickets` and the WebSocket); every device-direct REST operation refuses them
//!   (`CLIENT_TOKENS_NOT_ENABLED` / `SESSION_DEVICE_REALTIME_ONLY`);
//! * the gateway lets it subscribe to MANAGED conversations only: a PRIVATE_E2EE
//!   subscription is refused (`E2EE_DEVICE_REQUIRED`), so it never receives ciphertext;
//! * it is bound to the account session that created it through an opaque reference the
//!   relay derives (domain-separated SHA-256; Messaging can match it, never reverse it).
//!   Each token lives at most `MESSAGING_SESSION_DEVICE_TOKEN_TTL_SECONDS` (default 300)
//!   and never past the relay's `notAfter` (the account access token's own expiry); every
//!   renewal goes back through the relay, which re-verifies the account session and
//!   epoch. Ending the account session revokes its session devices
//!   (`POST /v1/messaging/session-devices/revoke`), "log out everywhere" revokes all of
//!   them, and either revocation closes open connections through the outbox.
//!
//! Enabling session devices (`MESSAGING_SESSION_DEVICES_ENABLED`) is independent of, and
//! never implies, E2EE device registration, which production keeps refusing.

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    sessions::CLIENT_SCOPES,
    token::{Claims, AUDIENCE, ISSUER},
};
use crate::{
    conversations::service::{request_context, rfc3339, scope_of},
    db,
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    state::AppState,
};

/// `device_kind` of a client session bound to a session device.
pub const SESSION_DEVICE: &str = "SESSION_DEVICE";
/// `device_kind` of a client session bound to a registered MLS device.
pub const E2EE_DEVICE: &str = "E2EE_DEVICE";
/// The only capability a session device ever has.
pub const CAPABILITIES: &[&str] = &["REALTIME_MANAGED"];
/// A session device lives at most this long (a client then creates a new one).
pub const MAX_LIFETIME_HOURS: i64 = 24;

fn valid_reference(r: &str) -> bool {
    r.len() == 64 && r.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Session devices are a first-party relay capability (D-5): a customer application
/// keeps registering MLS devices for its users.
fn require_first_party(ctx: &TrustedContext) -> Result<()> {
    if ctx.first_party {
        Ok(())
    } else {
        Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "SESSION_DEVICES_FIRST_PARTY_ONLY",
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueRequest {
    /// The relay's opaque reference of the verified account session (64 hex).
    pub account_session_ref: String,
    /// No token outlives this instant (the account access token's expiry).
    pub not_after: DateTime<Utc>,
    /// Renew a token of this ACTIVE session device (same account session) instead of
    /// creating one, so an open connection can continue with `auth.refresh`.
    #[serde(default)]
    pub session_device_id: Option<Uuid>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeRequest {
    pub account_session_ref: String,
}

#[derive(Debug, Clone, FromQueryResult)]
struct DeviceRow {
    id: Uuid,
    expires_at: DateTime<Utc>,
}

/// `POST /v1/messaging/session-devices`: a session device (new or renewed) and a
/// realtime-only client token bound to it.
pub async fn issue(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: IssueRequest,
) -> Result<Value> {
    let rt = &state.realtime;
    if !rt.settings.session_devices_enabled {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "SESSION_DEVICES_DISABLED",
        ));
    }
    let keys = rt.keys.as_ref().ok_or(Error::reason(
        ErrorCode::FeatureDisabled,
        "CLIENT_TOKENS_NOT_CONFIGURED",
    ))?;
    require_first_party(ctx)?;
    if !valid_reference(&request.account_session_ref) {
        return Err(Error::validation(
            "accountSessionRef must be 64 lower-case hexadecimal characters",
        ));
    }
    let now = state.now();
    let max = rt.settings.session_device_token_ttl.num_seconds();
    let ttl = request.ttl_seconds.unwrap_or(max);
    if !(60..=max).contains(&ttl) {
        return Err(Error::validation(format!(
            "ttlSeconds must be between 60 and {max}"
        )));
    }
    if request.not_after <= now + chrono::Duration::seconds(30) {
        return Err(Error::reason(
            ErrorCode::AuthenticationFailed,
            "ACCOUNT_SESSION_EXPIRING",
        ));
    }
    let scopes: Vec<&str> = CLIENT_SCOPES
        .iter()
        .copied()
        .filter(|s| ctx.has_scope(s))
        .collect();
    if !scopes.contains(&"messages.read") {
        return Err(Error::code(ErrorCode::InsufficientScope));
    }
    let key = format!(
        "session-device:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(&state.db, &key, Window::Hour, 240, now).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let ctx2 = ctx.clone();
    let principal = acting.principal_id;
    let scopes_text = scopes.join(" ");
    let (device, session_id, expires) = state
        .db
        .transaction::<_, (DeviceRow, Uuid, DateTime<Utc>), Error>(move |txn| {
            Box::pin(async move {
                let device: DeviceRow = match request.session_device_id {
                    Some(id) => db::one(
                        txn,
                        "SELECT id, expires_at FROM messaging_session_devices \
                         WHERE id = $1 AND environment_id = $2 AND principal_id = $3 \
                           AND account_session_sha256 = $4 AND status = 'ACTIVE' AND expires_at > $5 \
                         FOR UPDATE",
                        vec![
                            id.into(),
                            ctx2.scope.environment_id.into(),
                            principal.into(),
                            request.account_session_ref.clone().into(),
                            now.into(),
                        ],
                    )
                    .await?
                    // Unknown, foreign, revoked, expired or bound to another account
                    // session: indistinguishable.
                    .ok_or(Error::code(ErrorCode::NotFound))?,
                    None => {
                        let id = Uuid::new_v4();
                        let expires_at = now + chrono::Duration::hours(MAX_LIFETIME_HOURS);
                        db::exec(
                            txn,
                            "INSERT INTO messaging_session_devices (id, tenant_id, application_id, environment_id, \
                               principal_id, account_session_sha256, created_at, expires_at) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                            vec![
                                id.into(),
                                ctx2.scope.tenant_id.into(),
                                ctx2.scope.application_id.into(),
                                ctx2.scope.environment_id.into(),
                                principal.into(),
                                request.account_session_ref.clone().into(),
                                now.into(),
                                expires_at.into(),
                            ],
                        )
                        .await?;
                        DeviceRow { id, expires_at }
                    }
                };
                let expires = (now + chrono::Duration::seconds(ttl))
                    .min(request.not_after)
                    .min(device.expires_at);
                let session_id = Uuid::new_v4();
                db::exec(
                    txn,
                    "INSERT INTO messaging_client_sessions (id, tenant_id, application_id, environment_id, environment_kind, \
                       service_account_id, principal_id, device_id, credential_id, scopes, created_at, expires_at, device_kind) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'SESSION_DEVICE')",
                    vec![
                        session_id.into(),
                        ctx2.scope.tenant_id.into(),
                        ctx2.scope.application_id.into(),
                        ctx2.scope.environment_id.into(),
                        ctx2.scope.environment_kind.as_str().into(),
                        ctx2.scope.service_account_id.into(),
                        principal.into(),
                        device.id.into(),
                        ctx2.credential_id.into(),
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
        sub: acting.principal_id,
        tid: ctx.scope.tenant_id,
        aid: ctx.scope.application_id,
        eid: ctx.scope.environment_id,
        did: device.id,
        sid: session_id,
        jti: Uuid::new_v4(),
        iat: now.timestamp(),
        exp: expires.timestamp(),
        scp: scopes.iter().map(|s| s.to_string()).collect(),
    };
    crate::metrics::inc("messaging_session_device_tokens_issued_total", &[]);
    Ok(json!({
        "token": keys.sign(&claims),
        "tokenType": "CF-Client-Token",
        "sessionId": session_id,
        "deviceId": device.id,
        "deviceKind": SESSION_DEVICE,
        "sessionDevice": {
            "sessionDeviceId": device.id,
            "status": "ACTIVE",
            "capabilities": CAPABILITIES,
            "expiresAt": rfc3339(device.expires_at),
        },
        "scopes": scopes,
        "expiresAt": rfc3339(expires),
    }))
}

/// `POST /v1/messaging/session-devices/revoke`: the account session behind
/// `accountSessionRef` ended; its session devices and their client sessions end now
/// (connected gateways close them after commit). Always available, even with session
/// devices switched off, so a revocation is never lost.
pub async fn revoke(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: RevokeRequest,
) -> Result<Value> {
    require_first_party(ctx)?;
    if !valid_reference(&request.account_session_ref) {
        return Err(Error::validation(
            "accountSessionRef must be 64 lower-case hexadecimal characters",
        ));
    }
    #[derive(FromQueryResult)]
    struct Id {
        id: Uuid,
    }
    let now = state.now();
    let ctx2 = ctx.clone();
    let principal = acting.principal_id;
    let result = state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let devices: Vec<Id> = db::all(
                    txn,
                    "UPDATE messaging_session_devices SET status = 'REVOKED', revoked_at = $4, revoked_reason = 'ACCOUNT_SESSION_ENDED' \
                     WHERE environment_id = $1 AND principal_id = $2 AND account_session_sha256 = $3 AND status = 'ACTIVE' \
                     RETURNING id",
                    vec![
                        ctx2.scope.environment_id.into(),
                        principal.into(),
                        request.account_session_ref.into(),
                        now.into(),
                    ],
                )
                .await?;
                let ids: Vec<Uuid> = devices.iter().map(|d| d.id).collect();
                let sessions = revoke_device_sessions(txn, &ctx2, principal, &ids, now).await?;
                Ok(json!({ "revokedSessionDevices": ids.len(), "revokedSessions": sessions }))
            })
        })
        .await
        .map_err(Error::from)?;
    state.realtime.outbox_wakeup.notify_one();
    Ok(result)
}

/// Revokes every ACTIVE client session of the given session devices and queues the
/// revocation events that close their connections (same transaction).
pub(crate) async fn revoke_device_sessions<C: sea_orm::ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    principal: Uuid,
    devices: &[Uuid],
    now: DateTime<Utc>,
) -> Result<usize> {
    if devices.is_empty() {
        return Ok(0);
    }
    #[derive(FromQueryResult)]
    struct Id {
        id: Uuid,
    }
    let revoked: Vec<Id> = db::all(
        txn,
        "UPDATE messaging_client_sessions SET status = 'REVOKED', revoked_at = $3, revoked_reason = 'DEVICE_REVOKED' \
         WHERE device_kind = 'SESSION_DEVICE' AND device_id = ANY($1) AND principal_id = $2 AND status = 'ACTIVE' \
         RETURNING id",
        vec![devices.to_vec().into(), principal.into(), now.into()],
    )
    .await?;
    let request_ctx = request_context();
    for r in &revoked {
        outbox::enqueue(
            txn,
            NewEvent {
                event_type: "messaging.client-session.revoked",
                aggregate_type: "CLIENT_SESSION",
                aggregate_id: r.id,
                scope: scope_of(ctx),
                payload: json!({ "session_id": r.id, "principal_id": principal }),
            },
            &request_ctx,
            now,
        )
        .await?;
    }
    Ok(revoked.len())
}

/// Whether a session device is still usable (ACTIVE, unexpired, the feature on).
pub async fn active<C: sea_orm::ConnectionTrait>(
    db: &C,
    enabled: bool,
    device_id: Uuid,
    principal_id: Uuid,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    if !enabled {
        return Ok(false);
    }
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_session_devices WHERE id = $1 AND principal_id = $2 AND environment_id = $3 \
           AND status = 'ACTIVE' AND expires_at > $4",
        vec![
            device_id.into(),
            principal_id.into(),
            environment_id.into(),
            now.into(),
        ],
    )
    .await?
        == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_session_references_are_opaque_lower_hex_digests() {
        assert!(valid_reference(&"a1".repeat(32)));
        for bad in [
            "",
            &"A1".repeat(32),
            &"a1".repeat(31),
            &"a1".repeat(33),
            &format!("{}zz", "a".repeat(62)),
        ] {
            assert!(!valid_reference(bad), "{bad}");
        }
    }
}

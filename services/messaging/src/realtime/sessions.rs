//! Client sessions, client access tokens and single-use WebSocket tickets (§59).
//!
//! 1. The application backend (Platform Access credential, `messages.read`, acting
//!    principal) asks for a token for one of the principal's ACTIVE devices:
//!    `POST /v1/messaging/client-tokens`. Messaging creates a session row binding
//!    tenant / application / environment / service account / principal / device /
//!    credential and returns a short-lived signed token (never a permanent credential).
//! 2. The DEVICE exchanges it (`Authorization: CF-Client-Token …`) for a single-use
//!    ticket valid for seconds: `POST /v1/messaging/realtime/tickets`.
//! 3. The device opens the WebSocket with the ticket in `Sec-WebSocket-Protocol` — never
//!    in the URL, so neither tokens nor tickets reach proxy access logs or analytics.
//!
//! At every step — and periodically for open connections — the session (ACTIVE, not
//! expired), the device (ACTIVE) and the principal (ACTIVE in its environment, resolved
//! by Platform Access inside the signed delegated scope, which also fails for a
//! suspended tenant / application / environment) are re-validated.

use chrono::{DateTime, Utc};
use rand::RngCore;
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::token::{Claims, AUDIENCE, ISSUER};
use crate::{
    conversations::service::{request_context, rfc3339, scope_of},
    db,
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    http::middleware::Acting,
    identity::{EnvironmentKind, OwnerScope, PrincipalStatus, TrustedContext},
    limits::{self, Window},
    state::AppState,
};

/// Messaging scopes a client token may carry (never more than the credential holds).
pub const CLIENT_SCOPES: &[&str] = &["messages.read", "messages.send"];

#[derive(Debug, Clone, FromQueryResult)]
pub struct SessionRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub service_account_id: Uuid,
    pub principal_id: Uuid,
    pub device_id: Uuid,
    pub credential_id: Uuid,
    pub scopes: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
}

const SESSION_COLUMNS: &str =
    "id, tenant_id, application_id, environment_id, environment_kind, service_account_id, \
     principal_id, device_id, credential_id, scopes, status, expires_at";

impl SessionRow {
    pub fn owner_scope(&self) -> Option<OwnerScope> {
        Some(OwnerScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
            environment_kind: EnvironmentKind::parse(&self.environment_kind)?,
            service_account_id: self.service_account_id,
        })
    }

    /// The session as a trusted context for Messaging operations performed on behalf of
    /// the device (its credential's scope, restricted to the session's scopes).
    pub fn trusted_context(&self) -> Option<TrustedContext> {
        Some(TrustedContext {
            scope: self.owner_scope()?,
            credential_id: self.credential_id,
            first_party: false,
            full_authority: true,
            scopes: self
                .scopes
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            principal_restriction: Some([self.principal_id].into_iter().collect()),
        })
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.split(' ').any(|s| s == scope)
    }
}

pub async fn load_session(state: &AppState, id: Uuid) -> Result<Option<SessionRow>> {
    Ok(db::one(
        &state.db,
        &format!("SELECT {SESSION_COLUMNS} FROM messaging_client_sessions WHERE id = $1"),
        vec![id.into()],
    )
    .await?)
}

/// Why a session is no longer valid (connection close reasons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    Expired,
    SessionRevoked,
    DeviceRevoked,
    PrincipalInactive,
    Unavailable,
}

impl Invalid {
    pub fn reason(&self) -> &'static str {
        match self {
            Invalid::Expired => "SESSION_EXPIRED",
            Invalid::SessionRevoked => "SESSION_REVOKED",
            Invalid::DeviceRevoked => "DEVICE_REVOKED",
            Invalid::PrincipalInactive => "PRINCIPAL_INACTIVE",
            Invalid::Unavailable => "AUTHORITY_UNAVAILABLE",
        }
    }

    pub fn error(&self) -> Error {
        match self {
            Invalid::Unavailable => Error::code(ErrorCode::ServiceUnavailable),
            other => Error::reason(ErrorCode::AuthenticationFailed, other.reason()),
        }
    }
}

/// Re-validates a session now: row, expiry, device and principal (fresh resolution).
pub async fn validate(state: &AppState, session: &SessionRow) -> std::result::Result<(), Invalid> {
    let current = load_session(state, session.id)
        .await
        .map_err(|_| Invalid::Unavailable)?
        .ok_or(Invalid::SessionRevoked)?;
    if current.status != "ACTIVE" {
        return Err(Invalid::SessionRevoked);
    }
    if current.expires_at <= state.now() {
        return Err(Invalid::Expired);
    }
    let device_active = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_devices WHERE id = $1 AND principal_id = $2 AND environment_id = $3 AND status = 'ACTIVE'",
        vec![current.device_id.into(), current.principal_id.into(), current.environment_id.into()],
    )
    .await
    .map_err(|_| Invalid::Unavailable)?;
    if device_active != 1 {
        return Err(Invalid::DeviceRevoked);
    }
    let scope = current.owner_scope().ok_or(Invalid::SessionRevoked)?;
    match state.principals.resolve(&scope, current.principal_id).await {
        Ok(s @ PrincipalStatus::Active { .. }) if s.is_messaging_capable() => Ok(()),
        Ok(_) => Err(Invalid::PrincipalInactive),
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable during session validation");
            Err(Invalid::Unavailable)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueRequest {
    pub device_id: Uuid,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

/// `POST /v1/messaging/client-tokens`.
pub async fn issue_token(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: IssueRequest,
) -> Result<Value> {
    let rt = &state.realtime;
    let keys = rt.keys.as_ref().ok_or(Error::reason(
        ErrorCode::FeatureDisabled,
        "CLIENT_TOKENS_NOT_CONFIGURED",
    ))?;
    let max = rt.settings.token_ttl.num_seconds();
    let ttl = request.ttl_seconds.unwrap_or(max);
    if !(60..=max).contains(&ttl) {
        return Err(Error::validation(format!(
            "ttlSeconds must be between 60 and {max}"
        )));
    }
    crate::messages::e2ee::require_own_device(
        &state.db,
        ctx,
        acting.principal_id,
        request.device_id,
    )
    .await?;
    let now = state.now();
    let key = format!("client-token:{}", request.device_id.simple());
    if let Some(retry) = limits::hit(&state.db, &key, Window::Hour, 120, now).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let scopes: Vec<&str> = CLIENT_SCOPES
        .iter()
        .copied()
        .filter(|s| ctx.has_scope(s))
        .collect();
    if !scopes.contains(&"messages.read") {
        return Err(Error::code(ErrorCode::InsufficientScope));
    }
    let session_id = Uuid::new_v4();
    let expires = now + chrono::Duration::seconds(ttl);
    db::exec(
        &state.db,
        "INSERT INTO messaging_client_sessions (id, tenant_id, application_id, environment_id, environment_kind, \
           service_account_id, principal_id, device_id, credential_id, scopes, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            session_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            ctx.scope.environment_kind.as_str().into(),
            ctx.scope.service_account_id.into(),
            acting.principal_id.into(),
            request.device_id.into(),
            ctx.credential_id.into(),
            scopes.join(" ").into(),
            now.into(),
            expires.into(),
        ],
    )
    .await?;
    let claims = Claims {
        iss: ISSUER.into(),
        aud: AUDIENCE.into(),
        sub: acting.principal_id,
        tid: ctx.scope.tenant_id,
        aid: ctx.scope.application_id,
        eid: ctx.scope.environment_id,
        did: request.device_id,
        sid: session_id,
        jti: Uuid::new_v4(),
        iat: now.timestamp(),
        exp: expires.timestamp(),
        scp: scopes.iter().map(|s| s.to_string()).collect(),
    };
    crate::metrics::inc("messaging_client_tokens_issued_total", &[]);
    Ok(json!({
        "token": keys.sign(&claims),
        "tokenType": "CF-Client-Token",
        "sessionId": session_id,
        "deviceId": request.device_id,
        "scopes": scopes,
        "expiresAt": rfc3339(expires),
    }))
}

/// Verifies a presented client token and returns its (still valid) session.
pub async fn authenticate(state: &AppState, presented: &str) -> Result<SessionRow> {
    let keys = state.realtime.keys.as_ref().ok_or(Error::reason(
        ErrorCode::FeatureDisabled,
        "CLIENT_TOKENS_NOT_CONFIGURED",
    ))?;
    let claims = keys.verify(presented, state.now()).map_err(|e| {
        crate::metrics::inc(
            "messaging_client_token_rejected_total",
            &[("reason", e.as_str())],
        );
        Error::reason(ErrorCode::AuthenticationFailed, "INVALID_CLIENT_TOKEN")
    })?;
    let session = load_session(state, claims.sid).await?.ok_or(Error::reason(
        ErrorCode::AuthenticationFailed,
        "SESSION_REVOKED",
    ))?;
    // Every bound field must match the session the server recorded (defence in depth
    // against a forged-but-signed or a cross-tenant token).
    if session.device_id != claims.did
        || session.principal_id != claims.sub
        || session.environment_id != claims.eid
        || session.tenant_id != claims.tid
        || session.application_id != claims.aid
    {
        return Err(Error::reason(
            ErrorCode::AuthenticationFailed,
            "TOKEN_SESSION_MISMATCH",
        ));
    }
    validate(state, &session).await.map_err(|i| i.error())?;
    Ok(session)
}

fn ticket_hash(ticket: &str) -> String {
    hex::encode(Sha256::digest(ticket.as_bytes()))
}

/// `POST /v1/messaging/realtime/tickets`: a single-use connection ticket.
pub async fn issue_ticket(state: &AppState, session: &SessionRow) -> Result<Value> {
    let now = state.now();
    let key = format!("rt-ticket:{}", session.id.simple());
    if let Some(retry) = limits::hit(&state.db, &key, Window::Minute, 30, now).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let ticket = hex::encode(raw);
    let expires = now + state.realtime.settings.ticket_ttl;
    db::exec(
        &state.db,
        "INSERT INTO messaging_realtime_tickets (id, ticket_sha256, session_id, created_at, expires_at) VALUES ($1, $2, $3, $4, $5)",
        vec![Uuid::new_v4().into(), ticket_hash(&ticket).into(), session.id.into(), now.into(), expires.into()],
    )
    .await?;
    Ok(json!({
        "ticket": ticket,
        "expiresAt": rfc3339(expires),
        "path": "/v1/realtime",
        "subprotocol": super::SUBPROTOCOL,
        "ticketSubprotocolPrefix": super::TICKET_PROTOCOL_PREFIX,
    }))
}

/// Consumes a ticket exactly once (a replay finds it consumed) and returns its session.
pub async fn consume_ticket(state: &AppState, ticket: &str) -> Result<SessionRow> {
    if ticket.len() != 64 || !ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::reason(
            ErrorCode::AuthenticationFailed,
            "INVALID_TICKET",
        ));
    }
    #[derive(FromQueryResult)]
    struct T {
        session_id: Uuid,
    }
    let t: Option<T> = db::one(
        &state.db,
        "UPDATE messaging_realtime_tickets SET consumed_at = $2 WHERE ticket_sha256 = $1 AND consumed_at IS NULL \
           AND expires_at > $2 RETURNING session_id",
        vec![ticket_hash(ticket).into(), state.now().into()],
    )
    .await?;
    let t = t.ok_or_else(|| {
        crate::metrics::inc("messaging_realtime_ticket_rejected_total", &[]);
        Error::reason(ErrorCode::AuthenticationFailed, "INVALID_TICKET")
    })?;
    let session = load_session(state, t.session_id)
        .await?
        .ok_or(Error::reason(
            ErrorCode::AuthenticationFailed,
            "SESSION_REVOKED",
        ))?;
    validate(state, &session).await.map_err(|i| i.error())?;
    Ok(session)
}

/// Revokes a session (terminal) and notifies every gateway after commit (outbox).
pub async fn revoke_session(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    session_id: Uuid,
) -> Result<Value> {
    let now = state.now();
    let request_ctx = request_context();
    let st = state.clone();
    let ctx2 = ctx.clone();
    let acting = acting.clone();
    use sea_orm::TransactionTrait;
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let n = db::exec(
                    txn,
                    "UPDATE messaging_client_sessions SET status = 'REVOKED', revoked_at = $4, revoked_reason = 'USER_REQUESTED' \
                     WHERE id = $1 AND principal_id = $2 AND environment_id = $3 AND status = 'ACTIVE'",
                    vec![session_id.into(), acting.principal_id.into(), ctx2.scope.environment_id.into(), now.into()],
                )
                .await?;
                if n == 0 {
                    let exists = db::scalar_i64(
                        txn,
                        "SELECT count(*) FROM messaging_client_sessions WHERE id = $1 AND principal_id = $2 AND environment_id = $3",
                        vec![session_id.into(), acting.principal_id.into(), ctx2.scope.environment_id.into()],
                    )
                    .await?;
                    if exists == 0 {
                        return Err(Error::code(ErrorCode::NotFound));
                    }
                } else {
                    outbox::enqueue(
                        txn,
                        NewEvent {
                            event_type: "messaging.client-session.revoked",
                            aggregate_type: "CLIENT_SESSION",
                            aggregate_id: session_id,
                            scope: scope_of(&ctx2),
                            payload: json!({ "session_id": session_id, "principal_id": acting.principal_id }),
                        },
                        &request_ctx,
                        now,
                    )
                    .await?;
                }
                let _ = &st;
                Ok(json!({ "sessionId": session_id, "status": "REVOKED" }))
            })
        })
        .await
        .map_err(Error::from)
        .inspect(|_| state.realtime.outbox_wakeup.notify_one())
}

/// Revokes every ACTIVE session of a device (device revocation; same transaction).
pub async fn revoke_device_sessions<C: sea_orm::ConnectionTrait>(
    db: &C,
    device_id: Uuid,
    now: DateTime<Utc>,
) -> Result<u64> {
    Ok(db::exec(
        db,
        "UPDATE messaging_client_sessions SET status = 'REVOKED', revoked_at = $2, revoked_reason = 'DEVICE_REVOKED' \
         WHERE device_id = $1 AND status = 'ACTIVE'",
        vec![device_id.into(), now.into()],
    )
    .await?)
}

/// Removes expired tickets and long-expired sessions (bounded batches).
pub async fn cleanup_expired(state: &AppState) -> Result<u64> {
    let now = state.now();
    let tickets = db::exec(
        &state.db,
        "DELETE FROM messaging_realtime_tickets WHERE id IN (SELECT id FROM messaging_realtime_tickets WHERE expires_at < $1 LIMIT 5000)",
        vec![(now - chrono::Duration::minutes(5)).into()],
    )
    .await?;
    Ok(tickets)
}

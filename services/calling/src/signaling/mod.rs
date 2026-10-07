//! Realtime call signaling (Part C §8–§10, §87, §88).
//!
//! * [`credentials`] — short-lived, separately keyed Call Signaling Credentials minted for
//!   one endpoint of one principal;
//! * [`bus`] — the provider-neutral [`RealtimeSignalBus`] (in-memory and cross-instance
//!   PostgreSQL LISTEN / NOTIFY);
//! * [`signals`] — the realtime signal and push-ready payload contracts;
//! * [`push`] — the transport-neutral push hook for ring offers (no vendor integrated);
//! * [`ws`] — the WebSocket channel `GET /v1/calling/signaling`;
//! * [`http`] — credential minting (API credential) and the endpoint's own operations
//!   (signaling credential): invitation recovery, accept / decline, media credentials.
//!
//! PostgreSQL stays authoritative: signals are sent after commit, a lost signal never
//! loses state, and a reconnecting endpoint recovers from `GET /v1/calling/invitations`
//! and the Call API.

pub mod bus;
pub mod credentials;
pub mod http;
pub mod push;
pub mod signals;
pub mod ws;

use std::sync::Arc;

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts, HeaderMap},
};
use cf_service_auth::{CorrelationId, RequestId};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

pub use bus::{InMemorySignalBus, PgNotifySignalBus, RealtimeSignalBus, SignalEnvelope};
pub use credentials::{SignalingClaims, SignalingKeys};
pub use push::{NoPushTransport, PushDispatcher};

use crate::{
    calls::command::CallScope,
    domain::{
        CredentialId, CredentialStatus, PrincipalId, RequestContext, ServiceAccountId, SharedClock,
    },
    endpoints::CallEndpointId,
    error::CallingError,
    platform::{credentials::CredentialEffectiveState, scopes::names},
    state::AppState,
};

/// A verified signaling identity: a valid credential whose endpoint, principal and
/// minting authority are still valid in the database.
#[derive(Debug, Clone)]
pub struct SignalingIdentity {
    claims: SignalingClaims,
    request: RequestContext,
}

impl SignalingIdentity {
    pub fn scope(&self) -> CallScope {
        self.claims.scope
    }

    pub fn principal_id(&self) -> PrincipalId {
        self.claims.principal_id
    }

    pub fn endpoint_id(&self) -> CallEndpointId {
        self.claims.endpoint_id
    }

    pub fn minted_by_service_account_id(&self) -> ServiceAccountId {
        self.claims.minted_by_service_account_id
    }

    pub fn minted_by_credential_id(&self) -> CredentialId {
        self.claims.minted_by_credential_id
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.claims.expires_at
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request.request_id
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.request.correlation_id
    }
}

/// Verifies signaling credentials against the key and the persisted authority chain.
#[derive(Clone)]
pub struct SignalingAuthority {
    keys: Arc<SignalingKeys>,
    db: DatabaseConnection,
    clock: SharedClock,
}

/// The signaling credential's authority, re-checked on every use: endpoint ACTIVE,
/// principal ACTIVE, minting credential holding `calls.write`, and its service account,
/// environment, application and tenant ACTIVE — all in the credential's scope.
const AUTHORITY_SQL: &str = r#"
SELECT c.status AS credential_status, c.not_before, c.expires_at, c.supersession_valid_until
  FROM call_endpoints ep
  JOIN platform_principals p
    ON p.id = ep.principal_id AND p.tenant_id = ep.tenant_id
   AND p.application_id = ep.application_id AND p.environment_id = ep.environment_id
  JOIN platform_api_credentials c
    ON c.id = $5 AND c.tenant_id = ep.tenant_id AND c.application_id = ep.application_id
   AND c.environment_id = ep.environment_id AND c.service_account_id = $6
  JOIN platform_service_accounts sa ON sa.id = c.service_account_id
  JOIN platform_environments e ON e.id = ep.environment_id
  JOIN platform_applications a ON a.id = ep.application_id
  JOIN platform_tenants t ON t.id = ep.tenant_id
 WHERE ep.id = $1 AND ep.tenant_id = $2 AND ep.application_id = $3 AND ep.environment_id = $4
   AND ep.principal_id = $7 AND ep.status = 'ACTIVE' AND p.status = 'ACTIVE'
   AND sa.status = 'ACTIVE' AND e.status = 'ACTIVE' AND a.status = 'ACTIVE' AND t.status = 'ACTIVE'
   AND EXISTS (
       SELECT 1 FROM platform_api_credential_scopes cs
         JOIN platform_application_scopes aps
           ON aps.application_id = cs.application_id AND aps.scope = cs.scope
        WHERE cs.credential_id = c.id AND cs.scope = $8)
"#;

impl SignalingAuthority {
    pub fn new(keys: Arc<SignalingKeys>, db: DatabaseConnection, clock: SharedClock) -> Self {
        Self { keys, db, clock }
    }

    pub fn keys(&self) -> &SignalingKeys {
        &self.keys
    }

    /// Authenticates `token`. Every failure is the same AUTHENTICATION_FAILED.
    pub async fn authenticate(
        &self,
        token: &str,
        request: RequestContext,
    ) -> Result<SignalingIdentity, CallingError> {
        let now = self.clock.now();
        let claims = self.keys.verify(token, now).map_err(|_| {
            tracing::info!("signaling authentication rejected (credential)");
            CallingError::AuthenticationFailed
        })?;
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                AUTHORITY_SQL,
                vec![
                    claims.endpoint_id.as_uuid().into(),
                    claims.scope.tenant_id().as_uuid().into(),
                    claims.scope.application_id().as_uuid().into(),
                    claims.scope.environment_id().as_uuid().into(),
                    claims.minted_by_credential_id.as_uuid().into(),
                    claims.minted_by_service_account_id.as_uuid().into(),
                    claims.principal_id.as_uuid().into(),
                    names::CALLS_WRITE.into(),
                ],
            ))
            .await
            .map_err(|_| CallingError::ServiceUnavailable)?
            .ok_or_else(|| {
                tracing::info!("signaling authentication rejected (authority)");
                CallingError::AuthenticationFailed
            })?;
        let status: String = row
            .try_get("", "credential_status")
            .map_err(|_| CallingError::Internal)?;
        let not_before: DateTime<Utc> = row
            .try_get("", "not_before")
            .map_err(|_| CallingError::Internal)?;
        let expires_at: DateTime<Utc> = row
            .try_get("", "expires_at")
            .map_err(|_| CallingError::Internal)?;
        let overlap: Option<DateTime<Utc>> = row
            .try_get("", "supersession_valid_until")
            .map_err(|_| CallingError::Internal)?;
        let status = CredentialStatus::parse(&status).ok_or(CallingError::Internal)?;
        match CredentialEffectiveState::evaluate(status, not_before, expires_at, overlap, now) {
            CredentialEffectiveState::Active | CredentialEffectiveState::RotationOverlap => {}
            _ => {
                tracing::info!("signaling authentication rejected (minting credential)");
                return Err(CallingError::AuthenticationFailed);
            }
        }
        Ok(SignalingIdentity { claims, request })
    }
}

/// `Authorization: Bearer cfsig_…` (platform API credentials are never accepted here).
pub(crate) fn bearer(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > 2_100 {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && token.trim().starts_with(credentials::TOKEN_PREFIX))
        .then(|| token.trim().to_string())
}

#[axum::async_trait]
impl FromRequestParts<AppState> for SignalingIdentity {
    type Rejection = CallingError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // A request carrying an internal service token never reaches endpoint routes.
        if parts.headers.contains_key("x-cf-service-authorization") {
            return Err(CallingError::AuthenticationFailed);
        }
        let token = bearer(&parts.headers).ok_or(CallingError::AuthenticationFailed)?;
        let request = parts
            .extensions
            .get::<RequestContext>()
            .cloned()
            .unwrap_or_else(RequestContext::generate);
        state.signaling.authenticate(&token, request).await
    }
}

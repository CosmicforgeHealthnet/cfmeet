//! Media connection gateway: CF Meet's admission authority over every media signaling
//! connection (Part C §22, §24, §44; ADR 0007).
//!
//! A self-hosted media server cannot revoke an access token it has issued or refreshed.
//! So no token a client holds is ever accepted by the media server itself: clients hold
//! credentials sealed by Calling's media edge and connect to this gateway, which on EVERY
//! fresh connection and reconnection
//!
//! 1. authenticates the credential (edge signature, expiry, resource, identity,
//!    generation);
//! 2. asks Calling's authoritative state ([`ConnectionAdmission`]) whether that exact
//!    session generation may hold media now — and refuses BEFORE the media server is
//!    contacted otherwise;
//! 3. only then opens the upstream connection to the media server with a short-lived,
//!    join-only token minted for this one connection, and relays it.
//!
//! The decision fails closed: an unverifiable credential, an unknown identity, a database
//! or Calling error, a stale generation or a revoked session are all refusals. The
//! protocol-specific relay (frame filtering, token re-sealing) is the adapter's
//! (`livekit_gateway`); this module is provider-neutral.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, WebSocketUpgrade},
        RawQuery, State,
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::{error::CallingError, state::AppState};

/// The signaling connection a presented media credential claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionClaims {
    /// The Call's canonical media resource name.
    pub resource_name: String,
    /// The session's server-generated media identity.
    pub identity: String,
    /// The session media generation the credential was issued for.
    pub generation: Option<i32>,
}

/// Why a connection is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDenied {
    /// Missing, malformed, forged or expired credential.
    InvalidCredential,
    /// No session has this media identity.
    UnknownIdentity,
    /// The identity belongs to a session of another Call / resource.
    ForeignResource,
    /// The session's media access is revoked / ended / removed, its participant left, or
    /// the Call no longer permits media.
    SessionRevoked,
    /// The credential's generation is not the session's current generation.
    StaleGeneration,
    /// Authoritative state could not be read: refuse (fail closed).
    Unavailable,
}

impl ConnectionDenied {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionDenied::InvalidCredential => "INVALID_CREDENTIAL",
            ConnectionDenied::UnknownIdentity => "UNKNOWN_IDENTITY",
            ConnectionDenied::ForeignResource => "FOREIGN_RESOURCE",
            ConnectionDenied::SessionRevoked => "SESSION_REVOKED",
            ConnectionDenied::StaleGeneration => "STALE_GENERATION",
            ConnectionDenied::Unavailable => "UNAVAILABLE",
        }
    }
}

/// Calling's authoritative admission decision (implemented by the application-calling
/// service against the database; never cached).
#[async_trait]
pub trait ConnectionAdmission: Send + Sync + 'static {
    /// Whether this exact session generation may hold a media connection now.
    async fn admit_connection(&self, claims: &ConnectionClaims) -> Result<(), ConnectionDenied>;

    /// The media server accepted an admitted (re)connection: apply the session's
    /// server-derived permissions now (join-then-admit, also after a resume).
    async fn connection_established(&self, claims: &ConnectionClaims);
}

/// Which signaling endpoint of the media SDK a request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayPath {
    /// The original signaling path.
    Legacy,
    /// The versioned signaling path.
    V1,
}

/// One gateway request as received from a client.
pub struct GatewayRequest {
    pub path: GatewayPath,
    /// Only check admission (the SDK's connection diagnostics): never relay.
    pub validate_only: bool,
    pub headers: HeaderMap,
    pub query: Option<String>,
    pub upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
}

/// A provider's media connection gateway.
#[async_trait]
pub trait MediaConnectionGateway: Send + Sync + 'static {
    /// Authenticates, admits (via `admission`) and relays one signaling connection, or
    /// refuses it before the media server is contacted.
    async fn handle(
        &self,
        request: GatewayRequest,
        admission: Arc<dyn ConnectionAdmission>,
    ) -> Response;
}

/// Gateway routes. Mounted always; without a configured gateway they answer 404.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calling/media/rtc", get(legacy))
        .route("/v1/calling/media/rtc/v1", get(v1))
        .route("/v1/calling/media/rtc/validate", get(legacy_validate))
        .route("/v1/calling/media/rtc/v1/validate", get(v1_validate))
}

async fn dispatch(
    state: AppState,
    path: GatewayPath,
    validate_only: bool,
    headers: HeaderMap,
    query: Option<String>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Some(gateway) = state.media_gateway.clone() else {
        return CallingError::NotFound.into_response();
    };
    // A request carrying an internal service token never reaches media routes.
    if headers.contains_key("x-cf-service-authorization") {
        return CallingError::AuthenticationFailed.into_response();
    }
    let admission: Arc<dyn ConnectionAdmission> = state.app.clone();
    gateway
        .handle(
            GatewayRequest {
                path,
                validate_only,
                headers,
                query,
                upgrade,
            },
            admission,
        )
        .await
}

async fn legacy(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    dispatch(state, GatewayPath::Legacy, false, headers, query, upgrade).await
}

async fn v1(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    dispatch(state, GatewayPath::V1, false, headers, query, upgrade).await
}

async fn legacy_validate(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    dispatch(state, GatewayPath::Legacy, true, headers, query, upgrade).await
}

async fn v1_validate(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    dispatch(state, GatewayPath::V1, true, headers, query, upgrade).await
}

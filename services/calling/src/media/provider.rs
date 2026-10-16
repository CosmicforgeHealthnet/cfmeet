//! The Calling media-provider contract (Part C §24).
//!
//! Business code depends only on [`CallMediaProvider`] and the provider-neutral types in
//! this module. An adapter (today: the self-hosted LiveKit adapter in `livekit.rs`)
//! implements it; nothing outside the adapter names a provider, branches on a provider or
//! sees a provider type. Capabilities are generic flags ([`MediaProviderCapabilities`]);
//! business code asks "can this provider update permissions?", never "is this LiveKit?".
//!
//! The provider never decides anything: who may join, with which identity and which
//! permissions is decided by Calling from its own database. Provider callbacks and
//! listings are operational evidence that Calling verifies against that state.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use super::permissions::MediaPermissions;
use crate::calls::{ids::CallId, kinds::MediaEventKind};

/// Generic capability flags of a media provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProviderCapabilities {
    /// Create / ensure and terminate a call-specific media resource.
    pub session_control: bool,
    /// Issue short-lived join credentials.
    pub credential_issuance: bool,
    /// Remove one participant connection.
    pub participant_removal: bool,
    /// Change a connected participant's media permissions server-side.
    pub permission_update: bool,
    /// List the participants of a resource (reconciliation).
    pub participant_listing: bool,
    /// Authenticated callbacks.
    pub verified_callbacks: bool,
    /// Bounded quality metrics observed by the provider.
    pub quality_metrics: bool,
}

impl MediaProviderCapabilities {
    /// What application RTC calling needs at minimum.
    /// Application calls are join-then-admit: join credentials carry no media right, so
    /// the provider must also apply permissions server-side (admission, hold, resume).
    pub fn supports_app_calls(&self) -> bool {
        self.session_control
            && self.credential_issuance
            && self.participant_removal
            && self.permission_update
            && self.participant_listing
    }
}

/// Canonical, provider-neutral name of a Call's media resource: `cfc-` followed by the
/// Call id without hyphens. Deterministic (retries never create a second resource),
/// distinct from every Meeting resource namespace and never chosen by a client.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MediaResourceName(String);

impl MediaResourceName {
    pub const PREFIX: &'static str = "cfc-";

    pub fn for_call(call_id: CallId) -> Self {
        Self(format!("{}{}", Self::PREFIX, call_id.as_uuid().simple()))
    }

    /// The Call a canonical resource name belongs to (`None` for any foreign name).
    pub fn call_id(name: &str) -> Option<CallId> {
        let hex = name.strip_prefix(Self::PREFIX)?;
        if hex.len() != 32 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        uuid::Uuid::parse_str(hex).ok().map(CallId::from_uuid)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Call's media resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSessionRef {
    pub name: MediaResourceName,
}

/// Server policy for a media resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaSessionSpec {
    pub max_participants: u32,
    /// How long an empty resource survives before the provider may close it.
    pub empty_timeout: Duration,
}

/// One participant connection of a media resource, identified by the session's
/// server-generated media identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaParticipantRef {
    pub session: MediaSessionRef,
    pub identity: String,
}

/// A request to mint a join credential for one session generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCredentialRequest {
    pub session: MediaSessionRef,
    /// The session's media identity (`cfmid_…`).
    pub identity: String,
    /// The session's media generation this credential belongs to.
    pub generation: i32,
    pub ttl: Duration,
}

/// A short-lived join credential. The token is redacted from `Debug` output.
pub struct IssuedMediaCredential {
    /// Client connection URL of the media service.
    pub url: String,
    pub token: MediaToken,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for IssuedMediaCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedMediaCredential")
            .field("url", &self.url)
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A client join token: returned once in the credential response, never stored or logged.
pub struct MediaToken(secrecy::SecretString);

impl MediaToken {
    pub fn new(value: String) -> Self {
        Self(secrecy::SecretString::from(value))
    }

    pub fn expose(&self) -> &str {
        secrecy::ExposeSecret::expose_secret(&self.0)
    }
}

impl fmt::Debug for MediaToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaToken([REDACTED])")
    }
}

/// What a provider reports about one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderParticipant {
    /// The connection's identity as the provider reports it (expected `cfmid_…`; anything
    /// else is unknown and never attached to a session).
    pub identity: String,
    /// The session generation the connection's credential was minted for, if present.
    pub generation: Option<i32>,
    /// Permissions currently applied to the connection.
    pub permissions: MediaPermissions,
    /// The connection is fully established (not merely joining).
    pub active: bool,
    pub published_audio: bool,
    pub published_video: bool,
    pub joined_at: Option<DateTime<Utc>>,
}

/// A resource as the provider currently sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionSnapshot {
    pub exists: bool,
    pub participants: Vec<ProviderParticipant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsuredMediaSession {
    /// Provider-internal reference of the resource (kept in the private binding only).
    pub resource_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalOutcome {
    Removed,
    /// The connection was not (or no longer) present: removal is already in effect.
    NotPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOutcome {
    Applied,
    /// No such connection: nothing to apply (it gets the permissions on admission).
    NotPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationOutcome {
    Terminated,
    AlreadyAbsent,
}

/// Neutral provider failure. Detail stays inside the adapter's logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MediaProviderError {
    #[error("media provider timed out")]
    Timeout,
    #[error("media provider unavailable")]
    Unavailable,
    #[error("media provider rejected the request")]
    Rejected,
    #[error("media provider resource not found")]
    NotFound,
    #[error("media provider capability not supported")]
    Unsupported,
}

impl MediaProviderError {
    pub fn failure(&self) -> crate::calls::kinds::MediaFailure {
        use crate::calls::kinds::MediaFailure;
        match self {
            MediaProviderError::Timeout => MediaFailure::Timeout,
            MediaProviderError::Unavailable => MediaFailure::Unavailable,
            MediaProviderError::Rejected | MediaProviderError::Unsupported => {
                MediaFailure::Rejected
            }
            MediaProviderError::NotFound => MediaFailure::NotFound,
        }
    }
}

/// An authenticated, normalized provider callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedMediaEvent {
    /// The provider's event id (dedupe key; never public).
    pub event_id: String,
    pub kind: MediaEventKind,
    /// Name of the resource the event concerns.
    pub resource_name: Option<String>,
    pub participant: Option<ProviderParticipant>,
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EventVerificationError {
    #[error("callback signature missing")]
    MissingSignature,
    #[error("callback signature invalid")]
    InvalidSignature,
    #[error("callback body malformed")]
    Malformed,
}

/// The Calling media-provider contract.
#[async_trait]
pub trait CallMediaProvider: Send + Sync + 'static {
    /// Adapter identity for Calling's private binding table (never public).
    fn adapter_name(&self) -> &'static str;

    fn capabilities(&self) -> MediaProviderCapabilities;

    /// Creates the resource, or returns the existing one with the same name (idempotent).
    async fn create_or_ensure_session(
        &self,
        session: &MediaSessionRef,
        spec: &MediaSessionSpec,
    ) -> Result<EnsuredMediaSession, MediaProviderError>;

    /// Mints a join-only credential for one session generation: it lets the connection
    /// join exactly this resource under exactly this identity and grants no media
    /// permission and no administrative capability.
    async fn issue_credentials(
        &self,
        request: &MediaCredentialRequest,
    ) -> Result<IssuedMediaCredential, MediaProviderError>;

    /// Disconnects one participant connection.
    async fn remove_session(
        &self,
        participant: &MediaParticipantRef,
    ) -> Result<RemovalOutcome, MediaProviderError>;

    /// Applies server-authorized permissions to one connection.
    async fn update_permissions(
        &self,
        participant: &MediaParticipantRef,
        permissions: MediaPermissions,
    ) -> Result<PermissionOutcome, MediaProviderError>;

    /// Closes the resource and disconnects everyone (idempotent).
    async fn terminate_session(
        &self,
        session: &MediaSessionRef,
    ) -> Result<TerminationOutcome, MediaProviderError>;

    /// Current connections of the resource; empty if the resource does not exist.
    async fn list_participants(
        &self,
        session: &MediaSessionRef,
    ) -> Result<Vec<ProviderParticipant>, MediaProviderError>;

    /// The resource as the provider sees it, for reconciliation.
    async fn reconcile(
        &self,
        session: &MediaSessionRef,
    ) -> Result<ProviderSessionSnapshot, MediaProviderError>;

    /// Authenticates and normalizes a callback (`authorization` header value, raw body).
    fn verify_event(
        &self,
        authorization: Option<&str>,
        body: &[u8],
    ) -> Result<VerifiedMediaEvent, EventVerificationError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_names_are_derived_from_the_call_and_reversible() {
        let call = CallId::from_uuid(
            uuid::Uuid::parse_str("0b6f1c4e-3a6d-4f1e-9a49-6f0e3f2d1c7a").unwrap(),
        );
        let name = MediaResourceName::for_call(call);
        assert_eq!(name.as_str(), "cfc-0b6f1c4e3a6d4f1e9a496f0e3f2d1c7a");
        assert_eq!(MediaResourceName::call_id(name.as_str()), Some(call));
        // The Call id itself is never the resource name.
        assert_ne!(name.as_str(), call.as_uuid().to_string());
        for foreign in [
            "",
            "cfc-",
            "meeting-0b6f1c4e3a6d4f1e9a496f0e3f2d1c7a",
            "cfc-0B6F1C4E3A6D4F1E9A496F0E3F2D1C7A",
            "cfc-0b6f1c4e-3a6d-4f1e-9a49-6f0e3f2d1c7a",
            "cfc-0b6f1c4e3a6d4f1e9a496f0e3f2d1c7a0",
        ] {
            assert!(MediaResourceName::call_id(foreign).is_none(), "{foreign}");
        }
    }

    #[test]
    fn tokens_never_appear_in_debug_output() {
        let credential = IssuedMediaCredential {
            url: "wss://media.example".into(),
            token: MediaToken::new("eyJhbGciOi.secret.part".into()),
            expires_at: Utc::now(),
        };
        let debug = format!("{credential:?}");
        assert!(!debug.contains("secret.part"));
        assert_eq!(credential.token.expose(), "eyJhbGciOi.secret.part");
    }
}

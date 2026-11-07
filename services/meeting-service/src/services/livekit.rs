//! LiveKit adapter for the legacy `/api/v1` meeting API.
//!
//! Provider-specific implementation of [`LegacyRtcCredentials`]; handlers only see the
//! provider-neutral trait. The Phase 2 domain uses [`crate::services::rtc_provider`].

use livekit_api::access_token::{AccessToken, VideoGrants};
use std::time::Duration;

use crate::config::LiveKitConfig;
use crate::error::{ApiError, ApiResult};
use crate::services::rtc_provider::LegacyRtcCredentials;

/// SEC-009: legacy join credential lifetime (`LEGACY_RTC_TOKEN_TTL_SECS`, default 15 min,
/// bounded to 1 min - 6 h). A connected participant keeps its session (LiveKit refreshes
/// tokens of connected participants); the lifetime only bounds how long a removed
/// participant could reconnect with a credential it already holds.
pub const DEFAULT_LEGACY_TOKEN_TTL_SECS: u64 = 15 * 60;
const MIN_LEGACY_TOKEN_TTL_SECS: u64 = 60;
const MAX_LEGACY_TOKEN_TTL_SECS: u64 = 6 * 60 * 60;

/// The configured legacy credential lifetime.
pub fn legacy_token_ttl_from_env() -> Duration {
    let secs = crate::config::env_num("LEGACY_RTC_TOKEN_TTL_SECS", DEFAULT_LEGACY_TOKEN_TTL_SECS)
        .clamp(MIN_LEGACY_TOKEN_TTL_SECS, MAX_LEGACY_TOKEN_TTL_SECS);
    Duration::from_secs(secs)
}

/// LiveKit service for token generation and room management
#[derive(Clone)]
pub struct LiveKitService {
    api_key: String,
    api_secret: String,
    url: String,
    token_ttl: Duration,
}

impl LiveKitService {
    /// Create a new LiveKit service instance
    pub fn new(config: &LiveKitConfig) -> Self {
        Self {
            api_key: config.api_key.clone(),
            api_secret: config.api_secret.clone(),
            url: config.url.clone(),
            token_ttl: legacy_token_ttl_from_env(),
        }
    }

    /// Explicit credential lifetime (tests).
    pub fn with_token_ttl(mut self, ttl: Duration) -> Self {
        self.token_ttl = ttl;
        self
    }

    /// Get the LiveKit server URL
    pub fn get_url(&self) -> &str {
        &self.url
    }

    /// Generate a join token for a participant
    ///
    /// # Arguments
    /// * `room_name` - The meeting/room identifier
    /// * `participant_identity` - Unique identifier for the participant (usually participant_id)
    /// * `participant_name` - Display name for the participant
    /// * `is_host` - Whether this participant is the host. SEC-009: hosts get the same
    ///   client grants as everyone else; host controls (mute, kick) are server-side API
    ///   calls, so no client credential carries `room_admin`.
    pub fn generate_join_token(
        &self,
        room_name: &str,
        participant_identity: &str,
        participant_name: &str,
        is_host: bool,
    ) -> ApiResult<String> {
        let mut grants = VideoGrants::default();
        grants.room_join = true;
        grants.room = room_name.to_string();
        grants.can_publish = true;
        grants.can_subscribe = true;
        grants.can_publish_data = true;

        // No client credential is an admin or recorder credential (same rule as the v2
        // `LiveKitProvider::grants_for`): `room_admin` would let a client mute or remove
        // anyone outside the server's authorization and audit, and `room_record` would
        // bypass the Part G recording authority. Host controls are server-side calls.
        let _ = is_host;
        grants.room_admin = false;
        grants.room_record = false;
        grants.room_create = false;
        grants.room_list = false;

        let token = AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_identity(participant_identity)
            .with_name(participant_name)
            .with_grants(grants)
            .with_ttl(self.token_ttl)
            .to_jwt()
            .map_err(|e| {
                ApiError::InternalError(format!("Failed to generate LiveKit token: {}", e))
            })?;

        Ok(token)
    }

    /// Generate a token for creating/managing a room (used internally)
    pub fn generate_room_service_token(&self, room_name: &str) -> ApiResult<String> {
        let mut grants = VideoGrants::default();
        grants.room_create = true;
        grants.room_list = true;
        grants.room_admin = true;
        grants.room = room_name.to_string();

        let token = AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_identity("server")
            .with_grants(grants)
            .with_ttl(Duration::from_secs(60)) // Short TTL for service tokens
            .to_jwt()
            .map_err(|e| {
                ApiError::InternalError(format!("Failed to generate room service token: {}", e))
            })?;

        Ok(token)
    }
}

impl LegacyRtcCredentials for LiveKitService {
    fn issue_legacy_join_credential(
        &self,
        room_name: &str,
        participant_identity: &str,
        participant_name: &str,
        is_host: bool,
    ) -> ApiResult<String> {
        self.generate_join_token(room_name, participant_identity, participant_name, is_host)
    }

    fn media_url(&self) -> &str {
        self.get_url()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_host_token_never_carries_recording_grant() {
        let svc = LiveKitService::new(&LiveKitConfig {
            url: "wss://rtc.test".into(),
            api_key: "key".into(),
            api_secret: "secret-secret-secret-secret-secret".into(),
        });
        let token = svc
            .issue_legacy_join_credential("ROOM2345", "p1", "Host", true)
            .unwrap();
        let claims = livekit_api::access_token::TokenVerifier::with_api_key(
            "key",
            "secret-secret-secret-secret-secret",
        )
        .verify(&token)
        .unwrap();
        assert!(claims.video.room_join);
        assert!(
            !claims.video.room_admin,
            "SEC-009: a legacy host credential never carries room_admin"
        );
        assert!(
            !claims.video.room_record,
            "legacy host must not hold room_record"
        );
        assert!(!claims.video.room_create);
        assert_eq!(claims.video.room, "ROOM2345");
    }

    #[test]
    fn legacy_credentials_are_short_lived() {
        let svc = LiveKitService::new(&LiveKitConfig {
            url: "wss://rtc.test".into(),
            api_key: "key".into(),
            api_secret: "secret-secret-secret-secret-secret".into(),
        })
        .with_token_ttl(Duration::from_secs(DEFAULT_LEGACY_TOKEN_TTL_SECS));
        let token = svc
            .issue_legacy_join_credential("ROOM2345", "p1", "Guest", false)
            .unwrap();
        let claims = livekit_api::access_token::TokenVerifier::with_api_key(
            "key",
            "secret-secret-secret-secret-secret",
        )
        .verify(&token)
        .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize;
        assert!(claims.exp <= now + DEFAULT_LEGACY_TOKEN_TTL_SECS as usize + 5);
        assert!(!claims.video.room_admin);
    }
}

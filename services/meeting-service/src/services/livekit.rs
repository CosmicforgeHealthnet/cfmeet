//! LiveKit adapter for the legacy `/api/v1` meeting API.
//!
//! Provider-specific implementation of [`LegacyRtcCredentials`]; handlers only see the
//! provider-neutral trait. The Phase 2 domain uses [`crate::services::rtc_provider`].

use livekit_api::access_token::{AccessToken, VideoGrants};
use std::time::Duration;

use crate::config::LiveKitConfig;
use crate::error::{ApiError, ApiResult};
use crate::services::rtc_provider::LegacyRtcCredentials;

/// Token validity duration (default: 6 hours)
const TOKEN_TTL_SECS: u64 = 6 * 60 * 60;

/// LiveKit service for token generation and room management
#[derive(Clone)]
pub struct LiveKitService {
    api_key: String,
    api_secret: String,
    url: String,
}

impl LiveKitService {
    /// Create a new LiveKit service instance
    pub fn new(config: &LiveKitConfig) -> Self {
        Self {
            api_key: config.api_key.clone(),
            api_secret: config.api_secret.clone(),
            url: config.url.clone(),
        }
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
    /// * `is_host` - Whether this participant is the host (grants additional permissions)
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

        // Host-specific permissions. `room_record` is deliberately never granted: a
        // client-held recording grant would bypass the Part G recording authority
        // (permission, profile and consent checks). Recording is server-initiated only.
        if is_host {
            grants.room_admin = true;
        }

        let token = AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_identity(participant_identity)
            .with_name(participant_name)
            .with_grants(grants)
            .with_ttl(Duration::from_secs(TOKEN_TTL_SECS))
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
        assert!(claims.video.room_join && claims.video.room_admin);
        assert!(
            !claims.video.room_record,
            "legacy host must not hold room_record"
        );
        assert!(!claims.video.room_create);
        assert_eq!(claims.video.room, "ROOM2345");
    }
}

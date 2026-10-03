//! LiveKit Service
//!
//! Service for generating LiveKit access tokens and managing rooms.

use livekit_api::access_token::{AccessToken, VideoGrants};
use std::time::Duration;

use crate::config::LiveKitConfig;
use crate::error::{ApiError, ApiResult};

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

        // Host-specific permissions
        if is_host {
            grants.room_admin = true;
            grants.room_record = true;
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

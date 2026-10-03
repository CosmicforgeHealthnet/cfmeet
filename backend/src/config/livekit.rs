//! LiveKit Configuration
//!
//! Configuration for LiveKit RTC server integration.

use crate::error::{ApiError, ApiResult};

/// LiveKit configuration
#[derive(Clone)]
pub struct LiveKitConfig {
    /// LiveKit server URL (e.g., wss://your-livekit-server.com)
    pub url: String,

    /// LiveKit API key
    pub api_key: String,

    /// LiveKit API secret
    pub api_secret: String,
}

impl LiveKitConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> ApiResult<Self> {
        Ok(Self {
            url: std::env::var("LIVEKIT_URL")
                .map_err(|_| ApiError::InternalError("LIVEKIT_URL must be set".to_string()))?,

            api_key: std::env::var("LIVEKIT_API_KEY")
                .map_err(|_| ApiError::InternalError("LIVEKIT_API_KEY must be set".to_string()))?,

            api_secret: std::env::var("LIVEKIT_API_SECRET").map_err(|_| {
                ApiError::InternalError("LIVEKIT_API_SECRET must be set".to_string())
            })?,
        })
    }
}

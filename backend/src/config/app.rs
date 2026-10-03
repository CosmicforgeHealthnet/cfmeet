//! Application Configuration
//!
//! Core application settings loaded from environment variables.

use crate::error::{ApiError, ApiResult};

/// Application configuration
#[derive(Clone)]
pub struct AppConfig {
    /// Database connection URL
    pub database_url: String,

    /// JWT signing secret
    pub jwt_secret: String,

    /// Server host address
    pub host: String,

    /// Server port
    pub port: u16,

    /// Frontend URLs for CORS (comma-separated in env)
    pub frontend_urls: Vec<String>,

    /// Frontend/Hosted UI URL (e.g., https://meet.cosmicforge.com)
    pub app_url: String,

    /// Google OAuth Client ID
    pub google_client_id: String,

    /// Google OAuth Client Secret
    pub google_client_secret: String,

    /// Google OAuth Redirect URL
    pub google_redirect_url: String,

    /// Require email verification before login (default: true)
    pub require_email_verification: bool,
}

impl AppConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> ApiResult<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL")
                .map_err(|_| ApiError::InternalError("DATABASE_URL must be set".to_string()))?,

            jwt_secret: std::env::var("JWT_SECRET")
                .unwrap_or_else(|_| "your-secret-key-change-this-in-production".to_string()),

            host: std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),

            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .map_err(|_| ApiError::InternalError("Invalid PORT".to_string()))?,

            frontend_urls: std::env::var("FRONTEND_URLS")
                .unwrap_or_else(|_| "http://localhost:3000".to_string())
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect(),

            app_url: std::env::var("APP_URL")
                .unwrap_or_else(|_| "http://localhost:3000".to_string())
                .trim_end_matches('/')
                .to_string(),

            google_client_id: std::env::var("GOOGLE_CLIENT_ID")
                .map_err(|_| ApiError::InternalError("GOOGLE_CLIENT_ID must be set".to_string()))?,

            google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET")
                .map_err(|_| ApiError::InternalError("GOOGLE_CLIENT_SECRET must be set".to_string()))?,

            google_redirect_url: std::env::var("GOOGLE_REDIRECT_URL")
                .map_err(|_| ApiError::InternalError("GOOGLE_REDIRECT_URL must be set".to_string()))?,

            require_email_verification: std::env::var("EMAIL_VERIFICATION_REQUIRED")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
        })
    }

    /// Get the server address (host:port)
    pub fn server_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Generate join URL for a meeting
    pub fn join_url(&self, meeting_identifier: &str) -> String {
        format!("{}/join/{}", self.app_url, meeting_identifier)
    }
}

use sea_orm::DatabaseConnection;
use std::sync::Arc;

use crate::config::{AppConfig, EmailConfig, LiveKitConfig};
use crate::services::auth::JwtService;
use crate::services::email::EmailService;
use crate::services::livekit::LiveKitService;
use crate::services::{OtpService, RateLimiterService, RedisService, TokenBlacklistService};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub jwt_service: Arc<JwtService>,
    pub email_service: Arc<EmailService>,
    pub livekit_service: Arc<LiveKitService>,
    pub redis: Arc<RedisService>,
    pub otp_service: Arc<OtpService>,
    pub token_blacklist: Arc<TokenBlacklistService>,
    pub rate_limiter: Arc<RateLimiterService>,
    /// Frontend/Hosted UI URL for generating join links
    pub app_url: String,
    /// Google OAuth Client ID
    pub google_client_id: String,
    /// Google OAuth Client Secret
    pub google_client_secret: String,
    /// Google OAuth Redirect URL
    pub google_redirect_url: String,
    /// Require email verification before login
    pub require_email_verification: bool,
}

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        redis: RedisService,
        app_config: &AppConfig,
        email_config: &EmailConfig,
        livekit_config: &LiveKitConfig,
    ) -> Self {
        let redis = Arc::new(redis);
        let otp_service = Arc::new(OtpService::new(redis.clone()));
        let token_blacklist = Arc::new(TokenBlacklistService::new(redis.clone()));
        let rate_limiter = Arc::new(RateLimiterService::new(redis.clone()));

        Self {
            db,
            jwt_service: Arc::new(JwtService::new(app_config.jwt_secret.clone())),
            email_service: Arc::new(EmailService::new(redis.clone(), email_config)),
            livekit_service: Arc::new(LiveKitService::new(livekit_config)),
            redis,
            otp_service,
            token_blacklist,
            rate_limiter,
            app_url: app_config.app_url.clone(),
            google_client_id: app_config.google_client_id.clone(),
            google_client_secret: app_config.google_client_secret.clone(),
            google_redirect_url: app_config.google_redirect_url.clone(),
            require_email_verification: app_config.require_email_verification,
        }
    }

    /// Generate join URL for a meeting
    pub fn join_url(&self, meeting_identifier: &str) -> String {
        format!("{}/{}", self.app_url, meeting_identifier)
    }
}

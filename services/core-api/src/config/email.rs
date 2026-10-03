use crate::error::{ApiError, ApiResult};

/// Email configuration loaded from environment variables
#[derive(Clone)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_email: String,
    pub from_name: String,
    pub app_url: String,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_username: String::new(),
            smtp_password: String::new(),
            from_email: "noreply@cosmicforge.com".to_string(),
            from_name: "CosmicForge".to_string(),
            app_url: "http://localhost:3000".to_string(),
        }
    }
}

impl EmailConfig {
    /// Create EmailConfig from environment variables
    pub fn from_env() -> ApiResult<Self> {
        Ok(Self {
            smtp_host: std::env::var("SMTP_HOST")
                .map_err(|_| ApiError::InternalError("SMTP_HOST not configured".to_string()))?,
            smtp_port: std::env::var("SMTP_PORT")
                .unwrap_or_else(|_| "587".to_string())
                .parse()
                .map_err(|_| ApiError::InternalError("Invalid SMTP_PORT".to_string()))?,
            smtp_username: std::env::var("SMTP_USERNAME")
                .map_err(|_| ApiError::InternalError("SMTP_USERNAME not configured".to_string()))?,
            smtp_password: std::env::var("SMTP_PASSWORD")
                .map_err(|_| ApiError::InternalError("SMTP_PASSWORD not configured".to_string()))?,
            from_email: std::env::var("SMTP_FROM_EMAIL")
                .unwrap_or_else(|_| "noreply@cosmicforge.com".to_string()),
            from_name: std::env::var("SMTP_FROM_NAME")
                .unwrap_or_else(|_| "CosmicForge".to_string()),
            app_url: std::env::var("APP_URL")
                .unwrap_or_else(|_| "http://localhost:3000".to_string()),
        })
    }
}

//! The live push adapters' configuration (files are read and checked at startup). Moved
//! verbatim from `services/messaging/src/config/mod.rs`; each consuming service parses
//! its own environment variables into these structures.

/// The live push adapters' configuration (files are read and checked at startup).
#[derive(Debug, Clone, Default)]
pub struct PushAdapterConfig {
    pub apns: Option<ApnsConfig>,
    pub fcm: Option<FcmConfig>,
    pub web: Option<WebPushConfig>,
}

#[derive(Debug, Clone)]
pub struct ApnsConfig {
    pub key_file: std::path::PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub topic: String,
    pub base_url: String,
}

#[derive(Debug, Clone)]
pub struct FcmConfig {
    pub service_account_file: std::path::PathBuf,
    pub base_url: String,
}

#[derive(Debug, Clone)]
pub struct WebPushConfig {
    pub vapid_key_file: std::path::PathBuf,
    pub subject: String,
    pub allowed_hosts: Vec<String>,
}

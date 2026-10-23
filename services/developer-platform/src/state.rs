//! Shared application state (cheap to clone).

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags},
    platform_access::{PlatformAccessPort, UnconfiguredPlatformAccess},
    session::{HumanSessions, UnconfiguredSessions},
    webhooks::WebhookService,
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub service_auth: ServiceAuthRuntime,
    pub platform_access: Arc<dyn PlatformAccessPort>,
    pub sessions: Arc<dyn HumanSessions>,
    pub flags: FeatureFlags,
    /// Central customer webhooks (Section 53); `None`: every webhook route is 404.
    pub webhooks: Option<WebhookService>,
}

impl AppState {
    pub fn new(db: DatabaseConnection, service_auth: ServiceAuthRuntime) -> Self {
        Self {
            db,
            service_auth,
            platform_access: Arc::new(UnconfiguredPlatformAccess),
            sessions: Arc::new(UnconfiguredSessions),
            flags: FeatureFlags::default(),
            webhooks: None,
        }
    }

    pub fn with_platform_access(mut self, platform_access: Arc<dyn PlatformAccessPort>) -> Self {
        self.platform_access = platform_access;
        self
    }

    pub fn with_sessions(mut self, sessions: Arc<dyn HumanSessions>) -> Self {
        self.sessions = sessions;
        self
    }

    pub fn with_flags(mut self, flags: FeatureFlags) -> Self {
        self.flags = flags;
        self
    }

    pub fn with_webhooks(mut self, webhooks: WebhookService) -> Self {
        self.webhooks = Some(webhooks);
        self
    }
}

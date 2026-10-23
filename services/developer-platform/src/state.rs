//! Shared application state (cheap to clone).

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags},
    platform_access::{PlatformAccessPort, UnconfiguredPlatformAccess},
    session::{HumanSessions, UnconfiguredSessions},
    webhooks::{
        migration::port::{CallingMigrationPort, UnconfiguredCalling},
        WebhookService,
    },
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
    /// Calling's controlled webhook export (Section 53 migration); unconfigured: every
    /// migration command fails closed.
    pub calling_export: Arc<dyn CallingMigrationPort>,
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
            calling_export: Arc::new(UnconfiguredCalling),
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

    pub fn with_calling_export(mut self, calling: Arc<dyn CallingMigrationPort>) -> Self {
        self.calling_export = calling;
        self
    }

    pub fn with_webhooks(mut self, webhooks: WebhookService) -> Self {
        self.webhooks = Some(webhooks);
        self
    }
}

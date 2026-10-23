//! Shared application state (cheap to clone).

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags},
    platform_access::{PlatformAccessPort, UnconfiguredPlatformAccess},
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub service_auth: ServiceAuthRuntime,
    pub platform_access: Arc<dyn PlatformAccessPort>,
    pub flags: FeatureFlags,
}

impl AppState {
    pub fn new(db: DatabaseConnection, service_auth: ServiceAuthRuntime) -> Self {
        Self {
            db,
            service_auth,
            platform_access: Arc::new(UnconfiguredPlatformAccess),
            flags: FeatureFlags::default(),
        }
    }

    pub fn with_platform_access(mut self, platform_access: Arc<dyn PlatformAccessPort>) -> Self {
        self.platform_access = platform_access;
        self
    }

    pub fn with_flags(mut self, flags: FeatureFlags) -> Self {
        self.flags = flags;
        self
    }
}

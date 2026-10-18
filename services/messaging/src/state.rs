//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_service_auth::RuntimeEnvironment;
use sea_orm::DatabaseConnection;

use crate::{
    config::{service_auth::ServiceAuthRuntime, DeviceSettings, WorkerSettings},
    domain::SharedClock,
    identity::{cache::CachingAuthority, PrincipalDirectory, Unconfigured},
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub clock: SharedClock,
    /// Platform Access credential verification behind the bounded cache.
    pub authority: Arc<CachingAuthority>,
    pub principals: Arc<dyn PrincipalDirectory>,
    pub service_auth: ServiceAuthRuntime,
    pub devices: DeviceSettings,
    pub workers: WorkerSettings,
}

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        authority: Arc<CachingAuthority>,
        service_auth: ServiceAuthRuntime,
    ) -> Self {
        Self {
            db,
            clock,
            authority,
            principals: Arc::new(Unconfigured),
            service_auth,
            devices: DeviceSettings::default(),
            workers: WorkerSettings::default(),
        }
    }

    pub fn with_principals(mut self, principals: Arc<dyn PrincipalDirectory>) -> Self {
        self.principals = principals;
        self
    }

    pub fn with_devices(mut self, devices: DeviceSettings) -> Self {
        self.devices = devices;
        self
    }

    pub fn with_workers(mut self, workers: WorkerSettings) -> Self {
        self.workers = workers;
        self
    }

    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.service_auth.signer().runtime_environment()
    }
}

//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_service_auth::RuntimeEnvironment;
use sea_orm::DatabaseConnection;

use crate::{
    config::{
        service_auth::ServiceAuthRuntime, ControlSettings, TransportSettings, WorkerSettings,
    },
    domain::SharedClock,
    identity::{CustomerAuthority, PrincipalDirectory, UnconfiguredPrincipals},
    numbering::{NumberingPort, UnconfiguredNumbering},
    providers::SharedProvider,
    sms::protection::Protection,
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub clock: SharedClock,
    pub authority: Arc<dyn CustomerAuthority>,
    pub principals: Arc<dyn PrincipalDirectory>,
    pub service_auth: ServiceAuthRuntime,
    pub numbering: Arc<dyn NumberingPort>,
    /// The SMS provider (none: sending is refused, callbacks are unknown).
    pub provider: Option<SharedProvider>,
    pub protection: Protection,
    pub transport: TransportSettings,
    pub controls: ControlSettings,
    pub workers: WorkerSettings,
    /// Identifies this instance in leases.
    pub instance: String,
}

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        authority: Arc<dyn CustomerAuthority>,
        service_auth: ServiceAuthRuntime,
        protection: Protection,
    ) -> Self {
        Self {
            db,
            clock,
            authority,
            principals: Arc::new(UnconfiguredPrincipals),
            service_auth,
            numbering: Arc::new(UnconfiguredNumbering),
            provider: None,
            protection,
            transport: TransportSettings::default(),
            controls: ControlSettings::default(),
            workers: WorkerSettings::default(),
            instance: format!("sms-gateway-{}", std::process::id()),
        }
    }

    pub fn with_numbering(mut self, numbering: Arc<dyn NumberingPort>) -> Self {
        self.numbering = numbering;
        self
    }

    pub fn with_principals(mut self, principals: Arc<dyn PrincipalDirectory>) -> Self {
        self.principals = principals;
        self
    }

    /// Installs the provider (the caller already applied [`crate::providers::guard_runtime`]).
    pub fn with_provider(mut self, provider: SharedProvider) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_transport(mut self, transport: TransportSettings) -> Self {
        self.transport = transport;
        self
    }

    pub fn with_controls(mut self, controls: ControlSettings) -> Self {
        self.controls = controls;
        self
    }

    pub fn with_workers(mut self, workers: WorkerSettings) -> Self {
        self.workers = workers;
        self
    }

    pub fn with_instance(mut self, instance: &str) -> Self {
        self.instance = instance.to_string();
        self
    }

    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.service_auth.signer().runtime_environment()
    }
}

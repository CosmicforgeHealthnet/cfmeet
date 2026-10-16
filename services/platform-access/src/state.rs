//! Shared application state.

use std::sync::Arc;

use crate::{
    config::service_auth::ServiceAuthRuntime,
    identity::{authentication::CredentialAuthenticator, scopes::ScopeRegistry, PlatformService},
};

#[derive(Clone)]
pub struct AppState {
    /// The identity authority (administration, principals, change feed).
    pub platform: Arc<PlatformService>,
    /// Credential verification.
    pub authenticator: Arc<CredentialAuthenticator>,
    pub registry: Arc<ScopeRegistry>,
    /// Internal service authentication (the only way into `/internal/v1`).
    pub service_auth: ServiceAuthRuntime,
}

impl AppState {
    pub fn new(
        platform: PlatformService,
        authenticator: CredentialAuthenticator,
        service_auth: ServiceAuthRuntime,
    ) -> Self {
        let registry = Arc::new(platform.registry().clone());
        Self {
            platform: Arc::new(platform),
            authenticator: Arc::new(authenticator),
            registry,
            service_auth,
        }
    }
}

//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_service_auth::ServiceTokenVerifier;
use sea_orm::DatabaseConnection;

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    /// Receiver-authoritative verifier of `cfmeet.organizations`.
    pub verifier: Arc<ServiceTokenVerifier>,
    /// Whether the Section 7 provisioning saga runs (reported by the provisioning read).
    pub provisioning_enabled: bool,
}

impl AppState {
    pub fn new(db: DatabaseConnection, verifier: Arc<ServiceTokenVerifier>) -> Self {
        Self {
            db,
            verifier,
            provisioning_enabled: false,
        }
    }

    pub fn with_provisioning(mut self, enabled: bool) -> Self {
        self.provisioning_enabled = enabled;
        self
    }
}

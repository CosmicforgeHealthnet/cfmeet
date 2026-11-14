//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_service_auth::ServiceTokenVerifier;
use sea_orm::DatabaseConnection;

use crate::provisioning::PlatformScopes;

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    /// Receiver-authoritative verifier of `cfmeet.organizations`.
    pub verifier: Arc<ServiceTokenVerifier>,
    /// Whether the Section 7 provisioning saga runs (reported by the provisioning read).
    pub provisioning_enabled: bool,
    /// The Platform Access port (operator binding rotation); `None` while provisioning
    /// is off.
    pub platform: Option<Arc<dyn PlatformScopes>>,
}

impl AppState {
    pub fn new(db: DatabaseConnection, verifier: Arc<ServiceTokenVerifier>) -> Self {
        Self {
            db,
            verifier,
            provisioning_enabled: false,
            platform: None,
        }
    }

    pub fn with_provisioning(mut self, enabled: bool) -> Self {
        self.provisioning_enabled = enabled;
        self
    }

    pub fn with_platform(mut self, platform: Option<Arc<dyn PlatformScopes>>) -> Self {
        self.platform = platform;
        self
    }
}

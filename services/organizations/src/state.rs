//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_service_auth::ServiceTokenVerifier;
use sea_orm::DatabaseConnection;

use crate::{exports::ExportDelivery, provisioning::PlatformScopes};

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
    /// Consumer destinations the outbox worker delivers to (empty: delivery disabled).
    /// Reported by `/ready`; the reconciliation record is never among them.
    pub event_destinations: Vec<String>,
    /// ADR 0064 (`ORGANIZATIONS_APP_INSTALLS_ENABLED`): the policy section `apps` and the
    /// app-install check. Off (default): both are absent, exactly as before.
    pub app_installs_enabled: bool,
    /// ADR 0068 A4 (`ORGANIZATIONS_EXPORT_DELIVERY_ENABLED`): owner-delivered exports (the
    /// at-rest keys and the notice builder). `None` (default): the bridge's export command
    /// is refused (409 `EXPORT_DELIVERY_UNAVAILABLE`).
    pub exports: Option<Arc<ExportDelivery>>,
}

impl AppState {
    pub fn new(db: DatabaseConnection, verifier: Arc<ServiceTokenVerifier>) -> Self {
        Self {
            db,
            verifier,
            provisioning_enabled: false,
            platform: None,
            event_destinations: Vec::new(),
            app_installs_enabled: false,
            exports: None,
        }
    }

    pub fn with_provisioning(mut self, enabled: bool) -> Self {
        self.provisioning_enabled = enabled;
        self
    }

    pub fn with_event_destinations(mut self, destinations: Vec<String>) -> Self {
        self.event_destinations = destinations;
        self
    }

    pub fn with_app_installs(mut self, enabled: bool) -> Self {
        self.app_installs_enabled = enabled;
        self
    }

    pub fn with_exports(mut self, exports: Option<Arc<ExportDelivery>>) -> Self {
        self.exports = exports;
        self
    }

    pub fn with_platform(mut self, platform: Option<Arc<dyn PlatformScopes>>) -> Self {
        self.platform = platform;
        self
    }
}

//! Shared application state.

use std::sync::Arc;

use crate::{config::ServiceAuthRuntime, service::TelephonyService};

#[derive(Clone)]
pub struct AppState {
    pub telephony: Arc<TelephonyService>,
    /// Internal service authentication (the only way into `/internal/v1`).
    pub service_auth: ServiceAuthRuntime,
}

impl AppState {
    pub fn new(telephony: TelephonyService, service_auth: ServiceAuthRuntime) -> Self {
        Self {
            telephony: Arc::new(telephony),
            service_auth,
        }
    }
}

//! Shared application state.

use std::sync::Arc;

use crate::{config::ServiceAuthRuntime, service::RelayService};

#[derive(Clone)]
pub struct AppState {
    pub relay: Arc<RelayService>,
    /// Internal service authentication (the only way in).
    pub service_auth: ServiceAuthRuntime,
}

impl AppState {
    pub fn new(relay: RelayService, service_auth: ServiceAuthRuntime) -> Self {
        Self {
            relay: Arc::new(relay),
            service_auth,
        }
    }
}

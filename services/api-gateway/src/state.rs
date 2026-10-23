//! Shared application state (cheap to clone). The gateway holds no database handle.

use std::{sync::Arc, time::Duration};

use crate::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags, QuotaSettings},
    identity::CustomerAuthority,
    routing::RouteTable,
};

#[derive(Clone)]
pub struct AppState {
    pub authority: Arc<dyn CustomerAuthority>,
    pub service_auth: ServiceAuthRuntime,
    pub routes: Arc<RouteTable>,
    pub flags: FeatureFlags,
    pub quotas: QuotaSettings,
    pub upstream_timeout: Duration,
    pub max_body_bytes: usize,
}

impl AppState {
    pub fn new(
        authority: Arc<dyn CustomerAuthority>,
        service_auth: ServiceAuthRuntime,
        routes: RouteTable,
    ) -> Self {
        Self {
            authority,
            service_auth,
            routes: Arc::new(routes),
            flags: FeatureFlags::default(),
            quotas: QuotaSettings::default(),
            upstream_timeout: Duration::from_secs(10),
            max_body_bytes: 1_048_576,
        }
    }

    pub fn with_flags(mut self, flags: FeatureFlags) -> Self {
        self.flags = flags;
        self
    }

    pub fn with_quotas(mut self, quotas: QuotaSettings) -> Self {
        self.quotas = quotas;
        self
    }

    pub fn with_upstream_timeout(mut self, timeout: Duration) -> Self {
        self.upstream_timeout = timeout;
        self
    }

    pub fn with_max_body_bytes(mut self, max: usize) -> Self {
        self.max_body_bytes = max;
        self
    }
}

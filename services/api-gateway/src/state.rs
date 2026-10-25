//! Shared application state (cheap to clone). The gateway holds no database handle.

use std::{sync::Arc, time::Duration};

use crate::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags, QuotaSettings},
    identity::CustomerAuthority,
    quota::QuotaEngine,
    routing::{RouteTable, RoutingControls},
};

#[derive(Clone)]
pub struct AppState {
    pub authority: Arc<dyn CustomerAuthority>,
    pub service_auth: ServiceAuthRuntime,
    pub routes: Arc<RouteTable>,
    pub flags: FeatureFlags,
    pub controls: RoutingControls,
    pub quotas: QuotaSettings,
    /// `None`: no quota enforcement (tests of routing only).
    pub quota: Option<Arc<QuotaEngine>>,
    pub upstream_timeout: Duration,
    pub max_body_bytes: usize,
    pub max_header_bytes: usize,
    pub max_headers: usize,
    pub ws_max_message_bytes: usize,
    pub cors_allowed_origins: Arc<Vec<String>>,
    /// Forwarding client: no redirects, no proxy, bounded connect time.
    pub http: reqwest::Client,
}

pub fn forwarding_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(3))
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .expect("forwarding client")
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
            controls: RoutingControls::default(),
            quotas: QuotaSettings::default(),
            quota: None,
            upstream_timeout: Duration::from_secs(10),
            max_body_bytes: 1_048_576,
            max_header_bytes: 16_384,
            max_headers: 64,
            ws_max_message_bytes: 1_048_576,
            cors_allowed_origins: Arc::new(Vec::new()),
            http: forwarding_client(),
        }
    }

    pub fn with_flags(mut self, flags: FeatureFlags) -> Self {
        self.flags = flags;
        self
    }

    pub fn with_controls(mut self, controls: RoutingControls) -> Self {
        self.controls = controls;
        self
    }

    pub fn with_quotas(mut self, quotas: QuotaSettings) -> Self {
        self.quotas = quotas;
        self
    }

    pub fn with_quota_engine(mut self, engine: Arc<QuotaEngine>) -> Self {
        self.quota = Some(engine);
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

    pub fn with_header_limits(mut self, max_bytes: usize, max_count: usize) -> Self {
        self.max_header_bytes = max_bytes;
        self.max_headers = max_count;
        self
    }

    pub fn with_ws_max_message_bytes(mut self, max: usize) -> Self {
        self.ws_max_message_bytes = max;
        self
    }

    pub fn with_cors_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.cors_allowed_origins = Arc::new(origins);
        self
    }
}

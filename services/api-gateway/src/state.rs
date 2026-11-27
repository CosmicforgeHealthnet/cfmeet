//! Shared application state (cheap to clone). The gateway holds no database handle.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use crate::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags, QuotaSettings},
    identity::CustomerAuthority,
    origins::OriginIndex,
    quota::QuotaEngine,
    routing::{RouteTable, RoutingControls},
    user_tokens::UserTokenVerifier,
};

#[derive(Clone)]
pub struct AppState {
    pub authority: Arc<dyn CustomerAuthority>,
    pub service_auth: ServiceAuthRuntime,
    pub routes: Arc<RouteTable>,
    /// The public OpenAPI documents reduced to this deployment's routable operations
    /// (built once, on first use: the route table never changes after start).
    pub published_docs: Arc<OnceLock<crate::docs::PublishedDocs>>,
    pub flags: FeatureFlags,
    pub controls: RoutingControls,
    pub quotas: QuotaSettings,
    /// `None`: no quota enforcement (tests of routing only).
    pub quota: Option<Arc<QuotaEngine>>,
    /// Per-token requests per minute of end-user / device traffic (`0`: off).
    pub token_rate_limit_per_minute: u64,
    pub upstream_timeout: Duration,
    pub max_body_bytes: usize,
    pub max_header_bytes: usize,
    pub max_headers: usize,
    pub ws_max_message_bytes: usize,
    pub cors_allowed_origins: Arc<Vec<String>>,
    /// CF Meet user-token pre-check (`API_GATEWAY_USER_TOKENS_ENABLED`; with the flag on
    /// and no verifier, user tokens are refused).
    pub user_tokens: Option<Arc<UserTokenVerifier>>,
    /// Partner-website origins (`API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED`; with the flag
    /// on and no index, partner origins are refused).
    pub origins: Option<Arc<OriginIndex>>,
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
            published_docs: Arc::new(OnceLock::new()),
            flags: FeatureFlags::default(),
            controls: RoutingControls::default(),
            quotas: QuotaSettings::default(),
            quota: None,
            token_rate_limit_per_minute: 0,
            upstream_timeout: Duration::from_secs(10),
            max_body_bytes: 1_048_576,
            max_header_bytes: 16_384,
            max_headers: 64,
            ws_max_message_bytes: 1_048_576,
            cors_allowed_origins: Arc::new(Vec::new()),
            user_tokens: None,
            origins: None,
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

    /// PLT-012: the per-token ceiling of end-user / device traffic (`0`: off).
    pub fn with_token_rate_limit(mut self, per_minute: u64) -> Self {
        self.token_rate_limit_per_minute = per_minute;
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

    pub fn with_user_token_verifier(mut self, verifier: Arc<UserTokenVerifier>) -> Self {
        self.user_tokens = Some(verifier);
        self
    }

    pub fn with_origin_index(mut self, index: Arc<OriginIndex>) -> Self {
        self.origins = Some(index);
        self
    }

    /// The user-token verifier when `API_GATEWAY_USER_TOKENS_ENABLED` is on.
    pub fn user_token_verifier(&self) -> Option<&UserTokenVerifier> {
        self.user_tokens
            .as_deref()
            .filter(|_| self.flags.user_tokens)
    }

    /// The origin index when `API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED` is on.
    pub fn origin_index(&self) -> Option<&OriginIndex> {
        self.origins
            .as_deref()
            .filter(|_| self.flags.environment_origins)
    }
}

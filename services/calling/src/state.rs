//! Shared application state for the HTTP layer.

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    calls::{
        app::{AppCallService, AppCallingConfig},
        CallService, CallServiceConfig,
    },
    domain::SharedClock,
    endpoints::{EndpointService, PresenceWindow},
    internal_auth::ServiceAuthRuntime,
    media::{gateway::MediaConnectionGateway, CallMediaProvider, MediaCredentialPolicy},
    platform::{
        authentication::CredentialAuthenticator,
        limits::{LimitDefaults, LimitPolicyResolver, RequestRateLimiter},
        principals::PrincipalResolver,
        scopes::ScopeRegistry,
        verifier::VerifierKeyring,
    },
    signaling::{PushDispatcher, RealtimeSignalBus, SignalingAuthority, SignalingKeys},
};

/// Application-calling runtime dependencies (Part C §20–§25), chosen by the composition
/// root: the signaling key, the media provider (none = application media disabled), the
/// signal bus (cross-instance PostgreSQL bus in production) and the tunables.
#[derive(Clone)]
pub struct CallingRuntime {
    pub signaling_keys: Arc<SignalingKeys>,
    pub media: Option<Arc<dyn CallMediaProvider>>,
    /// The provider's media connection gateway (admission before every connection).
    pub media_gateway: Option<Arc<dyn MediaConnectionGateway>>,
    pub credential_policy: MediaCredentialPolicy,
    pub bus: Arc<dyn RealtimeSignalBus>,
    pub push: Arc<dyn PushDispatcher>,
    pub app_config: AppCallingConfig,
    pub call_config: CallServiceConfig,
}

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub clock: SharedClock,
    pub authenticator: Arc<CredentialAuthenticator>,
    pub rate_limiter: Arc<RequestRateLimiter>,
    pub limits: Arc<LimitPolicyResolver>,
    pub principals: Arc<PrincipalResolver>,
    /// The call domain (Part C §15–§19).
    pub calls: Arc<CallService>,
    /// Application endpoints (§20).
    pub endpoints: Arc<EndpointService>,
    /// Signaling credential verification (§8).
    pub signaling: SignalingAuthority,
    /// Application calling orchestration (§20–§25).
    pub app: Arc<AppCallService>,
    /// Media connection gateway (none when application media is disabled).
    pub media_gateway: Option<Arc<dyn MediaConnectionGateway>>,
    /// Internal service authentication (separate from customer credentials).
    pub service_auth: ServiceAuthRuntime,
}

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        keyring: Arc<VerifierKeyring>,
        registry: Arc<ScopeRegistry>,
        limit_defaults: LimitDefaults,
        service_auth: ServiceAuthRuntime,
        runtime: CallingRuntime,
    ) -> Self {
        let principals = Arc::new(PrincipalResolver::new(db.clone()));
        let calls = Arc::new(CallService::new(
            db.clone(),
            clock.clone(),
            limit_defaults,
            service_auth.signer().runtime_environment(),
            runtime.call_config,
        ));
        let endpoints = Arc::new(EndpointService::new(
            db.clone(),
            clock.clone(),
            PresenceWindow::default(),
        ));
        let media_gateway = runtime.media_gateway.clone();
        let app = Arc::new(
            AppCallService::new(
                calls.clone(),
                endpoints.clone(),
                principals.clone(),
                runtime.media,
                runtime.credential_policy,
                runtime.bus,
                runtime.app_config,
            )
            .with_push(runtime.push),
        );
        Self {
            authenticator: Arc::new(CredentialAuthenticator::new(
                db.clone(),
                keyring,
                registry,
                clock.clone(),
            )),
            rate_limiter: Arc::new(RequestRateLimiter::new(
                db.clone(),
                limit_defaults,
                clock.clone(),
            )),
            limits: Arc::new(LimitPolicyResolver::new(db.clone(), limit_defaults)),
            principals,
            calls,
            endpoints,
            signaling: SignalingAuthority::new(runtime.signaling_keys, db.clone(), clock.clone()),
            app,
            media_gateway,
            service_auth,
            clock,
            db,
        }
    }
}

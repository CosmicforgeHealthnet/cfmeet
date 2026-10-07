//! Shared application state for the HTTP layer.

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    domain::SharedClock,
    internal_auth::ServiceAuthRuntime,
    platform::{
        authentication::CredentialAuthenticator,
        limits::{LimitDefaults, LimitPolicyResolver, RequestRateLimiter},
        principals::PrincipalResolver,
        scopes::ScopeRegistry,
        verifier::VerifierKeyring,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub authenticator: Arc<CredentialAuthenticator>,
    pub rate_limiter: Arc<RequestRateLimiter>,
    pub limits: Arc<LimitPolicyResolver>,
    pub principals: Arc<PrincipalResolver>,
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
    ) -> Self {
        Self {
            authenticator: Arc::new(CredentialAuthenticator::new(
                db.clone(),
                keyring,
                registry,
                clock.clone(),
            )),
            rate_limiter: Arc::new(RequestRateLimiter::new(db.clone(), limit_defaults, clock)),
            limits: Arc::new(LimitPolicyResolver::new(db.clone(), limit_defaults)),
            principals: Arc::new(PrincipalResolver::new(db.clone())),
            service_auth,
            db,
        }
    }
}

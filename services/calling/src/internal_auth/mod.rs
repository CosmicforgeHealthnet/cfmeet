//! Internal service authentication for Calling: the Axum adapter over the shared
//! `cf-service-auth` protocol (Part B section 11).
//!
//! * [`config`] loads Calling's own identity (`cfmeet.calling`), runtime environment,
//!   signing key and receiver-controlled trust bundle; any problem stops startup.
//! * [`middleware`] authenticates every request under `/internal/v1/` with the service
//!   token verifier and exposes the caller as [`middleware::InternalCaller`].
//! * [`delegation`] turns a trusted customer [`PlatformAuthContext`] (and optionally a
//!   separately resolved principal) into the safe signed [`DelegatedPlatformContext`]
//!   for outbound calls.
//!
//! Internal service authentication is entirely separate from customer credential
//! authentication: different header, key material, verifier, configuration, middleware,
//! trust policy and token format. It never touches the customer credential tables.
//!
//! [`PlatformAuthContext`]: crate::platform::context::PlatformAuthContext
//! [`DelegatedPlatformContext`]: cf_service_auth::DelegatedPlatformContext

pub mod config;
pub mod delegation;
pub mod middleware;

use std::{fmt, sync::Arc};

use cf_service_auth::{
    client::InternalServiceClient, services, ServiceId, ServiceTokenSigner, ServiceTokenVerifier,
};

/// Path prefix of every internal route.
pub const INTERNAL_PREFIX: &str = "/internal/v1";

/// The Calling service identity.
pub fn calling_service_id() -> ServiceId {
    ServiceId::parse(services::CALLING).expect("canonical identity")
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("signer identity {0} is not {calling}", calling = services::CALLING)]
    WrongSignerIdentity(ServiceId),
    #[error("trust bundle receiver {0} is not {calling}", calling = services::CALLING)]
    WrongReceiver(ServiceId),
    #[error("signer and verifier run in different runtime environments")]
    EnvironmentMismatch,
}

/// Calling's internal service authentication runtime.
#[derive(Clone)]
pub struct ServiceAuthRuntime {
    signer: Arc<ServiceTokenSigner>,
    verifier: Arc<ServiceTokenVerifier>,
    client: Arc<InternalServiceClient>,
}

impl fmt::Debug for ServiceAuthRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAuthRuntime")
            .field("signer", &self.signer)
            .field("verifier", &self.verifier)
            .field("client", &self.client)
            .finish()
    }
}

impl ServiceAuthRuntime {
    pub fn new(
        signer: Arc<ServiceTokenSigner>,
        verifier: Arc<ServiceTokenVerifier>,
        client: Arc<InternalServiceClient>,
    ) -> Result<Self, RuntimeError> {
        let calling = calling_service_id();
        if signer.issuer() != &calling {
            return Err(RuntimeError::WrongSignerIdentity(signer.issuer().clone()));
        }
        if verifier.receiver() != &calling {
            return Err(RuntimeError::WrongReceiver(verifier.receiver().clone()));
        }
        if signer.runtime_environment() != verifier.runtime_environment() {
            return Err(RuntimeError::EnvironmentMismatch);
        }
        Ok(Self {
            signer,
            verifier,
            client,
        })
    }

    pub fn signer(&self) -> &ServiceTokenSigner {
        &self.signer
    }

    pub fn verifier(&self) -> &ServiceTokenVerifier {
        &self.verifier
    }

    /// Authenticated client for synchronous calls to configured internal targets.
    pub fn client(&self) -> &InternalServiceClient {
        &self.client
    }

    /// The shared client handle (for adapters that keep it, e.g. Platform Access).
    pub fn client_arc(&self) -> Arc<InternalServiceClient> {
        self.client.clone()
    }
}

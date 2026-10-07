//! The authenticated API client identity.
//!
//! [`PlatformAuthContext`] answers *which application / environment / service account /
//! credential is calling and what it may do*. Every field comes from the database row
//! chain of the presented credential; the type cannot be constructed outside this crate,
//! cannot be deserialized, and is reachable by handlers only through the authentication
//! middleware.
//!
//! It deliberately says nothing about *who* takes part in a communication. Participant
//! identity is a [`ResolvedPrincipal`](super::principals::ResolvedPrincipal), resolved
//! separately and never derived from this context.

use axum::{extract::FromRequestParts, http::request::Parts};

use super::scopes::ScopeSet;
use crate::{
    domain::{
        ApplicationId, CredentialId, EnvironmentId, EnvironmentKind, RequestId, ServiceAccountId,
        TenantClassification, TenantId,
    },
    error::CallingError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAuthContext {
    tenant_id: TenantId,
    tenant_classification: TenantClassification,
    application_id: ApplicationId,
    environment_id: EnvironmentId,
    environment_kind: EnvironmentKind,
    live_telecom_allowed: bool,
    service_account_id: ServiceAccountId,
    credential_id: CredentialId,
    scopes: ScopeSet,
    request_id: RequestId,
}

/// Values loaded from the credential's persisted authority chain.
pub(crate) struct TrustedAuthority {
    pub tenant_id: TenantId,
    pub tenant_classification: TenantClassification,
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub environment_kind: EnvironmentKind,
    pub live_telecom_allowed: bool,
    pub service_account_id: ServiceAccountId,
    pub credential_id: CredentialId,
    pub scopes: ScopeSet,
}

impl PlatformAuthContext {
    pub(crate) fn from_trusted(authority: TrustedAuthority, request_id: RequestId) -> Self {
        Self {
            tenant_id: authority.tenant_id,
            tenant_classification: authority.tenant_classification,
            application_id: authority.application_id,
            environment_id: authority.environment_id,
            environment_kind: authority.environment_kind,
            // Defence in depth: never report live telecom outside PRODUCTION, whatever
            // the row says (the database CHECK already forbids it).
            live_telecom_allowed: authority.live_telecom_allowed
                && authority.environment_kind.may_allow_live_telecom(),
            service_account_id: authority.service_account_id,
            credential_id: authority.credential_id,
            scopes: authority.scopes,
            request_id,
        }
    }

    pub fn tenant_id(&self) -> TenantId {
        self.tenant_id
    }

    pub fn tenant_classification(&self) -> TenantClassification {
        self.tenant_classification
    }

    pub fn application_id(&self) -> ApplicationId {
        self.application_id
    }

    pub fn environment_id(&self) -> EnvironmentId {
        self.environment_id
    }

    pub fn environment_kind(&self) -> EnvironmentKind {
        self.environment_kind
    }

    /// Whether this environment may ever produce real public-network telecom effects.
    /// Always `false` for SANDBOX and TEST.
    pub fn live_telecom_allowed(&self) -> bool {
        self.live_telecom_allowed
    }

    pub fn service_account_id(&self) -> ServiceAccountId {
        self.service_account_id
    }

    pub fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    pub fn scopes(&self) -> &ScopeSet {
        &self.scopes
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    /// Fails with `INSUFFICIENT_SCOPE` (403) unless the credential was granted `scope`.
    pub fn require_scope(&self, scope: &str) -> Result<(), CallingError> {
        if self.has_scope(scope) {
            Ok(())
        } else {
            tracing::info!(
                credential_id = %self.credential_id,
                required_scope = scope,
                "scope check denied"
            );
            Err(CallingError::InsufficientScope {
                required: scope.to_string(),
            })
        }
    }
}

/// Handlers obtain the context only from request extensions populated by the
/// authentication middleware. Without it (route not wrapped) the request is rejected.
#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for PlatformAuthContext {
    type Rejection = CallingError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<PlatformAuthContext>()
            .cloned()
            .ok_or(CallingError::AuthenticationFailed)
    }
}

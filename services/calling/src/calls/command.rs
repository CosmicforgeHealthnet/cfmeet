//! Trusted inputs of call-domain operations: ownership scope, actor and request context.
//!
//! None of these types can be built from request data. A [`CallScope`] comes from an
//! authenticated [`PlatformAuthContext`] or from the delegated context signed into an
//! internal service token; a [`CallCommandContext`] additionally carries the actor the
//! mutation is attributed to and the server-controlled request / correlation ids. Actor
//! identity is never fabricated: it is exactly the authenticated customer credential and
//! service account, or the authenticated internal service acting for the delegated
//! service account. No raw Authorization header, credential or service token is kept.
//!
//! These are explicit, reviewed crossings; there are no `From` conversions between the
//! authentication context types.

use cf_service_auth::{
    CorrelationId, DelegatedPlatformContext, InternalServiceContext, RequestId, ServiceId,
};

use super::{errors::CallError, kinds::ActorKind};
use crate::{
    domain::{ApplicationId, CredentialId, EnvironmentId, ServiceAccountId, TenantId},
    platform::{context::PlatformAuthContext, principals::ResolvedPrincipal},
};

/// Tenant / application / environment every call-domain query and mutation is confined to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallScope {
    tenant_id: TenantId,
    application_id: ApplicationId,
    environment_id: EnvironmentId,
}

impl CallScope {
    /// The scope of an authenticated customer request.
    pub fn of_platform(ctx: &PlatformAuthContext) -> Self {
        Self {
            tenant_id: ctx.tenant_id(),
            application_id: ctx.application_id(),
            environment_id: ctx.environment_id(),
        }
    }

    /// The scope asserted by an authenticated internal service in its signed token.
    pub fn of_delegated(ctx: &DelegatedPlatformContext) -> Self {
        Self {
            tenant_id: TenantId::from_uuid(ctx.tenant_id()),
            application_id: ApplicationId::from_uuid(ctx.application_id()),
            environment_id: EnvironmentId::from_uuid(ctx.application_environment_id()),
        }
    }

    /// The scope a resolved principal was resolved in.
    pub fn of_principal(principal: &ResolvedPrincipal) -> Self {
        Self {
            tenant_id: principal.tenant_id(),
            application_id: principal.application_id(),
            environment_id: principal.environment_id(),
        }
    }

    pub(crate) fn from_parts(
        tenant_id: TenantId,
        application_id: ApplicationId,
        environment_id: EnvironmentId,
    ) -> Self {
        Self {
            tenant_id,
            application_id,
            environment_id,
        }
    }

    pub fn tenant_id(&self) -> TenantId {
        self.tenant_id
    }

    pub fn application_id(&self) -> ApplicationId {
        self.application_id
    }

    pub fn environment_id(&self) -> EnvironmentId {
        self.environment_id
    }
}

/// Who a call mutation is attributed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallActor {
    /// A customer application, authenticated by its API credential.
    CustomerApplication {
        service_account_id: ServiceAccountId,
        credential_id: CredentialId,
    },
    /// An authenticated CF Meet backend service acting for the delegated service account.
    InternalService {
        service: ServiceId,
        on_behalf_of: ServiceAccountId,
    },
}

impl CallActor {
    pub fn kind(&self) -> ActorKind {
        match self {
            CallActor::CustomerApplication { .. } => ActorKind::CustomerApplication,
            CallActor::InternalService { .. } => ActorKind::InternalService,
        }
    }

    pub fn service_account_id(&self) -> ServiceAccountId {
        match self {
            CallActor::CustomerApplication {
                service_account_id, ..
            } => *service_account_id,
            CallActor::InternalService { on_behalf_of, .. } => *on_behalf_of,
        }
    }

    pub fn credential_id(&self) -> Option<CredentialId> {
        match self {
            CallActor::CustomerApplication { credential_id, .. } => Some(*credential_id),
            CallActor::InternalService { .. } => None,
        }
    }

    pub fn service(&self) -> Option<&ServiceId> {
        match self {
            CallActor::CustomerApplication { .. } => None,
            CallActor::InternalService { service, .. } => Some(service),
        }
    }
}

/// Scope + actor + request context of one call-domain command.
#[derive(Debug, Clone)]
pub struct CallCommandContext {
    scope: CallScope,
    actor: CallActor,
    request_id: RequestId,
    correlation_id: CorrelationId,
}

impl CallCommandContext {
    /// A command issued by an authenticated customer request.
    pub fn customer(ctx: &PlatformAuthContext) -> Self {
        Self {
            scope: CallScope::of_platform(ctx),
            actor: CallActor::CustomerApplication {
                service_account_id: ctx.service_account_id(),
                credential_id: ctx.credential_id(),
            },
            request_id: ctx.request_id().clone(),
            correlation_id: ctx.correlation_id().clone(),
        }
    }

    /// A command issued by an authenticated internal service. The scope is the delegated
    /// platform context signed into its token; without one there is no scope to act in.
    pub fn internal(caller: &InternalServiceContext) -> Result<Self, CallError> {
        let delegated = caller
            .delegated_platform_context()
            .ok_or(CallError::NotFound)?;
        Ok(Self {
            scope: CallScope::of_delegated(delegated),
            actor: CallActor::InternalService {
                service: caller.authenticated_service().clone(),
                on_behalf_of: ServiceAccountId::from_uuid(
                    delegated.initiating_service_account_id(),
                ),
            },
            request_id: caller.request_id().clone(),
            correlation_id: caller.correlation_id().clone(),
        })
    }

    pub fn scope(&self) -> CallScope {
        self.scope
    }

    pub fn actor(&self) -> &CallActor {
        &self.actor
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }
}

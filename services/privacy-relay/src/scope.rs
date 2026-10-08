//! Trusted inputs of every Privacy Relay operation.
//!
//! [`RelayScope`] (tenant / application / environment) comes ONLY from the delegated
//! platform context signed into the caller's service token — never from a request body,
//! path or query. [`CommandContext`] adds the acting service, the delegated service
//! account the mutation is attributed to and the signed request / correlation ids.
//! None of these types can be deserialized.

use cf_service_auth::{
    ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext, RequestId, ServiceId,
};
use uuid::Uuid;

use crate::{config::privacy_relay_service_id, error::RelayError, middleware::InternalCaller};

/// Ownership scope of every query and mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RelayScope {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
}

impl RelayScope {
    pub fn of_delegated(ctx: &DelegatedPlatformContext) -> Self {
        Self {
            tenant_id: ctx.tenant_id(),
            application_id: ctx.application_id(),
            environment_id: ctx.application_environment_id(),
        }
    }

    /// Scope of a persisted row (workers act on rows they own).
    pub(crate) fn from_row(tenant_id: Uuid, application_id: Uuid, environment_id: Uuid) -> Self {
        Self {
            tenant_id,
            application_id,
            environment_id,
        }
    }

    pub fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub fn application_id(&self) -> Uuid {
        self.application_id
    }

    pub fn environment_id(&self) -> Uuid {
        self.environment_id
    }
}

/// Who a mutation is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    /// A consumer application acting through the authenticated platform ingress.
    Application,
    /// Privacy Relay itself (expiry, enforcement delivery).
    System,
}

impl ActorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActorKind::Application => "APPLICATION",
            ActorKind::System => "SYSTEM",
        }
    }
}

/// Scope + actor + request identity of one command.
#[derive(Debug, Clone)]
pub struct CommandContext {
    scope: RelayScope,
    environment_kind: ApplicationEnvironmentKind,
    actor_kind: ActorKind,
    actor_service: ServiceId,
    service_account_id: Uuid,
    request_id: RequestId,
    correlation_id: CorrelationId,
}

impl CommandContext {
    /// A consumer command arriving through an authenticated internal caller. Without a
    /// signed delegated context there is no scope to act in.
    pub fn from_caller(caller: &InternalCaller) -> Result<Self, RelayError> {
        let delegated = caller.delegated()?;
        Ok(Self {
            scope: RelayScope::of_delegated(delegated),
            environment_kind: delegated.application_environment_kind(),
            actor_kind: ActorKind::Application,
            actor_service: caller.context().authenticated_service().clone(),
            service_account_id: delegated.initiating_service_account_id(),
            request_id: caller.request_id().clone(),
            correlation_id: caller.correlation_id().clone(),
        })
    }

    /// A command Privacy Relay issues itself for a row of `scope` (fresh ids).
    pub(crate) fn system(
        scope: RelayScope,
        environment_kind: ApplicationEnvironmentKind,
        on_behalf_of: Uuid,
    ) -> Self {
        Self {
            scope,
            environment_kind,
            actor_kind: ActorKind::System,
            actor_service: privacy_relay_service_id(),
            service_account_id: on_behalf_of,
            request_id: RequestId::generate(),
            correlation_id: CorrelationId::generate(),
        }
    }

    /// A consumer command for tests and in-process composition (same shape as
    /// [`from_caller`](Self::from_caller), built from an already verified delegated context).
    pub fn for_delegated(delegated: &DelegatedPlatformContext, caller: ServiceId) -> Self {
        Self {
            scope: RelayScope::of_delegated(delegated),
            environment_kind: delegated.application_environment_kind(),
            actor_kind: ActorKind::Application,
            actor_service: caller,
            service_account_id: delegated.initiating_service_account_id(),
            request_id: RequestId::generate(),
            correlation_id: CorrelationId::generate(),
        }
    }

    /// The same command continuing an existing causal operation.
    pub(crate) fn with_correlation(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = correlation_id;
        self
    }

    pub fn scope(&self) -> RelayScope {
        self.scope
    }

    pub fn environment_kind(&self) -> ApplicationEnvironmentKind {
        self.environment_kind
    }

    pub fn actor_kind(&self) -> ActorKind {
        self.actor_kind
    }

    pub fn actor_service(&self) -> &ServiceId {
        &self.actor_service
    }

    pub fn service_account_id(&self) -> Uuid {
        self.service_account_id
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }
}

pub fn environment_kind_str(kind: ApplicationEnvironmentKind) -> &'static str {
    match kind {
        ApplicationEnvironmentKind::Sandbox => "SANDBOX",
        ApplicationEnvironmentKind::Test => "TEST",
        ApplicationEnvironmentKind::Production => "PRODUCTION",
    }
}

pub fn parse_environment_kind(value: &str) -> Option<ApplicationEnvironmentKind> {
    ApplicationEnvironmentKind::ALL
        .iter()
        .copied()
        .find(|k| environment_kind_str(*k) == value)
}

//! Outbound trust-boundary adapter.
//!
//! ```text
//! customer request ──► PlatformAuthContext (customer credential verified here)
//!                         │ delegated_context()  — safe identifiers only
//!                         ▼
//!                  DelegatedPlatformContext ──► signed into the service token
//!                                                 │
//!                                                 ▼ downstream service
//!                                          InternalServiceContext + DelegatedPlatformContext
//! ```
//!
//! The downstream service never receives the customer credential and never runs
//! Calling's `PlatformAuthContext`. A separately resolved principal can be attached only
//! if it belongs to the same tenant / application / environment. There are no `From`
//! conversions between the context types: each crossing is an explicit, reviewed call.

use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
    OutboundServiceRequest, ServiceId,
};

use super::middleware::InternalCaller;
use crate::{
    domain::{EnvironmentKind, PrincipalKind},
    platform::{context::PlatformAuthContext, principals::ResolvedPrincipal},
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DelegationError {
    #[error("the principal does not belong to the authenticated application environment")]
    PrincipalOutOfScope,
}

fn environment_kind(kind: EnvironmentKind) -> ApplicationEnvironmentKind {
    match kind {
        EnvironmentKind::Sandbox => ApplicationEnvironmentKind::Sandbox,
        EnvironmentKind::Test => ApplicationEnvironmentKind::Test,
        EnvironmentKind::Production => ApplicationEnvironmentKind::Production,
    }
}

pub(crate) fn principal_kind(kind: PrincipalKind) -> DelegatedPrincipalKind {
    match kind {
        PrincipalKind::NativeCfUser => DelegatedPrincipalKind::NativeCfUser,
        PrincipalKind::ExternalApplicationPrincipal => {
            DelegatedPrincipalKind::ExternalApplicationPrincipal
        }
        PrincipalKind::EnterpriseMember => DelegatedPrincipalKind::EnterpriseMember,
        PrincipalKind::ServiceAccount => DelegatedPrincipalKind::ServiceAccount,
        PrincipalKind::TelephoneEndpoint => DelegatedPrincipalKind::TelephoneEndpoint,
        PrincipalKind::TemporaryExternalParticipant => {
            DelegatedPrincipalKind::TemporaryExternalParticipant
        }
        PrincipalKind::BotOrService => DelegatedPrincipalKind::BotOrService,
    }
}

/// Safe delegated context of an authenticated customer request, optionally naming the
/// initiating principal (which must be in the same scope).
pub fn delegated_context(
    customer: &PlatformAuthContext,
    initiating_principal: Option<&ResolvedPrincipal>,
) -> Result<DelegatedPlatformContext, DelegationError> {
    let context = DelegatedPlatformContext::new(
        customer.tenant_id().as_uuid(),
        customer.application_id().as_uuid(),
        customer.environment_id().as_uuid(),
        environment_kind(customer.environment_kind()),
        customer.service_account_id().as_uuid(),
    );
    match initiating_principal {
        None => Ok(context),
        Some(p) => {
            let same_scope = p.tenant_id() == customer.tenant_id()
                && p.application_id() == customer.application_id()
                && p.environment_id() == customer.environment_id();
            if !same_scope {
                return Err(DelegationError::PrincipalOutOfScope);
            }
            Ok(context.with_initiating_principal(p.id().as_uuid(), principal_kind(p.kind())))
        }
    }
}

/// Outbound hop of a customer request: new request id, the customer request's
/// correlation id, and the signed delegated context.
pub fn outbound_for_customer(
    customer: &PlatformAuthContext,
    audience: ServiceId,
    initiating_principal: Option<&ResolvedPrincipal>,
) -> Result<OutboundServiceRequest, DelegationError> {
    Ok(
        OutboundServiceRequest::continuing(audience, customer.correlation_id())
            .with_delegated_context(delegated_context(customer, initiating_principal)?),
    )
}

/// Outbound hop of an authenticated internal request: new request id, same correlation
/// id, same delegated context.
pub fn outbound_next_hop(inbound: &InternalCaller, audience: ServiceId) -> OutboundServiceRequest {
    OutboundServiceRequest::next_hop_of(inbound.context(), audience)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_mappings_are_exhaustive_and_name_preserving() {
        for kind in EnvironmentKind::ALL {
            assert_eq!(
                serde_json::to_value(environment_kind(*kind)).unwrap(),
                kind.as_str()
            );
        }
        for kind in PrincipalKind::ALL {
            assert_eq!(
                serde_json::to_value(principal_kind(*kind)).unwrap(),
                kind.as_str()
            );
        }
    }
}

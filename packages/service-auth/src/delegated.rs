//! Delegated platform context (contract `cfmeet.platform.delegated-platform-context`).
//!
//! Safe customer / application / actor identifiers an authenticated backend service
//! asserts on behalf of the request it serves. It travels only inside the signed service
//! token, so it cannot be altered after signing. It contains identifiers only — never a
//! credential, verifier, Authorization header, telephone number, protected contact,
//! password, cookie, provider secret or consumer business payload — and it is not customer
//! authentication: it never becomes a customer platform context downstream.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kind of the customer application environment (Part B section 7). Distinct from the
/// service runtime environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApplicationEnvironmentKind {
    Sandbox,
    Test,
    Production,
}

impl ApplicationEnvironmentKind {
    pub const ALL: &'static [ApplicationEnvironmentKind] = &[
        ApplicationEnvironmentKind::Sandbox,
        ApplicationEnvironmentKind::Test,
        ApplicationEnvironmentKind::Production,
    ];
}

/// Category of the initiating communication principal (Part B section 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DelegatedPrincipalKind {
    NativeCfUser,
    ExternalApplicationPrincipal,
    EnterpriseMember,
    ServiceAccount,
    TelephoneEndpoint,
    TemporaryExternalParticipant,
    BotOrService,
}

impl DelegatedPrincipalKind {
    pub const ALL: &'static [DelegatedPrincipalKind] = &[
        DelegatedPrincipalKind::NativeCfUser,
        DelegatedPrincipalKind::ExternalApplicationPrincipal,
        DelegatedPrincipalKind::EnterpriseMember,
        DelegatedPrincipalKind::ServiceAccount,
        DelegatedPrincipalKind::TelephoneEndpoint,
        DelegatedPrincipalKind::TemporaryExternalParticipant,
        DelegatedPrincipalKind::BotOrService,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InitiatingPrincipal {
    pub principal_id: Uuid,
    pub principal_kind: DelegatedPrincipalKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Wire", into = "Wire")]
pub struct DelegatedPlatformContext {
    tenant_id: Uuid,
    application_id: Uuid,
    application_environment_id: Uuid,
    application_environment_kind: ApplicationEnvironmentKind,
    initiating_service_account_id: Uuid,
    initiating_principal: Option<InitiatingPrincipal>,
    initiating_credential_id: Option<Uuid>,
}

/// Exact wire shape of the contract (closed object, principal id and kind together).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    tenant_id: Uuid,
    application_id: Uuid,
    application_environment_id: Uuid,
    application_environment_kind: ApplicationEnvironmentKind,
    initiating_service_account_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initiating_principal_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initiating_principal_kind: Option<DelegatedPrincipalKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initiating_credential_id: Option<Uuid>,
}

impl TryFrom<Wire> for DelegatedPlatformContext {
    type Error = String;
    fn try_from(w: Wire) -> Result<Self, Self::Error> {
        let initiating_principal = match (w.initiating_principal_id, w.initiating_principal_kind) {
            (Some(principal_id), Some(principal_kind)) => Some(InitiatingPrincipal {
                principal_id,
                principal_kind,
            }),
            (None, None) => None,
            _ => return Err(
                "initiating_principal_id and initiating_principal_kind must be present together"
                    .to_string(),
            ),
        };
        Ok(Self {
            tenant_id: w.tenant_id,
            application_id: w.application_id,
            application_environment_id: w.application_environment_id,
            application_environment_kind: w.application_environment_kind,
            initiating_service_account_id: w.initiating_service_account_id,
            initiating_principal,
            initiating_credential_id: w.initiating_credential_id,
        })
    }
}

impl From<DelegatedPlatformContext> for Wire {
    fn from(c: DelegatedPlatformContext) -> Self {
        Wire {
            tenant_id: c.tenant_id,
            application_id: c.application_id,
            application_environment_id: c.application_environment_id,
            application_environment_kind: c.application_environment_kind,
            initiating_service_account_id: c.initiating_service_account_id,
            initiating_principal_id: c.initiating_principal.map(|p| p.principal_id),
            initiating_principal_kind: c.initiating_principal.map(|p| p.principal_kind),
            initiating_credential_id: c.initiating_credential_id,
        }
    }
}

impl DelegatedPlatformContext {
    /// Built by a service adapter from its own trusted, already-authenticated customer
    /// context. There is deliberately no conversion from any other context type.
    pub fn new(
        tenant_id: Uuid,
        application_id: Uuid,
        application_environment_id: Uuid,
        application_environment_kind: ApplicationEnvironmentKind,
        initiating_service_account_id: Uuid,
    ) -> Self {
        Self {
            tenant_id,
            application_id,
            application_environment_id,
            application_environment_kind,
            initiating_service_account_id,
            initiating_principal: None,
            initiating_credential_id: None,
        }
    }

    /// The API credential (Platform Access id, never a secret) the API Gateway
    /// authenticated for this request. Receivers re-verify it with Platform Access
    /// (`POST /internal/v1/credentials/delegated-authority`) so revocation, expiry and
    /// effective scopes are evaluated live by the identity authority.
    pub fn with_initiating_credential(mut self, credential_id: Uuid) -> Self {
        self.initiating_credential_id = Some(credential_id);
        self
    }

    pub fn initiating_credential_id(&self) -> Option<Uuid> {
        self.initiating_credential_id
    }

    pub fn with_initiating_principal(
        mut self,
        principal_id: Uuid,
        principal_kind: DelegatedPrincipalKind,
    ) -> Self {
        self.initiating_principal = Some(InitiatingPrincipal {
            principal_id,
            principal_kind,
        });
        self
    }

    pub fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub fn application_id(&self) -> Uuid {
        self.application_id
    }

    pub fn application_environment_id(&self) -> Uuid {
        self.application_environment_id
    }

    pub fn application_environment_kind(&self) -> ApplicationEnvironmentKind {
        self.application_environment_kind
    }

    pub fn initiating_service_account_id(&self) -> Uuid {
        self.initiating_service_account_id
    }

    pub fn initiating_principal(&self) -> Option<InitiatingPrincipal> {
        self.initiating_principal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            ApplicationEnvironmentKind::Sandbox,
            Uuid::new_v4(),
        )
    }

    #[test]
    fn serializes_exactly_the_contract_fields() {
        let plain = serde_json::to_value(ctx()).unwrap();
        let mut keys: Vec<_> = plain.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "application_environment_id",
                "application_environment_kind",
                "application_id",
                "initiating_service_account_id",
                "tenant_id"
            ]
        );
        let with = serde_json::to_value(
            ctx().with_initiating_principal(Uuid::new_v4(), DelegatedPrincipalKind::NativeCfUser),
        )
        .unwrap();
        assert_eq!(with["initiating_principal_kind"], "NATIVE_CF_USER");
        assert!(with["initiating_principal_id"].is_string());
        let credential = Uuid::new_v4();
        let with = serde_json::to_value(ctx().with_initiating_credential(credential)).unwrap();
        assert_eq!(with["initiating_credential_id"], credential.to_string());
        let back: DelegatedPlatformContext = serde_json::from_value(with).unwrap();
        assert_eq!(back.initiating_credential_id(), Some(credential));
    }

    #[test]
    fn rejects_unknown_fields_and_half_principals() {
        let mut v = serde_json::to_value(ctx()).unwrap();
        v["phone_number"] = "+2348012345678".into();
        assert!(serde_json::from_value::<DelegatedPlatformContext>(v).is_err());
        let mut v = serde_json::to_value(ctx()).unwrap();
        v["initiating_principal_id"] = Uuid::new_v4().to_string().into();
        assert!(serde_json::from_value::<DelegatedPlatformContext>(v).is_err());
        let round = serde_json::to_value(ctx()).unwrap();
        assert!(serde_json::from_value::<DelegatedPlatformContext>(round).is_ok());
    }
}

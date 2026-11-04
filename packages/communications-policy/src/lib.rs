//! Typed, fail-closed personal policy contract. No service source or databases here.
use cf_service_auth::{
    client::InternalServiceClient, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub const CHECK_PATH: &str = "/internal/v1/communications/policy/check";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}
impl Scope {
    pub fn of(d: &DelegatedPlatformContext) -> Self {
        Self {
            tenant_id: d.tenant_id(),
            application_id: d.application_id(),
            environment_id: d.application_environment_id(),
        }
    }
    /// Only the configured first-party personal scope is governed here. Enterprise and
    /// external applications retain their own separately authenticated domain policies.
    pub fn from_env() -> Result<Option<Self>, String> {
        let keys = [
            "CF_COMMUNICATIONS_PERSONAL_TENANT_ID",
            "CF_COMMUNICATIONS_PERSONAL_APPLICATION_ID",
            "CF_COMMUNICATIONS_PERSONAL_ENVIRONMENT_ID",
        ];
        let values: Vec<_> = keys.iter().map(|k| std::env::var(k).ok()).collect();
        if values.iter().all(Option::is_none) {
            if std::env::var_os("CF_COMMUNICATIONS_BASE_URL").is_some() {
                return Err("personal policy scope is required with Communications".into());
            }
            return Ok(None);
        }
        if std::env::var_os("CF_COMMUNICATIONS_BASE_URL").is_none() {
            return Err("Communications URL is required with personal scope".into());
        }
        let ids: Result<Vec<_>, _> = values
            .iter()
            .map(|v| Uuid::parse_str(v.as_deref().unwrap_or("")))
            .collect();
        let ids = ids.map_err(|_| "personal scope must contain three UUIDs")?;
        Ok(Some(Self {
            tenant_id: ids[0],
            application_id: ids[1],
            environment_id: ids[2],
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Action {
    DirectCall,
    DirectConversation,
    MessageSend,
    ContactRequest,
    GroupInvitation,
    PersonalActivity,
    Presence,
    ProfileDiscovery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyCheck {
    pub action: Action,
    pub actor_principal_id: Uuid,
    pub target_principal_id: Uuid,
    /// From an authenticated endpoint/client token, never a public request body.
    pub credential_issued_at: Option<i64>,
    #[serde(default)]
    pub credential_principal_id: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decision {
    pub schema_version: u32,
    pub allowed: bool,
    pub reason: DecisionReason,
    pub policy_version: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionReason {
    Allowed,
    NotPermitted,
    AccountInactive,
    CredentialRevoked,
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("personal communication is not permitted")]
    Denied,
    #[error("personal communication policy is unavailable")]
    Unavailable,
}
#[derive(Clone)]
pub struct PolicyClient {
    client: Arc<InternalServiceClient>,
    personal: Scope,
}
impl PolicyClient {
    pub fn new(client: Arc<InternalServiceClient>, personal: Scope) -> Self {
        Self { client, personal }
    }
    pub fn governs(&self, scope: Scope) -> bool {
        self.personal == scope
    }
    pub async fn check(
        &self,
        scope: DelegatedPlatformContext,
        check: &PolicyCheck,
    ) -> Result<(), PolicyError> {
        if !self.governs(Scope::of(&scope)) {
            return Err(PolicyError::Denied);
        }
        let req = OutboundServiceRequest::new_operation(
            ServiceId::parse("cfmeet.communications").expect("service identity"),
        )
        .with_delegated_context(scope);
        let response = self
            .client
            .post_json(&req, CHECK_PATH, check)
            .await
            .map_err(|_| PolicyError::Unavailable)?;
        if response.status() != 200 {
            return Err(PolicyError::Unavailable);
        }
        let decision: Decision = response.json().map_err(|_| PolicyError::Unavailable)?;
        if decision.schema_version != 1 {
            return Err(PolicyError::Unavailable);
        }
        if decision.allowed && decision.reason == DecisionReason::Allowed {
            Ok(())
        } else {
            Err(PolicyError::Denied)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_policy_vocabulary_and_versioned_decisions() {
        assert!(serde_json::from_str::<Action>("\"ALLOW_ALL\"").is_err());
        assert!(serde_json::from_str::<PolicyCheck>(r#"{"action":"DIRECT_CALL","actorPrincipalId":"00000000-0000-0000-0000-000000000001","targetPrincipalId":"00000000-0000-0000-0000-000000000002","credentialIssuedAt":null,"allow":true}"#).is_err());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifiedAccount {
    pub account_reference: String,
    pub principal_id: Uuid,
    pub display_name: String,
    pub avatar_reference: Option<String>,
    /// Timestamp of credential authentication, not access-token refresh time.
    pub authenticated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dispatch {
    pub method: String,
    pub path: String,
    pub query: std::collections::BTreeMap<String, String>,
    pub body: Option<serde_json::Value>,
    pub idempotency_key: Option<Uuid>,
    pub account: VerifiedAccount,
}

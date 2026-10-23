//! Customer authentication — Platform Access is the ONLY credential authority.
//!
//! The gateway never verifies a credential itself: it asks Platform Access
//! (`POST /internal/v1/credentials/authenticate`, permission
//! `platform-credential.authenticate`) and takes tenant, application, environment, service
//! account and scopes ONLY from the authority chain Platform Access returns. A tenant,
//! project or environment id in a request (header, query or body) never becomes authority.
//! Platform Access unreachable means the request is refused (fail closed).

use std::{collections::BTreeSet, sync::Arc};

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext, OutboundServiceRequest,
    ServiceId,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// Trust classification of the tenant (Platform Access `TenantClassification`).
/// First-party CosmicForge products and external customers use the same routes; they
/// differ only in authorized scopes and this classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustClassification {
    FirstParty,
    External,
}

/// The authenticated customer of a request, from Platform Access's authority chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedCustomer {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: ApplicationEnvironmentKind,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub classification: TrustClassification,
    /// FULL authority; SAFETY_ONLY contexts are refused at the ingress.
    pub full_authority: bool,
    pub scopes: BTreeSet<String>,
}

impl AuthenticatedCustomer {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    /// The signed delegated context forwarded upstream (identifiers only).
    pub fn delegated(&self) -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            self.tenant_id,
            self.application_id,
            self.environment_id,
            self.environment_kind,
            self.service_account_id,
        )
        // The receiver re-verifies this credential with Platform Access (revocation,
        // expiry and effective scopes are evaluated live, never trusted from here).
        .with_initiating_credential(self.credential_id)
    }

    /// Parses Platform Access's `AuthorityChain` (contract `cfmeet.platform-access.identity`).
    pub fn from_authority(authority: &Value) -> Option<Self> {
        let id = |path: &[&str]| -> Option<Uuid> {
            let mut v = authority;
            for p in path {
                v = v.get(*p)?;
            }
            Uuid::parse_str(v.as_str()?).ok()
        };
        let environment_kind = match authority.get("environment")?.get("kind")?.as_str()? {
            "SANDBOX" => ApplicationEnvironmentKind::Sandbox,
            "TEST" => ApplicationEnvironmentKind::Test,
            "PRODUCTION" => ApplicationEnvironmentKind::Production,
            _ => return None,
        };
        let classification = match authority.get("tenant")?.get("classification")?.as_str()? {
            "FIRST_PARTY" => TrustClassification::FirstParty,
            "EXTERNAL" => TrustClassification::External,
            _ => return None,
        };
        let scopes = authority
            .get("scopes")?
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        Some(Self {
            tenant_id: id(&["tenant", "id"])?,
            application_id: id(&["application", "id"])?,
            environment_id: id(&["environment", "id"])?,
            environment_kind,
            service_account_id: id(&["serviceAccount", "id"])?,
            credential_id: id(&["credential", "id"])?,
            classification,
            full_authority: authority.get("authorityMode")?.as_str()? == "FULL",
            scopes,
        })
    }
}

/// Failed-authentication throttling reported by Platform Access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Throttle {
    pub retry_after_seconds: u64,
    pub threshold_crossed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    Authenticated(Box<AuthenticatedCustomer>),
    /// The credential does not authenticate (reason for logs only).
    Rejected {
        reason: String,
        throttle: Option<Throttle>,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("the identity authority is unavailable: {0}")]
pub struct AuthorityUnavailable(pub String);

#[async_trait::async_trait]
pub trait CustomerAuthority: Send + Sync {
    /// Authenticates `presented` as part of the operation `correlation`.
    async fn authenticate(
        &self,
        presented: &str,
        correlation: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable>;
}

/// Platform Access over authenticated internal HTTP.
pub struct PlatformAccessAuthority {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessAuthority {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl CustomerAuthority for PlatformAccessAuthority {
    async fn authenticate(
        &self,
        presented: &str,
        correlation: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation);
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/credentials/authenticate",
                &json!({ "credential": presented }),
            )
            .await
            .map_err(|e: InternalCallError| AuthorityUnavailable(e.to_string()))?;
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed authentication response".into()))?;
        match body.get("outcome").and_then(Value::as_str) {
            Some("AUTHENTICATED") => body
                .get("authority")
                .and_then(AuthenticatedCustomer::from_authority)
                .map(|c| Authentication::Authenticated(Box::new(c)))
                .ok_or_else(|| AuthorityUnavailable("malformed authority chain".into())),
            Some("REJECTED") => Ok(Authentication::Rejected {
                reason: body
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("rejected")
                    .chars()
                    .take(64)
                    .collect(),
                throttle: body
                    .get("throttle")
                    .and_then(|t| serde_json::from_value(t.clone()).ok()),
            }),
            _ => Err(AuthorityUnavailable(
                "unknown authentication outcome".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(kind: &str, classification: &str) -> Value {
        let id = || Uuid::new_v4().to_string();
        json!({
            "authorityMode": "FULL",
            "tenant": { "id": id(), "classification": classification },
            "application": { "id": id() },
            "environment": { "id": id(), "kind": kind },
            "serviceAccount": { "id": id() },
            "credential": { "id": id() },
            "scopes": ["sms.read"],
        })
    }

    #[test]
    fn the_customer_comes_only_from_a_well_formed_authority_chain() {
        let c = AuthenticatedCustomer::from_authority(&chain("SANDBOX", "EXTERNAL")).unwrap();
        assert_eq!(c.classification, TrustClassification::External);
        assert!(c.has_scope("sms.read") && !c.has_scope("sms.send"));
        let d = c.delegated();
        assert_eq!(d.tenant_id(), c.tenant_id);
        assert_eq!(d.application_environment_id(), c.environment_id);
        assert!(AuthenticatedCustomer::from_authority(&chain("STAGING", "EXTERNAL")).is_none());
        assert!(AuthenticatedCustomer::from_authority(&chain("SANDBOX", "PARTNER")).is_none());
        assert!(AuthenticatedCustomer::from_authority(&json!({})).is_none());
    }
}

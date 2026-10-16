//! The trusted request context.
//!
//! Customers present a CF Meet API credential; Numbering does not verify it itself — it
//! asks Platform Access (`POST /internal/v1/credentials/authenticate`, permission
//! `platform-credential.authenticate`) and takes the tenant, application, environment,
//! service account and scopes ONLY from the authority chain Platform Access returns. A
//! tenant or environment id in a request never becomes authority. Platform Access
//! unreachable means the request is refused (fail closed).

use std::{collections::BTreeSet, sync::Arc};

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    services, OutboundServiceRequest, ServiceId,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::numbers::{EnvironmentKind, OwnerScope};

/// The trusted context of an authenticated customer request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedContext {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: EnvironmentKind,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub first_party: bool,
    /// The environment may use live telecom (Platform Access `liveTelecomAllowed`;
    /// absent = false, fail closed).
    pub live_telecom_allowed: bool,
    /// FULL; SAFETY_ONLY contexts are refused by Numbering (it has no safety operation).
    pub full_authority: bool,
    pub scopes: BTreeSet<String>,
}

impl TrustedContext {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    pub fn owner_scope(&self) -> OwnerScope {
        OwnerScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
            environment_kind: self.environment_kind,
            service_account_id: self.service_account_id,
        }
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
            environment_kind: EnvironmentKind::parse(
                authority.get("environment")?.get("kind")?.as_str()?,
            )?,
            service_account_id: id(&["serviceAccount", "id"])?,
            credential_id: id(&["credential", "id"])?,
            first_party: authority.get("tenant")?.get("classification")?.as_str()? == "FIRST_PARTY",
            live_telecom_allowed: authority
                .get("environment")?
                .get("liveTelecomAllowed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
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
    Authenticated(Box<TrustedContext>),
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
    async fn authenticate(&self, presented: &str) -> Result<Authentication, AuthorityUnavailable>;
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

fn platform_access() -> ServiceId {
    ServiceId::parse(services::PLATFORM_ACCESS).expect("canonical identity")
}

#[async_trait::async_trait]
impl CustomerAuthority for PlatformAccessAuthority {
    async fn authenticate(&self, presented: &str) -> Result<Authentication, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(platform_access());
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/credentials/authenticate",
                &json!({ "credential": presented }),
            )
            .await
            .map_err(|e: InternalCallError| AuthorityUnavailable(e.to_string()))?;
        if response.status() != 200 {
            return Err(AuthorityUnavailable(format!(
                "HTTP {} {}",
                response.status(),
                response.error_code().unwrap_or_default()
            )));
        }
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed authentication response".into()))?;
        match body.get("outcome").and_then(Value::as_str) {
            Some("AUTHENTICATED") => body
                .get("authority")
                .and_then(TrustedContext::from_authority)
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

/// An authority that is not configured: every authentication fails closed.
pub struct UnconfiguredAuthority;

#[async_trait::async_trait]
impl CustomerAuthority for UnconfiguredAuthority {
    async fn authenticate(&self, _presented: &str) -> Result<Authentication, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no identity authority is configured".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(kind: &str, mode: &str) -> Value {
        let id = || Uuid::new_v4().to_string();
        json!({
            "authorityMode": mode,
            "tenant": { "id": id(), "classification": "EXTERNAL" },
            "application": { "id": id() },
            "environment": { "id": id(), "kind": kind },
            "serviceAccount": { "id": id() },
            "credential": { "id": id() },
            "scopes": ["numbers.read", "calls.read"],
        })
    }

    #[test]
    fn the_context_comes_only_from_the_authority_chain() {
        let ctx = TrustedContext::from_authority(&chain("SANDBOX", "FULL")).unwrap();
        assert_eq!(ctx.environment_kind, EnvironmentKind::Sandbox);
        assert!(ctx.full_authority && !ctx.first_party);
        assert!(ctx.has_scope("numbers.read") && !ctx.has_scope("numbers.manage"));
        assert!(
            !TrustedContext::from_authority(&chain("SANDBOX", "SAFETY_ONLY"))
                .unwrap()
                .full_authority
        );
        // Anything malformed is not a context.
        assert!(TrustedContext::from_authority(&chain("STAGING", "FULL")).is_none());
        assert!(TrustedContext::from_authority(&json!({})).is_none());
    }
}

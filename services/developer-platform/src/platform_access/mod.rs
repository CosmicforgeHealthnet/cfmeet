//! Integration boundary to Platform Access — the ONLY authority for tenants,
//! applications, environments, service accounts, API credentials, scopes and principals.
//!
//! The Developer Platform never verifies a customer credential and never stores the
//! identity hierarchy. It asks Platform Access over authenticated internal HTTP
//! (`platform-identity.read`):
//!
//! * `GET /internal/v1/identity/context` — re-validates a signed delegated context (the
//!   one the API Gateway derived from Platform Access's own authority chain) against the
//!   authority and returns its canonical ids;
//! * `GET /internal/v1/scopes` — the server-authoritative scope registry.
//!
//! Platform Access unreachable means the request is refused (fail closed).

use std::sync::Arc;

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use serde_json::Value;
use uuid::Uuid;

/// Canonical Platform Access ids of a re-validated application environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalContext {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    /// `SANDBOX` / `TEST` / `PRODUCTION`.
    pub environment_kind: String,
    pub service_account_id: Uuid,
    /// `FULL` / `SAFETY_ONLY`.
    pub authority_mode: String,
}

impl CanonicalContext {
    /// Parses Platform Access's `TrustedContext` (contract `cfmeet.platform-access.identity`).
    pub fn from_trusted_context(body: &Value) -> Option<Self> {
        let id = |a: &str| Uuid::parse_str(body.get(a)?.get("id")?.as_str()?).ok();
        let kind = body.get("environment")?.get("kind")?.as_str()?;
        let mode = body.get("authorityMode")?.as_str()?;
        if !matches!(kind, "SANDBOX" | "TEST" | "PRODUCTION")
            || !matches!(mode, "FULL" | "SAFETY_ONLY")
        {
            return None;
        }
        Some(Self {
            tenant_id: id("tenant")?,
            application_id: id("application")?,
            environment_id: id("environment")?,
            environment_kind: kind.to_string(),
            service_account_id: id("serviceAccount")?,
            authority_mode: mode.to_string(),
        })
    }

    /// Whether Platform Access confirmed exactly the delegated chain.
    pub fn matches(&self, delegated: &DelegatedPlatformContext) -> bool {
        self.tenant_id == delegated.tenant_id()
            && self.application_id == delegated.application_id()
            && self.environment_id == delegated.application_environment_id()
            && self.service_account_id == delegated.initiating_service_account_id()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextOutcome {
    Confirmed(CanonicalContext),
    /// Platform Access refused the delegated context (forged, stale, suspended scope).
    Refused {
        code: String,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("the identity authority is unavailable: {0}")]
pub struct AuthorityUnavailable(pub String);

#[async_trait::async_trait]
pub trait PlatformAccessPort: Send + Sync {
    /// Re-validates `delegated` against the authority (continuing `correlation`).
    async fn trusted_context(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
    ) -> Result<ContextOutcome, AuthorityUnavailable>;

    /// Names of the registered scopes.
    async fn scope_names(&self) -> Result<Vec<String>, AuthorityUnavailable>;
}

/// Platform Access over authenticated internal HTTP.
pub struct HttpPlatformAccess {
    client: Arc<InternalServiceClient>,
}

impl HttpPlatformAccess {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

fn unavailable(e: InternalCallError) -> AuthorityUnavailable {
    AuthorityUnavailable(e.to_string())
}

#[async_trait::async_trait]
impl PlatformAccessPort for HttpPlatformAccess {
    async fn trusted_context(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
    ) -> Result<ContextOutcome, AuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation)
            .with_delegated_context(delegated.clone());
        let response = self
            .client
            .exchange_json::<Value>(
                &request,
                axum::http::Method::GET,
                "/internal/v1/identity/context",
                None,
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            200 => {
                let body: Value = response
                    .json()
                    .map_err(|_| AuthorityUnavailable("malformed trusted context".into()))?;
                CanonicalContext::from_trusted_context(&body)
                    .map(ContextOutcome::Confirmed)
                    .ok_or_else(|| AuthorityUnavailable("malformed trusted context".into()))
            }
            400 | 403 | 404 => Ok(ContextOutcome::Refused {
                code: response
                    .error_code()
                    .unwrap_or_else(|| "DELEGATED_CONTEXT_INVALID".into())
                    .chars()
                    .take(64)
                    .collect(),
            }),
            status => Err(AuthorityUnavailable(format!(
                "HTTP {status} {}",
                response.error_code().unwrap_or_default()
            ))),
        }
    }

    async fn scope_names(&self) -> Result<Vec<String>, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(ServiceId::platform_access());
        let response = self
            .client
            .get(&request, "/internal/v1/scopes")
            .await
            .map_err(unavailable)?;
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed scope registry".into()))?;
        Ok(body
            .get("scopes")
            .and_then(Value::as_array)
            .map(|scopes| {
                scopes
                    .iter()
                    .filter_map(|s| s.get("name").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// No Platform Access configured: everything fails closed.
pub struct UnconfiguredPlatformAccess;

#[async_trait::async_trait]
impl PlatformAccessPort for UnconfiguredPlatformAccess {
    async fn trusted_context(
        &self,
        _delegated: &DelegatedPlatformContext,
        _correlation: &CorrelationId,
    ) -> Result<ContextOutcome, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no identity authority is configured".into(),
        ))
    }

    async fn scope_names(&self) -> Result<Vec<String>, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no identity authority is configured".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_a_well_formed_trusted_context_is_canonical() {
        let id = || Uuid::new_v4().to_string();
        let body = json!({
            "authorityMode": "FULL", "suspendedLink": null,
            "tenant": { "id": id() }, "application": { "id": id() },
            "environment": { "id": id(), "kind": "SANDBOX" }, "serviceAccount": { "id": id() },
        });
        let ctx = CanonicalContext::from_trusted_context(&body).unwrap();
        assert_eq!(ctx.environment_kind, "SANDBOX");
        let mut bad = body.clone();
        bad["environment"]["kind"] = json!("STAGING");
        assert!(CanonicalContext::from_trusted_context(&bad).is_none());
        assert!(CanonicalContext::from_trusted_context(&json!({})).is_none());
    }
}

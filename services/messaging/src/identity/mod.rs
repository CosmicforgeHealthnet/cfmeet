//! The trusted request context (Part V Workstream B / C.8).
//!
//! * Customers present a CF Meet API credential. Messaging never verifies it itself: it
//!   asks Platform Access (`POST /internal/v1/credentials/authenticate`,
//!   `platform-credential.authenticate`) and takes tenant, application, environment,
//!   service account, effective scopes and the authorized-principal restriction ONLY from
//!   the authority chain returned. A tenant / environment id in a request is never
//!   authority. Platform Access unreachable = refused (fail closed).
//! * The ACTING principal of a request is a claim of the authenticated application
//!   backend (`CF-Acting-Principal`). It is honoured only when the credential's principal
//!   restriction admits it AND Platform Access resolves it ACTIVE, of a messaging-capable
//!   kind, inside the credential's own environment (`POST /internal/v1/principals/resolve`
//!   with the signed delegated context) — fresh, on every request.
//! * Caching ([`cache`]): positive authentications may be reused for READ routes for at
//!   most `MESSAGING_AUTH_CACHE_TTL_SECONDS`, flushed on any identity change-feed entry
//!   and disabled while the feed is unreadable. Mutations always authenticate afresh.

pub mod cache;

use std::{collections::BTreeSet, sync::Arc};

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    services, OutboundServiceRequest, ServiceId,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// Kind of an application environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentKind {
    Sandbox,
    Test,
    Production,
}

impl EnvironmentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnvironmentKind::Sandbox => "SANDBOX",
            EnvironmentKind::Test => "TEST",
            EnvironmentKind::Production => "PRODUCTION",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "SANDBOX" => Some(EnvironmentKind::Sandbox),
            "TEST" => Some(EnvironmentKind::Test),
            "PRODUCTION" => Some(EnvironmentKind::Production),
            _ => None,
        }
    }
}

/// The trusted application environment of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OwnerScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: EnvironmentKind,
    pub service_account_id: Uuid,
}

impl OwnerScope {
    /// The signed delegated context of this scope.
    pub fn delegated(&self) -> cf_service_auth::DelegatedPlatformContext {
        use cf_service_auth::{ApplicationEnvironmentKind as K, DelegatedPlatformContext};
        let kind = match self.environment_kind {
            EnvironmentKind::Sandbox => K::Sandbox,
            EnvironmentKind::Test => K::Test,
            EnvironmentKind::Production => K::Production,
        };
        DelegatedPlatformContext::new(
            self.tenant_id,
            self.application_id,
            self.environment_id,
            kind,
            self.service_account_id,
        )
    }
}

/// The trusted context of an authenticated customer request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedContext {
    pub scope: OwnerScope,
    pub credential_id: Uuid,
    pub first_party: bool,
    /// FULL; SAFETY_ONLY (a suspended tenant / application / environment) is refused.
    pub full_authority: bool,
    pub scopes: BTreeSet<String>,
    /// `Some`: the credential may act only for these principals.
    pub principal_restriction: Option<BTreeSet<Uuid>>,
}

impl TrustedContext {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    /// Whether the credential may act for `principal` (restriction only; Platform Access
    /// still resolves the principal).
    pub fn may_act_for(&self, principal: Uuid) -> bool {
        self.principal_restriction
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&principal))
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
            .map(|s| s.as_str().map(str::to_string))
            .collect::<Option<BTreeSet<_>>>()?;
        let principal_restriction = match authority.get("principalRestriction") {
            None | Some(Value::Null) => None,
            Some(Value::Array(ids)) => Some(
                ids.iter()
                    .map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .collect::<Option<BTreeSet<_>>>()?,
            ),
            Some(_) => return None,
        };
        Some(Self {
            scope: OwnerScope {
                tenant_id: id(&["tenant", "id"])?,
                application_id: id(&["application", "id"])?,
                environment_id: id(&["environment", "id"])?,
                environment_kind: EnvironmentKind::parse(
                    authority.get("environment")?.get("kind")?.as_str()?,
                )?,
                service_account_id: id(&["serviceAccount", "id"])?,
            },
            credential_id: id(&["credential", "id"])?,
            first_party: authority.get("tenant")?.get("classification")?.as_str()? == "FIRST_PARTY",
            full_authority: authority.get("authorityMode")?.as_str()? == "FULL",
            scopes,
            principal_restriction,
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

fn platform_access() -> ServiceId {
    ServiceId::parse(services::PLATFORM_ACCESS).expect("canonical identity")
}

fn unavailable(e: InternalCallError) -> AuthorityUnavailable {
    AuthorityUnavailable(e.to_string())
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
            .map_err(unavailable)?;
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

/// Principal categories that may take part in Messaging (Part V B.4). Telephone
/// endpoints (SMS Bridge, later) and service accounts are not messaging principals.
pub const MESSAGING_PRINCIPAL_KINDS: &[&str] = &[
    "NATIVE_CF_USER",
    "EXTERNAL_APPLICATION_PRINCIPAL",
    "ENTERPRISE_MEMBER",
    "TEMPORARY_EXTERNAL_PARTICIPANT",
    "BOT_OR_SERVICE",
];

/// Platform Access's answer about one principal of the caller's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalStatus {
    /// ACTIVE, of `kind`.
    Active { kind: String },
    /// Suspended, deactivated or tombstoned.
    Inactive,
    /// Unknown in this scope (another tenant's principal is reported the same way).
    NotFound,
}

impl PrincipalStatus {
    /// Active and of a messaging-capable kind.
    pub fn is_messaging_capable(&self) -> bool {
        matches!(self, PrincipalStatus::Active { kind } if MESSAGING_PRINCIPAL_KINDS.contains(&kind.as_str()))
    }
}

/// Resolves communication principals in the caller's TRUSTED scope.
#[async_trait::async_trait]
pub trait PrincipalDirectory: Send + Sync {
    async fn resolve(
        &self,
        scope: &OwnerScope,
        principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable>;
}

/// Platform Access (`POST /internal/v1/principals/resolve`, `platform-principal.resolve`,
/// signed delegated context of the trusted scope).
pub struct PlatformAccessPrincipals {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessPrincipals {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl PrincipalDirectory for PlatformAccessPrincipals {
    async fn resolve(
        &self,
        scope: &OwnerScope,
        principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(platform_access())
            .with_delegated_context(scope.delegated());
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/principals/resolve",
                &json!({ "references": [{ "principalId": principal_id }] }),
            )
            .await
            .map_err(unavailable)?;
        if response.status() != 200 {
            return Err(AuthorityUnavailable(format!(
                "HTTP {} {}",
                response.status(),
                response.error_code().unwrap_or_default()
            )));
        }
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed resolution response".into()))?;
        let result = &body["results"][0];
        match result["outcome"].as_str() {
            Some("RESOLVED") => {
                // Defence in depth: the resolved principal must be the one asked for, in
                // this environment.
                let p = &result["principal"];
                let same = p["id"].as_str() == Some(principal_id.to_string().as_str())
                    && p["environmentId"].as_str()
                        == Some(scope.environment_id.to_string().as_str());
                match (same, p["kind"].as_str(), p["status"].as_str()) {
                    (true, Some(kind), Some("ACTIVE")) => Ok(PrincipalStatus::Active {
                        kind: kind.chars().take(48).collect(),
                    }),
                    (true, Some(_), Some(_)) => Ok(PrincipalStatus::Inactive),
                    _ => Err(AuthorityUnavailable(
                        "inconsistent principal resolution".into(),
                    )),
                }
            }
            Some("INACTIVE") => Ok(PrincipalStatus::Inactive),
            Some("NOT_FOUND" | "INVALID_REFERENCE") => Ok(PrincipalStatus::NotFound),
            _ => Err(AuthorityUnavailable("unknown resolution outcome".into())),
        }
    }
}

/// An authority / directory that is not configured: everything fails closed.
pub struct Unconfigured;

#[async_trait::async_trait]
impl CustomerAuthority for Unconfigured {
    async fn authenticate(&self, _presented: &str) -> Result<Authentication, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no identity authority is configured".into(),
        ))
    }
}

#[async_trait::async_trait]
impl PrincipalDirectory for Unconfigured {
    async fn resolve(
        &self,
        _scope: &OwnerScope,
        _principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no principal directory is configured".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(kind: &str, mode: &str, restriction: Value) -> Value {
        let id = || Uuid::new_v4().to_string();
        json!({
            "authorityMode": mode,
            "tenant": { "id": id(), "classification": "EXTERNAL" },
            "application": { "id": id() },
            "environment": { "id": id(), "kind": kind },
            "serviceAccount": { "id": id() },
            "credential": { "id": id() },
            "scopes": ["messages.read", "calls.read"],
            "principalRestriction": restriction,
        })
    }

    #[test]
    fn the_context_comes_only_from_the_authority_chain() {
        let ctx = TrustedContext::from_authority(&chain("SANDBOX", "FULL", Value::Null)).unwrap();
        assert_eq!(ctx.scope.environment_kind, EnvironmentKind::Sandbox);
        assert!(ctx.full_authority && !ctx.first_party);
        assert!(ctx.has_scope("messages.read") && !ctx.has_scope("messages.send"));
        assert!(ctx.may_act_for(Uuid::new_v4()));
        assert!(
            !TrustedContext::from_authority(&chain("SANDBOX", "SAFETY_ONLY", Value::Null))
                .unwrap()
                .full_authority
        );
        assert!(TrustedContext::from_authority(&chain("STAGING", "FULL", Value::Null)).is_none());
        assert!(TrustedContext::from_authority(&json!({})).is_none());
    }

    #[test]
    fn principal_restrictions_are_honoured_and_malformed_ones_refused() {
        let allowed = Uuid::new_v4();
        let ctx = TrustedContext::from_authority(&chain("TEST", "FULL", json!([allowed]))).unwrap();
        assert!(ctx.may_act_for(allowed));
        assert!(!ctx.may_act_for(Uuid::new_v4()));
        // An empty restriction admits nobody.
        let none = TrustedContext::from_authority(&chain("TEST", "FULL", json!([]))).unwrap();
        assert!(!none.may_act_for(allowed));
        assert!(
            TrustedContext::from_authority(&chain("TEST", "FULL", json!(["not-a-uuid"]))).is_none()
        );
        assert!(TrustedContext::from_authority(&chain("TEST", "FULL", json!("*"))).is_none());
    }

    #[test]
    fn only_active_messaging_kinds_are_capable() {
        let active = |k: &str| PrincipalStatus::Active { kind: k.into() };
        assert!(active("EXTERNAL_APPLICATION_PRINCIPAL").is_messaging_capable());
        assert!(active("BOT_OR_SERVICE").is_messaging_capable());
        assert!(!active("TELEPHONE_ENDPOINT").is_messaging_capable());
        assert!(!active("SERVICE_ACCOUNT").is_messaging_capable());
        assert!(!PrincipalStatus::Inactive.is_messaging_capable());
        assert!(!PrincipalStatus::NotFound.is_messaging_capable());
    }
}

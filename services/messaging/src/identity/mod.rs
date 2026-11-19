//! The trusted request context (Part V Workstream B / C.8).
//!
//! * Customers present a CF Meet API credential. Messaging never verifies it itself: it
//!   asks Platform Access (`POST /internal/v1/credentials/authenticate`,
//!   `platform-credential.authenticate`) and takes tenant, application, environment,
//!   service account, effective scopes and the authorized-principal restriction ONLY from
//!   the authority chain returned. A tenant / environment id in a request is never
//!   authority. Platform Access unreachable = refused (fail closed).
//! * Part VI §51 — a request the public API Gateway already authenticated arrives on the
//!   SAME public route without `Authorization`, carrying instead a fresh gateway service
//!   token (`messaging-request.forward`) whose signed delegated context names the
//!   initiating credential id. Messaging still never trusts that context as authority: it
//!   asks Platform Access for the LIVE chain of that credential
//!   (`POST /internal/v1/credentials/delegated-authority`, the same permission and the
//!   same response shape as `/credentials/authenticate`, the delegated context attached to
//!   Messaging's own service token) and takes everything from the chain returned
//!   ([`CustomerAuthority::authenticate_forwarded`]). Revocation and suspension therefore
//!   apply at once; forwarded requests are never served from the cache.
//! * The ACTING principal of a request is a claim of the authenticated application
//!   backend (`CF-Acting-Principal`). It is honoured only when the credential's principal
//!   restriction admits it AND Platform Access resolves it ACTIVE, of a messaging-capable
//!   kind, inside the credential's own environment (`POST /internal/v1/principals/resolve`
//!   with the signed delegated context) — fresh, on every request.
//! * Caching ([`cache`]): positive authentications may be reused for READ routes for at
//!   most `MESSAGING_AUTH_CACHE_TTL_SECONDS`, flushed on any identity change-feed entry
//!   and disabled while the feed is unreadable. Mutations always authenticate afresh.
//! * CF Meet Apps (ADR 0064, integrate contract §2 / §7): Platform Access also
//!   authenticates OAuth access (`cfoa_`) and bot (`cfob_`) tokens and reports
//!   `credentialKind` (`OAUTH_ACCESS` | `OAUTH_BOT`), `appId` and `grantId`
//!   ([`OAuthCredential`], parsed tolerantly: absent = an ordinary credential; an
//!   `OAUTH_*` kind without well-formed ids = a malformed chain, fail closed).

pub mod cache;

use std::{collections::BTreeSet, sync::Arc};

use cf_service_auth::{
    client::{InternalCallError, InternalResponse, InternalServiceClient},
    services, CorrelationId, OutboundServiceRequest, ServiceId,
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

/// How an OAuth credential acts (integrate contract §0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthKind {
    /// `OAUTH_ACCESS` (`cfoa_`): an app acting for the granting CF Meet user.
    Access,
    /// `OAUTH_BOT` (`cfob_`): an app's organization bot principal.
    Bot,
}

impl OAuthKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OAuthKind::Access => "OAUTH_ACCESS",
            OAuthKind::Bot => "OAUTH_BOT",
        }
    }
}

/// OAuth facts that name an `OAUTH_*` kind but are not well formed (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedOAuth;

/// The CF Meet App an authenticated credential belongs to (ADR 0064): the credential's
/// kind, the app (OAuth `client_id` = the app's application id) and the grant
/// (installation). Taken ONLY from Platform Access's authentication answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthCredential {
    pub kind: OAuthKind,
    pub app_id: Uuid,
    pub grant_id: Uuid,
}

impl OAuthCredential {
    /// Reads the OAuth facts of an authentication answer. Platform Access may report them
    /// on the authority chain (`credentialKind`, `appId`, `grantId`), in a nested `oauth`
    /// object, or beside the chain in the response: the first place that names a
    /// `credentialKind` wins. `Ok(None)`: an ordinary credential (no kind, or a non-OAuth
    /// kind). `Err`: an `OAUTH_*` kind that is unknown or lacks well-formed ids
    /// (fail closed).
    pub fn parse(places: &[&Value]) -> Result<Option<Self>, MalformedOAuth> {
        for place in places {
            // On the object itself `credentialKind`; in a nested `oauth` object also `kind`.
            let nested = &place["oauth"];
            let candidates = [
                (*place, place.get("credentialKind")),
                (
                    nested,
                    nested.get("credentialKind").or_else(|| nested.get("kind")),
                ),
            ];
            for (v, kind) in candidates {
                let Some(kind) = kind else { continue };
                let kind = match kind.as_str().ok_or(MalformedOAuth)? {
                    "OAUTH_ACCESS" => OAuthKind::Access,
                    "OAUTH_BOT" => OAuthKind::Bot,
                    k if k.starts_with("OAUTH_") => return Err(MalformedOAuth),
                    _ => return Ok(None),
                };
                let id = |name: &str| {
                    v.get(name)
                        .and_then(Value::as_str)
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .ok_or(MalformedOAuth)
                };
                return Ok(Some(Self {
                    kind,
                    app_id: id("appId")?,
                    grant_id: id("grantId")?,
                }));
            }
        }
        Ok(None)
    }
}

/// The trusted context of an authenticated customer request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedContext {
    pub scope: OwnerScope,
    pub credential_id: Uuid,
    /// Authoritative device-session creation time; never taken from consumer JSON.
    pub personal_session_issued_at: Option<i64>,
    pub first_party: bool,
    /// FULL; SAFETY_ONLY (a suspended tenant / application / environment) is refused.
    pub full_authority: bool,
    pub scopes: BTreeSet<String>,
    /// `Some`: the credential may act only for these principals.
    pub principal_restriction: Option<BTreeSet<Uuid>>,
    /// `Some`: a CF Meet App's OAuth credential (ADR 0064; only with
    /// `MESSAGING_APPS_ENABLED`, refused otherwise).
    pub oauth: Option<OAuthCredential>,
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

    /// Whether this chain is the one of the credential and scope a signed delegated
    /// context names (tenant, application, environment and its kind, service account and
    /// initiating credential).
    pub fn is_chain_of(&self, ctx: &cf_service_auth::DelegatedPlatformContext) -> bool {
        let own = self.scope.delegated();
        ctx.initiating_credential_id() == Some(self.credential_id)
            && ctx.tenant_id() == own.tenant_id()
            && ctx.application_id() == own.application_id()
            && ctx.application_environment_id() == own.application_environment_id()
            && ctx.application_environment_kind() == own.application_environment_kind()
            && ctx.initiating_service_account_id() == own.initiating_service_account_id()
    }

    /// The single principal an OAuth credential is restricted to (`None` when the
    /// restriction is absent or does not name exactly one principal).
    pub fn oauth_principal(&self) -> Option<Uuid> {
        match self.principal_restriction.as_ref() {
            Some(allowed) if allowed.len() == 1 => allowed.iter().next().copied(),
            _ => None,
        }
    }

    /// Parses Platform Access's `AuthenticateResponse` body (`outcome` AUTHENTICATED):
    /// the authority chain, and the OAuth facts wherever Platform Access reports them.
    pub fn from_response(body: &Value) -> Option<Self> {
        let authority = body.get("authority")?;
        let mut ctx = Self::from_authority(authority)?;
        if ctx.oauth.is_none() {
            ctx.oauth = OAuthCredential::parse(&[body]).ok()?;
        }
        Some(ctx)
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
            personal_session_issued_at: None,
            first_party: authority.get("tenant")?.get("classification")?.as_str()? == "FIRST_PARTY",
            full_authority: authority.get("authorityMode")?.as_str()? == "FULL",
            scopes,
            principal_restriction,
            oauth: OAuthCredential::parse(&[authority]).ok()?,
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

/// Platform Access's verdict on a signed delegated scope a peer service presented
/// (`GET /internal/v1/identity/context`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegatedScope {
    /// The tenant / application / environment / service account chain exists.
    Valid {
        first_party: bool,
        full_authority: bool,
    },
    /// The chain does not exist (forged or stale scope).
    Invalid,
}

/// Standard Messaging scopes a delegated first-party backend acts with
/// (`messaging-application.delegate`). Never moderation, export or administration.
pub const DELEGATED_SCOPES: &[&str] = &[
    "messages.send",
    "messages.read",
    "conversations.create",
    "conversations.manage",
    "attachments.write",
    "attachments.read",
];

impl TrustedContext {
    /// The trusted context of a peer service acting as the delegated backend of its own
    /// application environment. The credential id is the initiating service account (the
    /// audit actor); the principal restriction is none (principals are still resolved).
    pub fn delegated(
        ctx: &cf_service_auth::DelegatedPlatformContext,
        first_party: bool,
        full_authority: bool,
    ) -> Self {
        use cf_service_auth::ApplicationEnvironmentKind as K;
        Self {
            scope: OwnerScope {
                tenant_id: ctx.tenant_id(),
                application_id: ctx.application_id(),
                environment_id: ctx.application_environment_id(),
                environment_kind: match ctx.application_environment_kind() {
                    K::Sandbox => EnvironmentKind::Sandbox,
                    K::Test => EnvironmentKind::Test,
                    K::Production => EnvironmentKind::Production,
                },
                service_account_id: ctx.initiating_service_account_id(),
            },
            credential_id: ctx.initiating_service_account_id(),
            personal_session_issued_at: None,
            first_party,
            full_authority,
            scopes: DELEGATED_SCOPES.iter().map(|s| s.to_string()).collect(),
            principal_restriction: None,
            oauth: None,
        }
    }
}

#[async_trait::async_trait]
pub trait CustomerAuthority: Send + Sync {
    async fn authenticate(&self, presented: &str) -> Result<Authentication, AuthorityUnavailable>;

    /// Authenticates, LIVE, the credential an API Gateway forwarded request names in its
    /// verified delegated context (`initiating_credential_id`): the same outcomes as
    /// [`Self::authenticate`], the chain re-fetched by the identity authority and required
    /// to match the delegated scope. `correlation_id` continues the gateway's operation.
    /// Fail closed by default.
    async fn authenticate_forwarded(
        &self,
        _ctx: &cf_service_auth::DelegatedPlatformContext,
        _correlation_id: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "forwarded authentication is not supported".into(),
        ))
    }

    /// Re-validates a delegated scope against Platform Access (fail closed by default).
    async fn validate_delegated(
        &self,
        _ctx: &cf_service_auth::DelegatedPlatformContext,
    ) -> Result<DelegatedScope, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "delegated scopes are not supported".into(),
        ))
    }
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

/// Platform Access's `AuthenticateResponse` (both `/credentials/authenticate` and
/// `/credentials/delegated-authority` answer it): AUTHENTICATED with the chain, or
/// REJECTED with a reason (logs only) and an optional throttle. Anything else is an
/// unavailable authority (fail closed).
fn authentication_outcome(
    response: &InternalResponse,
) -> Result<Authentication, AuthorityUnavailable> {
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
        Some("AUTHENTICATED") => TrustedContext::from_response(&body)
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
        authentication_outcome(&response)
    }

    /// `POST /internal/v1/credentials/delegated-authority` (`platform-credential.authenticate`):
    /// no body; Platform Access reads the credential id from the delegated context signed
    /// into Messaging's own service token.
    async fn authenticate_forwarded(
        &self,
        ctx: &cf_service_auth::DelegatedPlatformContext,
        correlation_id: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(platform_access(), correlation_id)
            .with_delegated_context(ctx.clone());
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/credentials/delegated-authority",
                &json!({}),
            )
            .await
            .map_err(unavailable)?;
        match authentication_outcome(&response)? {
            // Defence in depth: the chain must be the delegated credential's, in the
            // delegated scope (Platform Access checks the same).
            Authentication::Authenticated(chain) if !chain.is_chain_of(ctx) => Err(
                AuthorityUnavailable("inconsistent delegated authority chain".into()),
            ),
            outcome => Ok(outcome),
        }
    }

    async fn validate_delegated(
        &self,
        ctx: &cf_service_auth::DelegatedPlatformContext,
    ) -> Result<DelegatedScope, AuthorityUnavailable> {
        self.identity_context(ctx).await
    }
}

impl PlatformAccessAuthority {
    async fn identity_context(
        &self,
        ctx: &cf_service_auth::DelegatedPlatformContext,
    ) -> Result<DelegatedScope, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(platform_access())
            .with_delegated_context(ctx.clone());
        let response = self
            .client
            .get(&request, "/internal/v1/identity/context")
            .await
            .map_err(unavailable)?;
        match response.status() {
            200 => {}
            403 if response.error_code().as_deref() == Some("DELEGATED_CONTEXT_INVALID") => {
                return Ok(DelegatedScope::Invalid)
            }
            s => {
                return Err(AuthorityUnavailable(format!(
                    "HTTP {s} {}",
                    response.error_code().unwrap_or_default()
                )))
            }
        }
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed identity context".into()))?;
        let first_party = body
            .get("tenant")
            .and_then(|t| t.get("classification"))
            .and_then(Value::as_str)
            == Some("FIRST_PARTY");
        let full_authority = body.get("authorityMode").and_then(Value::as_str) == Some("FULL");
        Ok(DelegatedScope::Valid {
            first_party,
            full_authority,
        })
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

    /// The external reference of each of `principals` that is an ACTIVE NATIVE_CF_USER of
    /// `scope` (a CF Meet account principal: `cfmeet-user:<account id>`, set by the
    /// account authority). Principals of any other kind, inactive or unknown, are absent.
    /// Callers still validate the reference's shape. Default: none known.
    async fn native_user_references(
        &self,
        _scope: &OwnerScope,
        _principals: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>, AuthorityUnavailable> {
        Ok(std::collections::HashMap::new())
    }

    /// ADR 0064: the app of each of `principals` that is an ACTIVE `BOT_OR_SERVICE`
    /// principal of `scope` whose external reference is `app:<appId>` (a CF Meet App's
    /// organization bot). Every other principal is absent. Default: none known.
    async fn application_bots(
        &self,
        _scope: &OwnerScope,
        _principals: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Uuid>, AuthorityUnavailable> {
        Ok(std::collections::HashMap::new())
    }
}

/// The app id of a bot principal's external reference `app:<uuid>` (canonical form).
pub fn application_bot_reference(reference: &str) -> Option<Uuid> {
    let id = reference.strip_prefix("app:")?;
    Uuid::parse_str(id)
        .ok()
        .filter(|u| u.hyphenated().to_string() == id)
}

/// Most principals in one Platform Access resolution request.
pub const RESOLVE_BATCH: usize = 50;

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

    async fn native_user_references(
        &self,
        scope: &OwnerScope,
        principals: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>, AuthorityUnavailable> {
        self.active_references(scope, principals, "NATIVE_CF_USER")
            .await
    }

    async fn application_bots(
        &self,
        scope: &OwnerScope,
        principals: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Uuid>, AuthorityUnavailable> {
        Ok(self
            .active_references(scope, principals, "BOT_OR_SERVICE")
            .await?
            .into_iter()
            .filter_map(|(p, reference)| application_bot_reference(&reference).map(|app| (p, app)))
            .collect())
    }
}

impl PlatformAccessPrincipals {
    /// The external reference of each of `principals` that is an ACTIVE principal of
    /// `kind` in `scope` (batched resolution; defence in depth on every answer).
    async fn active_references(
        &self,
        scope: &OwnerScope,
        principals: &[Uuid],
        kind: &str,
    ) -> Result<std::collections::HashMap<Uuid, String>, AuthorityUnavailable> {
        let mut out = std::collections::HashMap::new();
        let environment = scope.environment_id.to_string();
        for batch in principals.chunks(RESOLVE_BATCH) {
            let request = OutboundServiceRequest::new_operation(platform_access())
                .with_delegated_context(scope.delegated());
            let references: Vec<Value> =
                batch.iter().map(|p| json!({ "principalId": p })).collect();
            let response = self
                .client
                .post_json(
                    &request,
                    "/internal/v1/principals/resolve",
                    &json!({ "references": references }),
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
            let results = body["results"]
                .as_array()
                .filter(|r| r.len() == batch.len())
                .ok_or_else(|| AuthorityUnavailable("malformed resolution response".into()))?;
            for (asked, result) in batch.iter().zip(results) {
                match result["outcome"].as_str() {
                    Some("RESOLVED") => {
                        // Defence in depth, as in `resolve`.
                        let p = &result["principal"];
                        if p["id"].as_str() != Some(asked.to_string().as_str())
                            || p["environmentId"].as_str() != Some(environment.as_str())
                        {
                            return Err(AuthorityUnavailable(
                                "inconsistent principal resolution".into(),
                            ));
                        }
                        if let (Some(k), Some("ACTIVE"), Some(reference)) = (
                            p["kind"].as_str(),
                            p["status"].as_str(),
                            p["externalReference"].as_str(),
                        ) {
                            if k == kind {
                                out.insert(*asked, reference.chars().take(255).collect());
                            }
                        }
                    }
                    Some("INACTIVE" | "NOT_FOUND" | "INVALID_REFERENCE") => {}
                    _ => return Err(AuthorityUnavailable("unknown resolution outcome".into())),
                }
            }
        }
        Ok(out)
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

    async fn native_user_references(
        &self,
        _scope: &OwnerScope,
        _principals: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>, AuthorityUnavailable> {
        Err(AuthorityUnavailable(
            "no principal directory is configured".into(),
        ))
    }

    async fn application_bots(
        &self,
        _scope: &OwnerScope,
        _principals: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Uuid>, AuthorityUnavailable> {
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
    fn oauth_credentials_are_parsed_tolerantly_and_fail_closed() {
        let (app, grant) = (Uuid::new_v4(), Uuid::new_v4());
        // Absent: an ordinary credential (today's chains are unchanged).
        let plain = TrustedContext::from_authority(&chain("TEST", "FULL", Value::Null)).unwrap();
        assert_eq!(plain.oauth, None);
        // On the chain.
        let mut c = chain("TEST", "FULL", json!([Uuid::new_v4()]));
        c["credentialKind"] = json!("OAUTH_ACCESS");
        c["appId"] = json!(app);
        c["grantId"] = json!(grant);
        let ctx = TrustedContext::from_authority(&c).unwrap();
        assert_eq!(
            ctx.oauth,
            Some(OAuthCredential {
                kind: OAuthKind::Access,
                app_id: app,
                grant_id: grant
            })
        );
        assert!(ctx.oauth_principal().is_some());
        // Beside the chain in the response, or nested.
        let body = json!({ "outcome": "AUTHENTICATED", "authority": chain("TEST", "FULL", json!([])),
            "credentialKind": "OAUTH_BOT", "appId": app, "grantId": grant });
        let ctx = TrustedContext::from_response(&body).unwrap();
        assert_eq!(ctx.oauth.unwrap().kind, OAuthKind::Bot);
        assert_eq!(
            ctx.oauth_principal(),
            None,
            "an empty restriction names nobody"
        );
        let mut nested = chain("TEST", "FULL", Value::Null);
        nested["oauth"] = json!({ "kind": "OAUTH_BOT", "appId": app, "grantId": grant });
        assert!(TrustedContext::from_authority(&nested)
            .unwrap()
            .oauth
            .is_some());
        // A non-OAuth kind is an ordinary credential.
        let mut key = chain("TEST", "FULL", Value::Null);
        key["credentialKind"] = json!("API_KEY");
        assert_eq!(TrustedContext::from_authority(&key).unwrap().oauth, None);
        // Malformed OAuth facts fail closed.
        for bad in [
            json!({ "credentialKind": "OAUTH_ACCESS", "appId": app }),
            json!({ "credentialKind": "OAUTH_ACCESS", "appId": "x", "grantId": grant }),
            json!({ "credentialKind": "OAUTH_REFRESH", "appId": app, "grantId": grant }),
            json!({ "credentialKind": 7 }),
        ] {
            let mut c = chain("TEST", "FULL", Value::Null);
            for (k, v) in bad.as_object().unwrap() {
                c[k] = v.clone();
            }
            assert!(TrustedContext::from_authority(&c).is_none(), "{bad}");
        }
    }

    #[test]
    fn application_bot_references_are_canonical() {
        let app = Uuid::new_v4();
        assert_eq!(application_bot_reference(&format!("app:{app}")), Some(app));
        assert_eq!(
            application_bot_reference(&format!("app:{}", app.simple())),
            None
        );
        assert_eq!(application_bot_reference("cfmeet:meeting-system"), None);
        assert_eq!(
            application_bot_reference(&format!("cfmeet-user:{app}")),
            None
        );
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

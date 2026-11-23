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

    /// The registry entries (`name`, `description`, `trust`).
    async fn scope_registry(&self) -> Result<Vec<Value>, AuthorityUnavailable> {
        Ok(self
            .scope_names()
            .await?
            .into_iter()
            .map(|n| serde_json::json!({ "name": n, "description": null, "trust": "STANDARD" }))
            .collect())
    }

    /// Whether the credential of the delegated scope holds ALL `scopes` now (Platform
    /// Access `POST /internal/v1/authorization/evaluate`).
    async fn credential_allows(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
        credential_id: Uuid,
        scopes: &[&str],
    ) -> Result<bool, AuthorityUnavailable> {
        let _ = (delegated, correlation, credential_id, scopes);
        Err(AuthorityUnavailable("not supported".into()))
    }

    /// One delegated identity operation (`/internal/v1/delegation/**`).
    async fn delegate(
        &self,
        call: DelegationCall,
        correlation: &CorrelationId,
    ) -> Result<Delegated, AuthorityUnavailable> {
        let _ = (call, correlation);
        Err(AuthorityUnavailable("not supported".into()))
    }

    /// One Partner Users API operation (build contract §2.1, `platform-end-users.delegate`)
    /// on behalf of the gateway-forwarded credential: Platform Access re-verifies it live
    /// and takes the environment from it.
    async fn end_users(
        &self,
        call: EndUserCall,
        forwarded: &ForwardedCredential,
        correlation: &CorrelationId,
    ) -> Result<EndUsersReply, AuthorityUnavailable> {
        let _ = (call, forwarded, correlation);
        Err(AuthorityUnavailable("not supported".into()))
    }
}

/// The API credential the API Gateway authenticated, as carried by the signed delegated
/// context (build contract §2.1: `CF-Forwarded-Credential-Id`).
#[derive(Debug, Clone)]
pub struct ForwardedCredential {
    pub delegated: DelegatedPlatformContext,
    pub credential_id: Uuid,
}

/// The request header naming the forwarded credential (build contract §2.1).
pub const FORWARDED_CREDENTIAL_HEADER: &str = "cf-forwarded-credential-id";

/// A Partner Users API operation (build contract §2.1). Bodies are validated by the
/// caller ([`crate::control::users`]) and forwarded as built there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndUserCall {
    /// `POST /internal/v1/end-users` (`principals.write`; idempotent on `externalId`).
    Create { body: Value },
    /// `GET /internal/v1/end-users` (`principals.read`).
    List {
        external_id: Option<String>,
        cursor: Option<String>,
        limit: Option<u32>,
    },
    /// `GET /internal/v1/end-users/{userId}` (`principals.read`).
    Get { user_id: Uuid },
    /// `PATCH /internal/v1/end-users/{userId}` (`principals.write`).
    Update { user_id: Uuid, body: Value },
    /// `DELETE /internal/v1/end-users/{userId}` (`principals.write`; tombstone).
    Delete { user_id: Uuid },
    /// `POST /internal/v1/end-users/{userId}/revoke-sessions` (`principals.write`).
    RevokeSessions { user_id: Uuid },
    /// `POST /internal/v1/end-users/{userId}/tokens` (`user_tokens.issue`).
    IssueToken { user_id: Uuid, body: Value },
    /// `POST /internal/v1/end-users/tokens` (`principals.write` + `user_tokens.issue`).
    UpsertToken { body: Value },
}

impl EndUserCall {
    /// `(method, path, query, body)` of the Platform Access request.
    pub fn request(
        &self,
    ) -> (
        axum::http::Method,
        String,
        Vec<(&'static str, String)>,
        Option<Value>,
    ) {
        use axum::http::Method;
        const BASE: &str = "/internal/v1/end-users";
        match self {
            EndUserCall::Create { body } => (Method::POST, BASE.into(), vec![], Some(body.clone())),
            EndUserCall::List {
                external_id,
                cursor,
                limit,
            } => {
                let mut query = Vec::new();
                if let Some(e) = external_id {
                    query.push(("externalId", e.clone()));
                }
                if let Some(c) = cursor {
                    query.push(("cursor", c.clone()));
                }
                if let Some(l) = limit {
                    query.push(("limit", l.to_string()));
                }
                (Method::GET, BASE.into(), query, None)
            }
            EndUserCall::Get { user_id } => {
                (Method::GET, format!("{BASE}/{user_id}"), vec![], None)
            }
            EndUserCall::Update { user_id, body } => (
                Method::PATCH,
                format!("{BASE}/{user_id}"),
                vec![],
                Some(body.clone()),
            ),
            EndUserCall::Delete { user_id } => {
                (Method::DELETE, format!("{BASE}/{user_id}"), vec![], None)
            }
            EndUserCall::RevokeSessions { user_id } => (
                Method::POST,
                format!("{BASE}/{user_id}/revoke-sessions"),
                vec![],
                None,
            ),
            EndUserCall::IssueToken { user_id, body } => (
                Method::POST,
                format!("{BASE}/{user_id}/tokens"),
                vec![],
                Some(body.clone()),
            ),
            EndUserCall::UpsertToken { body } => (
                Method::POST,
                format!("{BASE}/tokens"),
                vec![],
                Some(body.clone()),
            ),
        }
    }
}

/// The outcome of a Partner Users API operation.
#[derive(Debug, Clone, PartialEq)]
pub enum EndUsersReply {
    /// 2xx with its status (`201` created / minted, `200` existing, `204` deleted: body
    /// `null`).
    Ok { status: u16, body: Value },
    /// Platform Access refused (stable code).
    Refused { status: u16, code: String },
}

/// The acting party of a delegated mutation: a human member (`u-…`), the service
/// account of a scoped machine credential (`s-…`) or a Developer Platform operator
/// command (`o-<command id>`, cross-referenced by the operator activity event).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationActor {
    Human(Uuid),
    ServiceAccount(Uuid),
    Operator(Uuid),
}

impl DelegationActor {
    pub fn header_value(&self) -> String {
        match self {
            DelegationActor::Human(id) => format!("u-{}", id.simple()),
            DelegationActor::ServiceAccount(id) => format!("s-{}", id.simple()),
            DelegationActor::Operator(id) => format!("o-{}", id.simple()),
        }
    }
}

/// A delegated Platform Access operation (Part VI §50). The Developer Platform never
/// chooses a tenant classification and never creates a PRODUCTION environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationCall {
    CreateTenant {
        actor: DelegationActor,
        slug: String,
        display_name: String,
    },
    GetTenant {
        tenant_id: Uuid,
    },
    /// Section 15 (ORG-014): the EXTERNAL tenant created under a creation slug.
    GetTenantBySlug {
        slug: String,
    },
    /// Section 15 (ORG-013): propagates an organization rename (EXTERNAL tenants only).
    RenameTenant {
        actor: DelegationActor,
        tenant_id: Uuid,
        display_name: String,
    },
    SetTenantStatus {
        actor: DelegationActor,
        tenant_id: Uuid,
        suspended: bool,
    },
    CreateApplication {
        actor: DelegationActor,
        tenant_id: Uuid,
        slug: String,
        display_name: String,
    },
    GetApplication {
        application_id: Uuid,
    },
    SetApplicationStatus {
        actor: DelegationActor,
        application_id: Uuid,
        suspended: bool,
    },
    CreateEnvironment {
        actor: DelegationActor,
        application_id: Uuid,
        kind: String,
    },
    GetEnvironment {
        environment_id: Uuid,
    },
    SetEnvironmentStatus {
        actor: DelegationActor,
        environment_id: Uuid,
        suspended: bool,
    },
    ListApplicationScopes {
        application_id: Uuid,
    },
    /// A service account in a non-production environment (Platform Access takes tenant
    /// and application from the persisted environment). Never a credential.
    CreateServiceAccount {
        actor: DelegationActor,
        environment_id: Uuid,
        name: String,
        description: Option<String>,
    },
    ListServiceAccounts {
        environment_id: Uuid,
    },
    /// One service account of exactly `environment_id` (any other: NOT_FOUND).
    GetServiceAccount {
        environment_id: Uuid,
        service_account_id: Uuid,
    },
    /// ACTIVE / SUSPENDED only: revocation is never delegated, nothing is deleted.
    SetServiceAccountStatus {
        actor: DelegationActor,
        environment_id: Uuid,
        service_account_id: Uuid,
        suspended: bool,
    },
    /// §54: issues a credential of a service account of `environment_id`. The raw
    /// secret comes back ONCE in Platform Access's response and is never stored here.
    IssueCredential {
        actor: DelegationActor,
        environment_id: Uuid,
        service_account_id: Uuid,
        scopes: Vec<String>,
        label: Option<String>,
        /// RFC 3339.
        expires_at: Option<String>,
    },
    ListCredentials {
        environment_id: Uuid,
        service_account_id: Uuid,
    },
    GetCredential {
        environment_id: Uuid,
        credential_id: Uuid,
    },
    RotateCredential {
        actor: DelegationActor,
        environment_id: Uuid,
        credential_id: Uuid,
        overlap_minutes: Option<i64>,
        scopes: Option<Vec<String>>,
        label: Option<String>,
        expires_at: Option<String>,
    },
    RevokeCredential {
        actor: DelegationActor,
        environment_id: Uuid,
        credential_id: Uuid,
    },
    SuspendCredential {
        actor: DelegationActor,
        environment_id: Uuid,
        credential_id: Uuid,
        reason_code: String,
    },
    ResumeCredential {
        actor: DelegationActor,
        environment_id: Uuid,
        credential_id: Uuid,
    },
    /// ADR 0068 A4: the owner-delivered keys of `environment_id` (issued or rotated by HQ
    /// staff through the admin bridge) still waiting for their one reveal; never a secret.
    ListCredentialDeliveries {
        environment_id: Uuid,
    },
    /// ADR 0068 A4: the ONE reveal of an owner-delivered key to a signed-in human member:
    /// `{credential, secret}`, the sealed copy destroyed by Platform Access. Never stored
    /// or replayed here.
    RevealCredentialDelivery {
        actor: DelegationActor,
        environment_id: Uuid,
        delivery_ref: Uuid,
    },
    /// §54: allows a STANDARD scope on the application for `environment_kinds`
    /// (FIRST_PARTY_ONLY only as an operator command; PRODUCTION only once activated).
    GrantApplicationScope {
        actor: DelegationActor,
        application_id: Uuid,
        scope: String,
        environment_kinds: Vec<String>,
    },
    WithdrawApplicationScope {
        actor: DelegationActor,
        application_id: Uuid,
        scope: String,
    },
    /// §55: activates the PRODUCTION environment of an approved request.
    ActivateProduction {
        actor: DelegationActor,
        application_id: Uuid,
        approval_reference: String,
    },
    /// §55: live telecom of an ACTIVATED production environment.
    SetLiveTelecom {
        actor: DelegationActor,
        environment_id: Uuid,
        allowed: bool,
    },
    /// ADR 0062 (build contract §2.1, console read): the partner users of an environment.
    ListEnvironmentEndUsers {
        environment_id: Uuid,
        cursor: Option<String>,
        limit: Option<u32>,
    },
    /// ADR 0062 (build contract §2.3): the environment's allowed websites.
    GetAllowedOrigins {
        environment_id: Uuid,
    },
    /// ADR 0062 (build contract §2.3): replaces the environment's allowed websites.
    PutAllowedOrigins {
        actor: DelegationActor,
        environment_id: Uuid,
        origins: Vec<String>,
    },
    /// ADR 0062: a SANDBOX development token for the console's test token generator
    /// (`POST /internal/v1/delegation/environments/{id}/test-user-tokens`, the body of
    /// `/v1/users/tokens`; requested from Platform Access — see `control::users`).
    IssueTestUserToken {
        actor: DelegationActor,
        environment_id: Uuid,
        body: Value,
    },
}

impl DelegationCall {
    /// `(method, path, body, actor)` of the Platform Access request.
    pub fn request(
        &self,
    ) -> (
        axum::http::Method,
        String,
        Option<Value>,
        Option<DelegationActor>,
    ) {
        use axum::http::Method;
        use serde_json::json;
        let status = |s: bool| json!({ "status": if s { "SUSPENDED" } else { "ACTIVE" } });
        match self {
            DelegationCall::CreateTenant {
                actor,
                slug,
                display_name,
            } => (
                Method::POST,
                "/internal/v1/delegation/tenants".into(),
                Some(json!({ "slug": slug, "displayName": display_name })),
                Some(*actor),
            ),
            DelegationCall::GetTenant { tenant_id } => (
                Method::GET,
                format!("/internal/v1/delegation/tenants/{tenant_id}"),
                None,
                None,
            ),
            DelegationCall::GetTenantBySlug { slug } => (
                Method::GET,
                format!("/internal/v1/delegation/tenant-slugs/{slug}"),
                None,
                None,
            ),
            DelegationCall::RenameTenant {
                actor,
                tenant_id,
                display_name,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/tenants/{tenant_id}/display-name"),
                Some(json!({ "displayName": display_name })),
                Some(*actor),
            ),
            DelegationCall::SetTenantStatus {
                actor,
                tenant_id,
                suspended,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/tenants/{tenant_id}/status"),
                Some(status(*suspended)),
                Some(*actor),
            ),
            DelegationCall::CreateApplication {
                actor,
                tenant_id,
                slug,
                display_name,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/tenants/{tenant_id}/applications"),
                Some(json!({ "slug": slug, "displayName": display_name })),
                Some(*actor),
            ),
            DelegationCall::GetApplication { application_id } => (
                Method::GET,
                format!("/internal/v1/delegation/applications/{application_id}"),
                None,
                None,
            ),
            DelegationCall::SetApplicationStatus {
                actor,
                application_id,
                suspended,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/applications/{application_id}/status"),
                Some(status(*suspended)),
                Some(*actor),
            ),
            DelegationCall::CreateEnvironment {
                actor,
                application_id,
                kind,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/applications/{application_id}/environments"),
                Some(json!({ "kind": kind })),
                Some(*actor),
            ),
            DelegationCall::GetEnvironment { environment_id } => (
                Method::GET,
                format!("/internal/v1/delegation/environments/{environment_id}"),
                None,
                None,
            ),
            DelegationCall::SetEnvironmentStatus {
                actor,
                environment_id,
                suspended,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/environments/{environment_id}/status"),
                Some(status(*suspended)),
                Some(*actor),
            ),
            DelegationCall::ListApplicationScopes { application_id } => (
                Method::GET,
                format!("/internal/v1/delegation/applications/{application_id}/scopes"),
                None,
                None,
            ),
            DelegationCall::CreateServiceAccount {
                actor,
                environment_id,
                name,
                description,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/environments/{environment_id}/service-accounts"),
                Some(json!({ "name": name, "description": description })),
                Some(*actor),
            ),
            DelegationCall::ListServiceAccounts { environment_id } => (
                Method::GET,
                format!("/internal/v1/delegation/environments/{environment_id}/service-accounts"),
                None,
                None,
            ),
            DelegationCall::GetServiceAccount {
                environment_id,
                service_account_id,
            } => (
                Method::GET,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/service-accounts/{service_account_id}"
                ),
                None,
                None,
            ),
            DelegationCall::SetServiceAccountStatus {
                actor,
                environment_id,
                service_account_id,
                suspended,
            } => (
                Method::POST,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/service-accounts/{service_account_id}/status"
                ),
                Some(status(*suspended)),
                Some(*actor),
            ),
            DelegationCall::IssueCredential {
                actor,
                environment_id,
                service_account_id,
                scopes,
                label,
                expires_at,
            } => (
                Method::POST,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/service-accounts/{service_account_id}/credentials"
                ),
                Some(json!({ "scopes": scopes, "label": label, "expiresAt": expires_at })),
                Some(*actor),
            ),
            DelegationCall::ListCredentials {
                environment_id,
                service_account_id,
            } => (
                Method::GET,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/service-accounts/{service_account_id}/credentials"
                ),
                None,
                None,
            ),
            DelegationCall::GetCredential {
                environment_id,
                credential_id,
            } => (
                Method::GET,
                format!("/internal/v1/delegation/environments/{environment_id}/credentials/{credential_id}"),
                None,
                None,
            ),
            DelegationCall::RotateCredential {
                actor,
                environment_id,
                credential_id,
                overlap_minutes,
                scopes,
                label,
                expires_at,
            } => (
                Method::POST,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/credentials/{credential_id}/rotate"
                ),
                Some(json!({
                    "overlapMinutes": overlap_minutes, "scopes": scopes, "label": label,
                    "expiresAt": expires_at,
                })),
                Some(*actor),
            ),
            DelegationCall::RevokeCredential {
                actor,
                environment_id,
                credential_id,
            } => (
                Method::POST,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/credentials/{credential_id}/revoke"
                ),
                None,
                Some(*actor),
            ),
            DelegationCall::SuspendCredential {
                actor,
                environment_id,
                credential_id,
                reason_code,
            } => (
                Method::POST,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/credentials/{credential_id}/suspend"
                ),
                Some(json!({ "reasonCode": reason_code })),
                Some(*actor),
            ),
            DelegationCall::ResumeCredential {
                actor,
                environment_id,
                credential_id,
            } => (
                Method::POST,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/credentials/{credential_id}/resume"
                ),
                None,
                Some(*actor),
            ),
            DelegationCall::ListCredentialDeliveries { environment_id } => (
                Method::GET,
                format!("/internal/v1/delegation/environments/{environment_id}/credential-deliveries"),
                None,
                None,
            ),
            DelegationCall::RevealCredentialDelivery {
                actor,
                environment_id,
                delivery_ref,
            } => (
                Method::POST,
                format!(
                    "/internal/v1/delegation/environments/{environment_id}/credential-deliveries/{delivery_ref}/reveal"
                ),
                None,
                Some(*actor),
            ),
            DelegationCall::GrantApplicationScope {
                actor,
                application_id,
                scope,
                environment_kinds,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/applications/{application_id}/scopes"),
                Some(json!({ "scope": scope, "environmentKinds": environment_kinds })),
                Some(*actor),
            ),
            DelegationCall::WithdrawApplicationScope {
                actor,
                application_id,
                scope,
            } => (
                Method::DELETE,
                format!("/internal/v1/delegation/applications/{application_id}/scopes/{scope}"),
                None,
                Some(*actor),
            ),
            DelegationCall::ActivateProduction {
                actor,
                application_id,
                approval_reference,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/applications/{application_id}/production-environment"),
                Some(json!({ "approvalReference": approval_reference })),
                Some(*actor),
            ),
            DelegationCall::SetLiveTelecom {
                actor,
                environment_id,
                allowed,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/environments/{environment_id}/live-telecom"),
                Some(json!({ "allowed": allowed })),
                Some(*actor),
            ),
            DelegationCall::ListEnvironmentEndUsers { environment_id, .. } => (
                Method::GET,
                format!("/internal/v1/delegation/environments/{environment_id}/end-users"),
                None,
                None,
            ),
            DelegationCall::GetAllowedOrigins { environment_id } => (
                Method::GET,
                format!("/internal/v1/delegation/environments/{environment_id}/allowed-origins"),
                None,
                None,
            ),
            DelegationCall::PutAllowedOrigins {
                actor,
                environment_id,
                origins,
            } => (
                Method::PUT,
                format!("/internal/v1/delegation/environments/{environment_id}/allowed-origins"),
                Some(json!({ "origins": origins })),
                Some(*actor),
            ),
            DelegationCall::IssueTestUserToken {
                actor,
                environment_id,
                body,
            } => (
                Method::POST,
                format!("/internal/v1/delegation/environments/{environment_id}/test-user-tokens"),
                Some(body.clone()),
                Some(*actor),
            ),
        }
    }

    /// The query string of the Platform Access request (list pagination).
    pub fn query(&self) -> Vec<(&'static str, String)> {
        match self {
            DelegationCall::ListEnvironmentEndUsers { cursor, limit, .. } => {
                let mut q = Vec::new();
                if let Some(c) = cursor {
                    q.push(("cursor", c.clone()));
                }
                if let Some(l) = limit {
                    q.push(("limit", l.to_string()));
                }
                q
            }
            _ => Vec::new(),
        }
    }
}

/// The outcome of a delegated operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Delegated {
    /// 2xx: Platform Access's resource view.
    Ok(Value),
    /// Platform Access refused (stable code; never a reason to retry).
    Refused { status: u16, code: String },
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

    async fn credential_allows(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
        credential_id: Uuid,
        scopes: &[&str],
    ) -> Result<bool, AuthorityUnavailable> {
        self.evaluate(delegated, correlation, credential_id, scopes)
            .await
    }

    async fn delegate(
        &self,
        call: DelegationCall,
        correlation: &CorrelationId,
    ) -> Result<Delegated, AuthorityUnavailable> {
        self.delegate_http(call, correlation).await
    }

    async fn end_users(
        &self,
        call: EndUserCall,
        forwarded: &ForwardedCredential,
        correlation: &CorrelationId,
    ) -> Result<EndUsersReply, AuthorityUnavailable> {
        self.end_users_http(call, forwarded, correlation).await
    }

    async fn scope_registry(&self) -> Result<Vec<Value>, AuthorityUnavailable> {
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
                    .filter_map(|s| {
                        Some(serde_json::json!({
                            "name": s.get("name")?.as_str()?,
                            "description": s.get("description").cloned().unwrap_or(Value::Null),
                            "trust": s.get("trust")?.as_str()?,
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default())
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

impl HttpPlatformAccess {
    async fn evaluate(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
        credential_id: Uuid,
        scopes: &[&str],
    ) -> Result<bool, AuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation)
            .with_delegated_context(delegated.clone());
        let body = serde_json::json!({ "credentialId": credential_id, "requiredScopes": scopes });
        let response = self
            .client
            .exchange_json(
                &request,
                axum::http::Method::POST,
                "/internal/v1/authorization/evaluate",
                Some(&body),
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            200 => Ok(response
                .json::<Value>()
                .ok()
                .and_then(|v| v.get("allowed").and_then(Value::as_bool))
                .unwrap_or(false)),
            400 | 403 | 404 => Ok(false),
            status => Err(AuthorityUnavailable(format!("HTTP {status}"))),
        }
    }

    async fn delegate_http(
        &self,
        call: DelegationCall,
        correlation: &CorrelationId,
    ) -> Result<Delegated, AuthorityUnavailable> {
        let (method, path, body, actor) = call.request();
        let owned_query = call.query();
        let query: Vec<(&str, &str)> = owned_query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation);
        let actor_value = actor.map(|a| a.header_value());
        let headers: Vec<(&str, &str)> = actor_value
            .as_deref()
            .map(|v| vec![("x-cf-delegated-actor", v)])
            .unwrap_or_default();
        let response = self
            .client
            .exchange_json_query_with_headers(
                &request,
                method,
                &path,
                &query,
                &headers,
                body.as_ref(),
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            200..=299 => response
                .json::<Value>()
                .map(Delegated::Ok)
                .map_err(|_| AuthorityUnavailable("malformed delegation response".into())),
            status @ 400..=499 => Ok(Delegated::Refused {
                status,
                code: response
                    .error_code()
                    .unwrap_or_else(|| "REFUSED".into())
                    .chars()
                    .take(64)
                    .collect(),
            }),
            status => Err(AuthorityUnavailable(format!("HTTP {status}"))),
        }
    }
}

impl HttpPlatformAccess {
    /// Build contract §2.1: the signed delegated context (which carries the initiating
    /// credential id) rides on the service token and `CF-Forwarded-Credential-Id` names the
    /// forwarded credential; Platform Access refuses a Partner Users call without it.
    async fn end_users_http(
        &self,
        call: EndUserCall,
        forwarded: &ForwardedCredential,
        correlation: &CorrelationId,
    ) -> Result<EndUsersReply, AuthorityUnavailable> {
        let (method, path, owned_query, body) = call.request();
        let query: Vec<(&str, &str)> = owned_query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation)
            .with_delegated_context(forwarded.delegated.clone());
        let credential = forwarded.credential_id.to_string();
        let headers = [(FORWARDED_CREDENTIAL_HEADER, credential.as_str())];
        let response = self
            .client
            .exchange_json_query_with_headers(
                &request,
                method,
                &path,
                &query,
                &headers,
                body.as_ref(),
            )
            .await
            .map_err(unavailable)?;
        match response.status() {
            204 => Ok(EndUsersReply::Ok {
                status: 204,
                body: Value::Null,
            }),
            status @ 200..=299 => response
                .json::<Value>()
                .map(|body| EndUsersReply::Ok { status, body })
                .map_err(|_| AuthorityUnavailable("malformed end-user response".into())),
            status @ 400..=499 => Ok(EndUsersReply::Refused {
                status,
                code: response
                    .error_code()
                    .unwrap_or_else(|| "REFUSED".into())
                    .chars()
                    .take(64)
                    .collect(),
            }),
            status => Err(AuthorityUnavailable(format!("HTTP {status}"))),
        }
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
    fn end_user_calls_address_the_partner_users_router() {
        use axum::http::Method;
        let id = Uuid::new_v4();
        let (m, p, q, b) = EndUserCall::List {
            external_id: Some("rider-1".into()),
            cursor: None,
            limit: Some(20),
        }
        .request();
        assert_eq!((m, p.as_str()), (Method::GET, "/internal/v1/end-users"));
        assert_eq!(
            q,
            vec![
                ("externalId", "rider-1".to_string()),
                ("limit", "20".to_string())
            ]
        );
        assert!(b.is_none());
        let (m, p, _, _) = EndUserCall::RevokeSessions { user_id: id }.request();
        assert_eq!(m, Method::POST);
        assert_eq!(p, format!("/internal/v1/end-users/{id}/revoke-sessions"));
        let (m, p, _, b) = EndUserCall::UpsertToken { body: json!({}) }.request();
        assert_eq!(
            (m, p.as_str()),
            (Method::POST, "/internal/v1/end-users/tokens")
        );
        assert_eq!(b, Some(json!({})));
        let (m, p, _, _) = EndUserCall::Delete { user_id: id }.request();
        assert_eq!(
            (m, p),
            (Method::DELETE, format!("/internal/v1/end-users/{id}"))
        );
        // Console delegations (platform-identity.delegate).
        let env = Uuid::new_v4();
        let list = DelegationCall::ListEnvironmentEndUsers {
            environment_id: env,
            cursor: Some("c1".into()),
            limit: None,
        };
        assert_eq!(
            list.request().1,
            format!("/internal/v1/delegation/environments/{env}/end-users")
        );
        assert_eq!(list.query(), vec![("cursor", "c1".to_string())]);
        let put = DelegationCall::PutAllowedOrigins {
            actor: DelegationActor::Human(id),
            environment_id: env,
            origins: vec!["https://app.example.com".into()],
        };
        let (m, p, b, a) = put.request();
        assert_eq!(m, Method::PUT);
        assert_eq!(
            p,
            format!("/internal/v1/delegation/environments/{env}/allowed-origins")
        );
        assert_eq!(b, Some(json!({ "origins": ["https://app.example.com"] })));
        assert!(a.is_some());
        assert!(put.query().is_empty());
    }

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

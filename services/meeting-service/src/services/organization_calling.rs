//! Blueprint Section 12 follow-up (CAL-007, ORG-012, CAL-009; ADR 0056 §3–§5, ADR 0058) —
//! per-organization Enterprise Calling through Meeting Service.
//!
//! Today the web Calling adapter runs every CF Meet user through ONE shared platform
//! credential and scope, with Enterprise administration granted by a deployment-wide
//! environment allowlist, and Calling's audit records only that shared credential. This
//! module is the server-side replacement for organization workspaces:
//!
//! ```text
//! human (bearer) ─▶ /api/v2/organizations/{id}/calling/{path}
//!   1. account authority: verified human, revocation checked
//!   2. Organizations: calling.enterprise.manage + features.calling — the SAME evaluation
//!      as GET /api/v2/me/workspaces/{id}/capabilities (workspaces.rs)
//!   3. Organizations: scopes/resolve → the organization's VERIFIED tenant / application /
//!      environment and its Calling service binding (service account + credential ids)
//!   4. Platform Access: the member's NATIVE_CF_USER principal in THAT scope (ensure)
//!   5. Calling customer route, no customer credential: a Meeting Service token holding
//!      calling-request.act-for-organization whose signed delegated context names the
//!      organization's credential (Platform Access re-verifies it live) and the acting
//!      principal (recorded on Calling's enterprise audit as actorPrincipalId)
//! ```
//!
//! Rollout (`ORGANIZATION_CALLING_SCOPE`):
//! * `shared` (default) — nothing here is constructed; the relay answers
//!   `409 ORGANIZATION_CALLING_DISABLED` and the existing shared-credential path stays
//!   the one in use. No active configuration changes.
//! * `organization` — the relay is live. Requires `ORGANIZATIONS_AUTHORITY_MODE=
//!   authoritative`, `CF_CALLING_BASE_URL`, `CF_PLATFORM_ACCESS_BASE_URL`,
//!   `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`
//!   (`CF_CALLING_TIMEOUT_MS` optional, 1000–30000, default 10000); anything missing stops
//!   startup. Calling must also run with `CALLING_ORGANIZATION_FORWARDING_ENABLED=true`
//!   and trust Meeting Service's key with the new grant.
//!
//! Scope: the organization's Enterprise administration surface (the routes the web
//! adapter gates with `CALLING_ADMIN_PRINCIPAL_IDS`). Personal calls keep the governed
//! personal scope (ADR 0056 §4). Nothing here weakens PRIVATE_E2EE or touches Messaging.

use std::{sync::Arc, sync::LazyLock, time::Duration};

use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind, KeyId,
    OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    SystemClock, TokenTtl,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        organizations_authority::{
            relayed_error, unavailable, ActingUser, AuthorityError, CallingServiceBinding,
            OrganizationsAuthority, ResolvedOrganizationScope, SharedOrganizationsAuthority,
        },
        workspaces::{self, OrganizationFeature},
    },
};

/// Rollout flag.
pub const MODE_ENV: &str = "ORGANIZATION_CALLING_SCOPE";
/// The organization capability Enterprise administration needs.
pub const CALLING_ENTERPRISE_MANAGE: &str = "calling.enterprise.manage";
/// The opaque Platform Access reference of a CF Meet account (same as Messaging's).
const USER_REFERENCE_PREFIX: &str = "cfmeet-user:";
/// Largest accepted Calling / Platform Access answer.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Largest relayed request body.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// `ORGANIZATION_CALLING_SCOPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallingScopeMode {
    /// Current behaviour: the shared credential path (outside this service) stays in use.
    Shared,
    /// Organization workspaces use their own provisioned scope through this relay.
    Organization,
}

impl CallingScopeMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "shared" => Ok(Self::Shared),
            "organization" => Ok(Self::Organization),
            _ => Err(format!("{MODE_ENV} must be shared or organization")),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Route allowlist (the organization's Enterprise administration surface)
// ---------------------------------------------------------------------------------------

/// `(method, pattern)`; `{}` = one lowercase canonical UUID, `{n}` = a positive integer.
static ROUTES: LazyLock<Vec<(Method, String)>> = LazyLock::new(|| {
    let mut r: Vec<(Method, String)> = Vec::new();
    let mut add = |methods: &[Method], pattern: String| {
        for m in methods {
            r.push((m.clone(), pattern.clone()));
        }
    };
    use Method as M;
    add(&[M::GET], "v1/calling/usage".into());
    add(&[M::GET], "v1/calling/limits".into());
    for c in [
        "policies",
        "departments",
        "extensions",
        "ring-groups",
        "queues",
        "ivr-flows",
        "business-hours",
        "public-contacts",
        "forwarding-rules",
        "voicemail/mailboxes",
        "recording-policies",
        "sip-devices",
    ] {
        add(&[M::GET, M::POST], format!("v1/enterprise/{c}"));
    }
    for c in [
        "policies",
        "departments",
        "extensions",
        "ring-groups",
        "queues",
        "ivr-flows",
        "forwarding-rules",
        "voicemail/mailboxes",
        "recording-policies",
    ] {
        add(
            &[M::GET, M::PATCH, M::DELETE],
            format!("v1/enterprise/{c}/{{}}"),
        );
    }
    add(
        &[M::GET, M::PUT, M::DELETE],
        "v1/enterprise/business-hours/{}".into(),
    );
    for c in ["public-contacts", "sip-devices"] {
        add(&[M::GET, M::DELETE], format!("v1/enterprise/{c}/{{}}"));
    }
    for c in ["departments", "ring-groups", "queues"] {
        add(&[M::PUT], format!("v1/enterprise/{c}/{{}}/members"));
    }
    add(&[M::GET], "v1/enterprise/queues/{}/entries".into());
    add(
        &[M::GET, M::POST],
        "v1/enterprise/ivr-flows/{}/versions".into(),
    );
    add(&[M::GET], "v1/enterprise/ivr-flows/{}/versions/{n}".into());
    add(&[M::PUT], "v1/enterprise/ivr-flows/{}/activation".into());
    add(
        &[M::POST],
        "v1/enterprise/business-hours/{}/evaluation".into(),
    );
    add(&[M::GET], "v1/enterprise/audit".into());
    add(&[M::GET], "v1/enterprise/effective-policy".into());
    add(&[M::GET], "v1/enterprise/recordings".into());
    add(&[M::GET], "v1/enterprise/voicemail/messages".into());
    add(
        &[M::GET, M::DELETE],
        "v1/enterprise/voicemail/messages/{}".into(),
    );
    add(
        &[M::PUT],
        "v1/enterprise/voicemail/messages/{}/read-state".into(),
    );
    add(
        &[M::POST],
        "v1/enterprise/voicemail/messages/{}/media-access".into(),
    );
    add(&[M::GET, M::DELETE], "v1/enterprise/recordings/{}".into());
    add(
        &[M::POST],
        "v1/enterprise/recordings/{}/media-access".into(),
    );
    add(&[M::GET], "v1/enterprise/recordings/{}/access-audit".into());
    add(&[M::GET, M::POST], "v1/webhooks".into());
    add(&[M::GET, M::PATCH, M::DELETE], "v1/webhooks/{}".into());
    add(&[M::POST], "v1/webhooks/{}/rotate-secret".into());
    add(&[M::GET], "v1/webhooks/{}/deliveries".into());
    add(&[M::POST], "v1/webhook-deliveries/{}/replay".into());
    r
});

/// `true` only for an exact allowlisted (method, path); UUIDs must be lowercase canonical.
pub fn is_allowlisted(method: &Method, path: &str) -> bool {
    if path.is_empty() || path.len() > 256 {
        return false;
    }
    let segments: Vec<&str> = path.split('/').collect();
    ROUTES.iter().any(|(m, pattern)| {
        if m != method {
            return false;
        }
        let expected: Vec<&str> = pattern.split('/').collect();
        expected.len() == segments.len()
            && expected.iter().zip(&segments).all(|(e, s)| match *e {
                "{}" => Uuid::try_parse(s).is_ok_and(|u| u.hyphenated().to_string() == *s),
                "{n}" => {
                    !s.is_empty()
                        && s.len() <= 9
                        && s.bytes().all(|b| b.is_ascii_digit())
                        && !s.starts_with('0')
                }
                literal => literal == *s,
            })
    })
}

/// The relayed query: at most 16 pairs, keys `[A-Za-z0-9_]{1,64}`, values ≤ 256 bytes.
pub fn parse_query(raw: Option<&str>) -> ApiResult<Vec<(String, String)>> {
    let Some(raw) = raw.filter(|q| !q.is_empty()) else {
        return Ok(vec![]);
    };
    let invalid = || ApiError::domain(ErrorCode::ValidationError, "Invalid query");
    if raw.len() > 4096 {
        return Err(invalid());
    }
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let ok = pairs.len() <= 16
        && pairs.iter().all(|(k, v)| {
            !k.is_empty()
                && k.len() <= 64
                && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && v.len() <= 256
        });
    if ok {
        Ok(pairs)
    } else {
        Err(invalid())
    }
}

// ---------------------------------------------------------------------------------------
// The relay
// ---------------------------------------------------------------------------------------

/// One relayed request.
pub struct CallingRequest<'a> {
    pub organization: Uuid,
    pub method: Method,
    pub path: &'a str,
    pub query: Vec<(String, String)>,
    pub idempotency_key: Option<String>,
    pub body: Option<Value>,
}

pub struct OrganizationCalling {
    signer: Arc<ServiceTokenSigner>,
    http: reqwest::Client,
    calling: url::Url,
    platform_access: url::Url,
}

impl std::fmt::Debug for OrganizationCalling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrganizationCalling")
            .field("calling", &self.calling.as_str())
            .field("platform_access", &self.platform_access.as_str())
            .finish()
    }
}

pub type SharedOrganizationCalling = Arc<OrganizationCalling>;

fn base_url(name: &str, value: &str, runtime: RuntimeEnvironment) -> Result<url::Url, String> {
    let base = url::Url::parse(value).map_err(|_| format!("{name} is not a valid URL"))?;
    let shape_ok = base.host().is_some()
        && base.username().is_empty()
        && base.password().is_none()
        && base.query().is_none()
        && base.fragment().is_none();
    if !shape_ok {
        return Err(format!("{name} must be a plain base URL"));
    }
    match base.scheme() {
        "https" => Ok(base),
        "http" if runtime.allows_insecure_transport() => Ok(base),
        _ => Err(format!(
            "{name} must use https in the {runtime} runtime environment"
        )),
    }
}

impl OrganizationCalling {
    /// `signer` must be Meeting Service's.
    pub fn new(
        signer: Arc<ServiceTokenSigner>,
        calling_url: &str,
        platform_access_url: &str,
        timeout: Duration,
    ) -> Result<Self, String> {
        if signer.issuer() != &ServiceId::meeting_service() {
            return Err("the organization Calling relay signs as Meeting Service only".into());
        }
        let runtime = signer.runtime_environment();
        let calling = base_url("CF_CALLING_BASE_URL", calling_url, runtime)?;
        let platform_access =
            base_url("CF_PLATFORM_ACCESS_BASE_URL", platform_access_url, runtime)?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .build()
            .map_err(|_| "the Calling HTTP client could not be built".to_string())?;
        Ok(Self {
            signer,
            http,
            calling,
            platform_access,
        })
    }

    /// `None` in `shared` mode (the default). `organization` mode requires the
    /// authoritative Organizations Service and a complete configuration; anything missing
    /// is an error (startup stops). Errors never contain secret material.
    pub fn from_env(
        authority: Option<&SharedOrganizationsAuthority>,
    ) -> Result<Option<SharedOrganizationCalling>, String> {
        let mode = CallingScopeMode::parse(&std::env::var(MODE_ENV).unwrap_or_default())?;
        if mode == CallingScopeMode::Shared {
            return Ok(None);
        }
        if !authority.is_some_and(|a| a.is_authoritative()) {
            return Err(format!(
                "{MODE_ENV}=organization requires ORGANIZATIONS_AUTHORITY_MODE=authoritative"
            ));
        }
        let required = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{k} is required with {MODE_ENV}=organization"))
        };
        let runtime = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT is invalid".to_string())?;
        let key = ServiceSigningKey::from_pkcs8_pem_file(
            KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
                .map_err(|_| "CF_SERVICE_AUTH_KEY_ID is invalid".to_string())?,
            std::path::Path::new(&required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
        )
        .map_err(|_| "the service signing key could not be loaded".to_string())?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            runtime,
            key,
            TokenTtl::seconds(60).map_err(|_| "invalid token TTL".to_string())?,
            Arc::new(SystemClock),
        ));
        let timeout_ms = std::env::var("CF_CALLING_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(10_000)
            .clamp(1_000, 30_000);
        Ok(Some(Arc::new(Self::new(
            signer,
            &required("CF_CALLING_BASE_URL")?,
            &required("CF_PLATFORM_ACCESS_BASE_URL")?,
            Duration::from_millis(timeout_ms),
        )?)))
    }

    /// The delegated platform context of the organization's scope.
    fn scope_context(
        scope: &ResolvedOrganizationScope,
        binding: CallingServiceBinding,
    ) -> DelegatedPlatformContext {
        // ADR 0058: the organization's environment is PRODUCTION-kind. Platform Access and
        // Calling compare the ids, never this kind, when they verify the context.
        DelegatedPlatformContext::new(
            scope.tenant_id,
            scope.application_id,
            scope.environment_id,
            ApplicationEnvironmentKind::Production,
            binding.service_account_id,
        )
    }

    /// One signed exchange; `Err` only when no usable answer was obtained.
    #[allow(clippy::too_many_arguments)]
    async fn exchange(
        &self,
        audience: ServiceId,
        base: &url::Url,
        method: Method,
        path: &str,
        query: &[(String, String)],
        idempotency_key: Option<&str>,
        body: Option<&Value>,
        context: DelegatedPlatformContext,
    ) -> Result<(u16, Value), &'static str> {
        let mut url = base.join(path).map_err(|_| "invalid path")?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
        let signed = self
            .signer
            .sign(&OutboundServiceRequest::new_operation(audience).with_delegated_context(context))
            .map_err(|_| "signing failed")?;
        // A fresh header map: nothing from the inbound request is forwarded except what is
        // added here explicitly. No customer credential exists on this path.
        let mut headers = HeaderMap::new();
        if let Some(key) = idempotency_key {
            headers.insert(
                HeaderName::from_static("idempotency-key"),
                HeaderValue::from_str(key).map_err(|_| "invalid idempotency key")?,
            );
        }
        signed.apply_to(&mut headers);
        let mut builder = self.http.request(method, url).headers(headers);
        if let Some(body) = body {
            builder = builder
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(body).map_err(|_| "encoding")?);
        }
        let mut response = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else {
                "transport"
            }
        })?;
        let status = response.status().as_u16();
        let mut buffer = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if buffer.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        return Err("response too large");
                    }
                    buffer.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => return Err("transport"),
            }
        }
        let value = if buffer.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            serde_json::from_slice(&buffer).map_err(|_| "undecodable response")?
        };
        Ok((status, value))
    }

    /// The member's NATIVE_CF_USER principal in the organization's environment (created
    /// on first use through Platform Access; cached in `communication_principal_links`,
    /// which is keyed by environment, so a personal-scope principal is never reused).
    async fn principal_for(
        &self,
        db: &DatabaseConnection,
        scope: &ResolvedOrganizationScope,
        binding: CallingServiceBinding,
        user_id: Uuid,
    ) -> ApiResult<Uuid> {
        if let Some(row) = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT principal_id FROM communication_principal_links \
                 WHERE subject_kind = 'USER' AND subject_id = $1 AND environment_id = $2",
                [user_id.into(), scope.environment_id.into()],
            ))
            .await?
        {
            return Ok(row.try_get_by_index(0)?);
        }
        let (status, value) = self
            .exchange(
                ServiceId::platform_access(),
                &self.platform_access,
                Method::POST,
                "/internal/v1/principals/ensure",
                &[],
                None,
                Some(&json!({
                    "kind": "NATIVE_CF_USER",
                    "externalReference": format!("{USER_REFERENCE_PREFIX}{user_id}"),
                })),
                Self::scope_context(scope, binding),
            )
            .await
            .map_err(|why| {
                tracing::warn!(
                    error = why,
                    "Platform Access unavailable for an organization principal"
                );
                calling_unavailable()
            })?;
        if !(200..300).contains(&status) {
            tracing::error!(
                status,
                code = value["error"]["code"].as_str().unwrap_or(""),
                organization_id = %scope.organization_id,
                "Platform Access refused the organization principal (binding / scope misconfigured?)"
            );
            return Err(if status >= 500 {
                calling_unavailable()
            } else {
                not_set_up()
            });
        }
        let principal = value["principal"]["id"]
            .as_str()
            .and_then(|s| Uuid::try_parse(s).ok())
            .ok_or_else(calling_unavailable)?;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO communication_principal_links \
             (subject_kind, subject_id, environment_id, principal_id, principal_kind) \
             VALUES ('USER', $1, $2, $3, 'NATIVE_CF_USER') ON CONFLICT DO NOTHING",
            [
                user_id.into(),
                scope.environment_id.into(),
                principal.into(),
            ],
        ))
        .await?;
        Ok(principal)
    }

    /// Relays one Enterprise administration request of an organization workspace and
    /// returns Calling's status and JSON body.
    pub async fn relay(
        &self,
        db: &DatabaseConnection,
        authority: &OrganizationsAuthority,
        actor: &ActorContext,
        req: CallingRequest<'_>,
    ) -> ApiResult<(u16, Value)> {
        let user = actor.require_registered()?;
        if !user.revocation_checked {
            return Err(ApiError::domain(
                ErrorCode::ServiceUnavailable,
                "Your session cannot be verified right now; retry shortly",
            ));
        }
        if !is_allowlisted(&req.method, req.path) {
            return Err(ApiError::domain(
                ErrorCode::EndpointNotFound,
                "No such organization Calling operation",
            ));
        }
        let acting = ActingUser::of(user);
        // Enterprise administration is an Organizations capability, decided exactly as the
        // workspace capabilities read reports it — never an environment allowlist.
        workspaces::require_organization_capability(
            authority,
            &acting,
            req.organization,
            OrganizationFeature::Calling,
            Some(CALLING_ENTERPRISE_MANAGE),
        )
        .await?;
        let scope = authority
            .resolve_scope(req.organization, &acting)
            .await
            .map_err(|e| match e {
                AuthorityError::Refused(reply) => relayed_error(&reply),
                AuthorityError::Unavailable(_) => unavailable(),
            })?;
        let Some(binding) = scope.calling else {
            return Err(not_set_up());
        };
        let principal = self
            .principal_for(db, &scope, binding, user.user_id)
            .await?;
        let context = Self::scope_context(&scope, binding)
            .with_initiating_credential(binding.credential_id)
            .with_initiating_principal(principal, DelegatedPrincipalKind::NativeCfUser);
        let path = format!("/{}", req.path);
        let (status, value) = self
            .exchange(
                ServiceId::calling(),
                &self.calling,
                req.method,
                &path,
                &req.query,
                req.idempotency_key.as_deref(),
                req.body.as_ref(),
                context,
            )
            .await
            .map_err(|why| {
                tracing::warn!(
                    error = why,
                    "Calling unavailable for an organization request"
                );
                calling_unavailable()
            })?;
        match status {
            // Calling did not accept the forwarded request: forwarding disabled there, the
            // trust bundle lacks the grant, or Platform Access no longer authenticates the
            // organization's credential. Never shown as the caller's own authentication.
            401 => {
                tracing::error!(
                    organization_id = %req.organization,
                    "Calling refused the organization forward (forwarding flag, trust bundle or credential binding)"
                );
                Err(calling_unavailable())
            }
            s if s >= 500 => Err(calling_unavailable()),
            s => Ok((s, value)),
        }
    }
}

fn calling_unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::ServiceUnavailable,
        "Calling is temporarily unavailable; retry later",
    )
}

fn not_set_up() -> ApiError {
    ApiError::domain(
        ErrorCode::OrganizationCallingDisabled,
        "Enterprise Calling is not set up for this organization yet",
    )
}

/// The answer while `ORGANIZATION_CALLING_SCOPE=shared`.
pub fn disabled() -> ApiError {
    ApiError::domain(
        ErrorCode::OrganizationCallingDisabled,
        "Per-organization Enterprise Calling is not enabled",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b5f8c9e-2a4d-4c1e-9f3a-7d6b5e4c3a21";

    #[test]
    fn mode_defaults_to_the_shared_path() {
        assert_eq!(
            CallingScopeMode::parse("").unwrap(),
            CallingScopeMode::Shared
        );
        assert_eq!(
            CallingScopeMode::parse(" Shared ").unwrap(),
            CallingScopeMode::Shared
        );
        assert_eq!(
            CallingScopeMode::parse("organization").unwrap(),
            CallingScopeMode::Organization
        );
        assert!(CallingScopeMode::parse("org").is_err());
        assert!(CallingScopeMode::parse("true").is_err());
    }

    #[test]
    fn only_the_enterprise_administration_surface_is_relayed() {
        for (m, p) in [
            (Method::GET, "v1/enterprise/extensions".to_string()),
            (Method::POST, "v1/enterprise/queues".to_string()),
            (Method::PATCH, format!("v1/enterprise/policies/{ID}")),
            (Method::PUT, format!("v1/enterprise/business-hours/{ID}")),
            (
                Method::PUT,
                format!("v1/enterprise/ring-groups/{ID}/members"),
            ),
            (
                Method::GET,
                format!("v1/enterprise/ivr-flows/{ID}/versions/12"),
            ),
            (Method::GET, "v1/enterprise/audit".to_string()),
            (Method::GET, "v1/enterprise/recordings".to_string()),
            (
                Method::POST,
                format!("v1/enterprise/recordings/{ID}/media-access"),
            ),
            (Method::GET, "v1/calling/usage".to_string()),
            (Method::POST, format!("v1/webhook-deliveries/{ID}/replay")),
        ] {
            assert!(is_allowlisted(&m, &p), "{m} {p}");
        }
        for (m, p) in [
            // Personal calling and endpoints keep the personal scope.
            (Method::POST, "v1/calls".to_string()),
            (Method::GET, "v1/calling/endpoints".to_string()),
            (Method::GET, "v1/blocks".to_string()),
            // Privacy Relay administration is not an organization capability.
            (Method::GET, "v1/privacy-relay/contexts".to_string()),
            // Wrong method, shape, case or extra segments.
            (Method::DELETE, "v1/enterprise/extensions".to_string()),
            (Method::PUT, format!("v1/enterprise/extensions/{ID}")),
            (
                Method::GET,
                format!("v1/enterprise/extensions/{}", ID.to_uppercase()),
            ),
            (
                Method::GET,
                "v1/enterprise/extensions/not-a-uuid".to_string(),
            ),
            (
                Method::GET,
                format!("v1/enterprise/ivr-flows/{ID}/versions/01"),
            ),
            (
                Method::GET,
                format!("v1/enterprise/ivr-flows/{ID}/versions/x"),
            ),
            (Method::GET, "v1/enterprise/audit/".to_string()),
            (Method::GET, "/v1/enterprise/audit".to_string()),
            (Method::GET, "v1/enterprise/../calls".to_string()),
            (Method::GET, "internal/v1/enterprise/audit".to_string()),
            (Method::GET, String::new()),
        ] {
            assert!(!is_allowlisted(&m, &p), "{m} {p}");
        }
    }

    #[test]
    fn queries_are_bounded() {
        assert!(parse_query(None).unwrap().is_empty());
        assert_eq!(
            parse_query(Some("limit=20&cursor=abc")).unwrap(),
            vec![
                ("limit".to_string(), "20".to_string()),
                ("cursor".to_string(), "abc".to_string())
            ]
        );
        assert!(parse_query(Some("a-b=1")).is_err());
        assert!(parse_query(Some(&format!("v={}", "x".repeat(257)))).is_err());
        let many: Vec<String> = (0..17).map(|i| format!("k{i}=v")).collect();
        assert!(parse_query(Some(&many.join("&"))).is_err());
    }

    #[test]
    fn scope_resolution_answers_are_parsed_defensively() {
        let org = Uuid::new_v4();
        let ids = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let doc = |bindings: Value| {
            json!({ "organizationId": org, "capability": "primary", "tenantId": ids.0,
                    "applicationId": ids.1, "environmentId": ids.2, "mappingVersion": 3,
                    "verifiedAt": "2026-11-07T00:00:00Z", "serviceBindings": bindings })
        };
        let unbound =
            ResolvedOrganizationScope::from_json(&doc(json!({ "calling": null }))).unwrap();
        assert_eq!(unbound.calling, None);
        assert_eq!(unbound.tenant_id, ids.0);
        let sa = Uuid::new_v4();
        let cred = Uuid::new_v4();
        let bound = ResolvedOrganizationScope::from_json(&doc(json!({
            "calling": { "serviceAccountId": sa, "credentialId": cred, "version": 1 }
        })))
        .unwrap();
        assert_eq!(
            bound.calling,
            Some(CallingServiceBinding {
                service_account_id: sa,
                credential_id: cred
            })
        );
        // A malformed binding refuses the whole answer; a missing id too.
        assert!(ResolvedOrganizationScope::from_json(&doc(json!({
            "calling": { "serviceAccountId": "x", "credentialId": cred }
        })))
        .is_none());
        let mut missing = doc(json!({ "calling": null }));
        missing.as_object_mut().unwrap().remove("tenantId");
        assert!(ResolvedOrganizationScope::from_json(&missing).is_none());
    }
}

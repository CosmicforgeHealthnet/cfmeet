//! Blueprint Section 4 — the Organizations Service as the first-party business
//! organization authority (ADR 0056 §2 single writer, §3 ingress, §5 workspace context,
//! §6 fail closed; internal contract `docs/api/organizations-internal-v1.md`).
//!
//! `ORGANIZATIONS_AUTHORITY_MODE`:
//! * `legacy` (default) — meeting-service's own `organizations` / `organization_memberships`
//!   tables are the authority, exactly as before. Nothing here is constructed.
//! * `shadow` — meeting-service is still the ONLY writer and every caller gets the legacy
//!   answer. A bounded background comparison asks Organizations for the same user (set of
//!   ACTIVE organizations and the role in each) and records mismatches as counters and
//!   structured logs that carry ids and counts only (never emails or names). A comparison
//!   that fails or is skipped never affects the request.
//! * `authoritative` — only after the gated production cutover: membership comes from
//!   `POST /internal/v1/organizations/memberships/resolve` and any failure grants nothing;
//!   legacy organization writes become authenticated facades over Organizations (never a
//!   second writer); the local `organizations` rows become a read-only projection, upserted
//!   from the authoritative snapshot whenever an organization is referenced, so the
//!   `meeting_spaces.organization_id` foreign key, the pre-join organization name and the
//!   organization profile policy keep working.
//!
//! Every call is signed with `cf_service_auth` (issuer `cfmeet.meeting-service`, audience
//! `cfmeet.organizations`, 60 s tokens bound to the hop's request id). The acting user
//! (`X-CF-Acting-User`) and the verified email (`X-CF-Acting-User-Verified-Email`) come
//! only from [`ActingUser::of`] — a [`RegisteredActor`] the account authority verified —
//! and every outbound request is built from a fresh header map, so a client-supplied
//! `X-CF-Acting-User*` header can never reach Organizations. Only the routes in
//! [`RELAY_ROUTES`] can ever be called.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, SystemClock, TokenTtl,
};

use crate::{
    error::{ApiError, ErrorCode},
    services::{
        actor::{OrganizationDirectory, RegisteredActor, SharedOrganizationDirectory},
        ops_metrics::{inc, OPS_METRICS as OM},
        organizations::DbOrganizationDirectory,
    },
};

/// Service identity of the Organizations Service.
pub const ORGANIZATIONS_SERVICE: &str = "cfmeet.organizations";
/// Acting user header (contract §1).
pub const ACTING_USER_HEADER: &str = "x-cf-acting-user";
/// Verified email of the acting user (contract §1, invitation acceptance).
pub const ACTING_USER_VERIFIED_EMAIL_HEADER: &str = "x-cf-acting-user-verified-email";
/// Largest accepted response body.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Concurrent background shadow comparisons; beyond this a comparison is skipped.
const SHADOW_CONCURRENCY: usize = 8;
/// Member-list pages read for one organization (200 per page).
const MAX_MEMBER_PAGES: usize = 10;

/// Which store answers organization questions (`ORGANIZATIONS_AUTHORITY_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityMode {
    Legacy,
    Shadow,
    Authoritative,
}

impl AuthorityMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "legacy" => Ok(Self::Legacy),
            "shadow" => Ok(Self::Shadow),
            "authoritative" => Ok(Self::Authoritative),
            _ => Err("ORGANIZATIONS_AUTHORITY_MODE must be legacy, shadow or authoritative".into()),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Shadow => "shadow",
            Self::Authoritative => "authoritative",
        }
    }
}

// ---------------------------------------------------------------------------------------
// Relay allowlist
// ---------------------------------------------------------------------------------------

/// The only internal operations meeting-service ever calls (`{}` = one UUID segment).
/// Migration, reconciliation and capability evaluation are deliberately absent.
pub const RELAY_ROUTES: &[(&str, &str)] = &[
    ("POST", "/internal/v1/organizations"),
    ("POST", "/internal/v1/organizations/memberships/resolve"),
    ("GET", "/internal/v1/organizations/{}"),
    ("PATCH", "/internal/v1/organizations/{}"),
    ("GET", "/internal/v1/organizations/{}/members"),
    ("PATCH", "/internal/v1/organizations/{}/members/{}"),
    ("POST", "/internal/v1/organizations/{}/members/me/leave"),
    ("POST", "/internal/v1/organizations/{}/ownership-transfer"),
    ("POST", "/internal/v1/organizations/{}/invitations"),
    ("GET", "/internal/v1/organizations/{}/invitations"),
    (
        "POST",
        "/internal/v1/organizations/{}/invitations/{}/revoke",
    ),
    ("POST", "/internal/v1/organization-invitations/accept"),
    ("POST", "/internal/v1/organization-invitations/decline"),
    ("POST", "/internal/v1/organizations/{}/suspend"),
    ("POST", "/internal/v1/organizations/{}/reactivate"),
    ("POST", "/internal/v1/organizations/{}/archive"),
    ("GET", "/internal/v1/organizations/{}/audit-events"),
    ("GET", "/internal/v1/organizations/{}/provisioning"),
    // Blueprint Section 8 (workspace capabilities): the role matrix under the current
    // policy and the effective organization constraints, read with the acting user.
    ("GET", "/internal/v1/organizations/{}/roles/capabilities"),
    ("GET", "/internal/v1/organizations/{}/policies/effective"),
    // Blueprint Section 12 follow-up (per-organization Enterprise Calling): the verified
    // technical scope and the organization's Calling service binding, read with the
    // acting user (membership required).
    ("POST", "/internal/v1/organizations/scopes/resolve"),
    ("GET", "/internal/v1/users/{}/organizations"),
    ("GET", "/internal/v1/admin/organizations"),
    ("POST", "/internal/v1/admin/organizations"),
    ("PATCH", "/internal/v1/admin/organizations/{}"),
];

/// `true` only for an exact [`RELAY_ROUTES`] match.
pub fn is_allowlisted(method: &Method, path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    RELAY_ROUTES.iter().any(|(m, pattern)| {
        if *m != method.as_str() {
            return false;
        }
        let expected: Vec<&str> = pattern.split('/').collect();
        expected.len() == segments.len()
            && expected.iter().zip(&segments).all(|(e, s)| match *e {
                "{}" => Uuid::try_parse(s).is_ok_and(|u| u.hyphenated().to_string() == *s),
                literal => literal == *s,
            })
    })
}

// ---------------------------------------------------------------------------------------
// Acting user
// ---------------------------------------------------------------------------------------

/// The human an Organizations operation is performed for. Constructed only from a
/// [`RegisteredActor`] (a bearer token the account authority verified, with its account
/// checks), never from request headers or bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActingUser {
    user_id: Uuid,
    verified_email: Option<String>,
}

impl ActingUser {
    pub fn of(actor: &RegisteredActor) -> Self {
        let verified_email = actor
            .verified_email
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| {
                !e.is_empty()
                    && e.len() <= 254
                    && e.contains('@')
                    && e.bytes().all(|b| (0x21..=0x7e).contains(&b))
            });
        Self {
            user_id: actor.user_id,
            verified_email,
        }
    }

    pub fn user_id(&self) -> Uuid {
        self.user_id
    }
}

// ---------------------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------------------

/// One internal call.
#[derive(Debug, Clone)]
pub struct Call<'a> {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub actor: Option<&'a ActingUser>,
    pub idempotency_key: Option<String>,
    pub body: Option<Value>,
}

impl<'a> Call<'a> {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: vec![],
            actor: None,
            idempotency_key: None,
            body: None,
        }
    }

    pub fn actor(mut self, actor: &'a ActingUser) -> Self {
        self.actor = Some(actor);
        self
    }

    pub fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    pub fn query(mut self, query: Vec<(String, String)>) -> Self {
        self.query = query;
        self
    }

    pub fn idempotency_key(mut self, key: Option<String>) -> Self {
        self.idempotency_key = key;
        self
    }
}

/// The answer of Organizations (any HTTP status, JSON body).
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub retry_after: Option<u32>,
    /// `Idempotent-Replayed` response header, when present.
    pub replayed: Option<String>,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn error_code(&self) -> Option<&str> {
        self.body
            .pointer("/error/code")
            .and_then(Value::as_str)
            .filter(|c| c.len() <= 64 && c.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
    }

    fn error_message(&self) -> Option<String> {
        self.body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty() && m.chars().count() <= 300)
            .map(str::to_string)
    }
}

/// Organizations could not be asked (transport, timeout, undecodable answer, 5xx,
/// a refused service credential or a route outside the allowlist). Never carries a URL,
/// token or body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable(pub &'static str);

/// Failure of a typed read.
#[derive(Debug, Clone)]
pub enum AuthorityError {
    Unavailable(Unavailable),
    /// Organizations answered with a client error (relayed to the caller).
    Refused(Reply),
}

impl From<Unavailable> for AuthorityError {
    fn from(u: Unavailable) -> Self {
        Self::Unavailable(u)
    }
}

/// Signed HTTP client for the Organizations Service.
pub struct OrganizationsClient {
    http: reqwest::Client,
    signer: Arc<ServiceTokenSigner>,
    base: url::Url,
    audience: ServiceId,
}

impl std::fmt::Debug for OrganizationsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrganizationsClient")
            .field("issuer", self.signer.issuer())
            .field("audience", &self.audience)
            .finish()
    }
}

impl OrganizationsClient {
    /// `base_url` must be HTTPS outside the `local` / `test` runtime environments, with no
    /// credentials, query or fragment.
    pub fn new(
        signer: Arc<ServiceTokenSigner>,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Self, String> {
        let base = url::Url::parse(base_url)
            .map_err(|_| "CF_ORGANIZATIONS_BASE_URL is not a valid URL".to_string())?;
        let shape_ok = base.host().is_some()
            && base.username().is_empty()
            && base.password().is_none()
            && base.query().is_none()
            && base.fragment().is_none();
        if !shape_ok {
            return Err("CF_ORGANIZATIONS_BASE_URL must be a plain base URL".into());
        }
        match base.scheme() {
            "https" => {}
            "http" if signer.runtime_environment().allows_insecure_transport() => {}
            _ => {
                return Err(format!(
                    "CF_ORGANIZATIONS_BASE_URL must use https in the {} runtime environment",
                    signer.runtime_environment()
                ))
            }
        }
        if signer.issuer().as_str() == ORGANIZATIONS_SERVICE {
            return Err("the Organizations client cannot sign as Organizations".into());
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .build()
            .map_err(|_| "the Organizations HTTP client could not be built".to_string())?;
        Ok(Self {
            http,
            signer,
            base,
            audience: ServiceId::parse(ORGANIZATIONS_SERVICE).expect("valid service id"),
        })
    }

    /// Sends one allowlisted call. Every HTTP status is returned as a [`Reply`]; only a
    /// failure to obtain a decodable answer is [`Unavailable`].
    pub async fn send(&self, call: &Call<'_>) -> Result<Reply, Unavailable> {
        if !is_allowlisted(&call.method, &call.path) {
            tracing::error!(method = %call.method, "refused an Organizations call outside the relay allowlist");
            return Err(Unavailable("route not allowlisted"));
        }
        let mut url = self
            .base
            .join(&call.path)
            .map_err(|_| Unavailable("invalid path"))?;
        if !call.query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(call.query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
        let signed = self
            .signer
            .sign(&OutboundServiceRequest::new_operation(
                self.audience.clone(),
            ))
            .map_err(|_| Unavailable("signing failed"))?;
        // A fresh header map: nothing from the inbound request is forwarded except what is
        // added here explicitly.
        let mut headers = HeaderMap::new();
        if let Some(key) = &call.idempotency_key {
            headers.insert(
                HeaderName::from_static("idempotency-key"),
                HeaderValue::from_str(key).map_err(|_| Unavailable("invalid idempotency key"))?,
            );
        }
        if let Some(actor) = call.actor {
            headers.insert(
                HeaderName::from_static(ACTING_USER_HEADER),
                HeaderValue::from_str(&actor.user_id.to_string()).expect("uuid header"),
            );
            if let Some(email) = &actor.verified_email {
                let mut value =
                    HeaderValue::from_str(email).map_err(|_| Unavailable("invalid email"))?;
                value.set_sensitive(true);
                headers.insert(
                    HeaderName::from_static(ACTING_USER_VERIFIED_EMAIL_HEADER),
                    value,
                );
            }
        }
        // Service authentication last: it always wins.
        signed.apply_to(&mut headers);
        let mut builder = self.http.request(call.method.clone(), url).headers(headers);
        if let Some(body) = &call.body {
            builder = builder
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(body).map_err(|_| Unavailable("encoding"))?);
        }
        let mut response = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                Unavailable("timeout")
            } else if e.is_connect() {
                Unavailable("connect")
            } else {
                Unavailable("transport")
            }
        })?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.len() <= 64)
                .map(str::to_string)
        };
        let retry_after = header("retry-after").and_then(|v| v.parse::<u32>().ok());
        let replayed = header("idempotent-replayed");
        let mut buffer = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if buffer.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        return Err(Unavailable("response too large"));
                    }
                    buffer.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => return Err(Unavailable("transport")),
            }
        }
        let body = if buffer.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            serde_json::from_slice(&buffer).map_err(|_| Unavailable("undecodable response"))?
        };
        Ok(Reply {
            status,
            body,
            retry_after,
            replayed,
        })
    }
}

// ---------------------------------------------------------------------------------------
// Contract shapes (parsed defensively)
// ---------------------------------------------------------------------------------------

/// One ACTIVE membership from `memberships/resolve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMembership {
    pub organization_id: Uuid,
    pub role: String,
    pub status: String,
    pub organization_status: String,
}

impl ResolvedMembership {
    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            organization_id: uuid_field(v, "organizationId")?,
            role: v["role"].as_str()?.to_string(),
            status: v["status"].as_str().unwrap_or("ACTIVE").to_string(),
            organization_status: v["organizationStatus"].as_str()?.to_string(),
        })
    }

    /// Whether the membership counts as a verified meeting-domain membership:
    /// ACTIVE membership of an ACTIVE organization, any role except GUEST.
    pub fn grants_meeting_membership(&self) -> bool {
        self.status == "ACTIVE" && self.organization_status == "ACTIVE" && self.role != "GUEST"
    }
}

/// The authoritative organization document (service read, no actor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationSnapshot {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub lifecycle_status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OrganizationSnapshot {
    pub fn from_json(v: &Value) -> Option<Self> {
        let created_at = time_field(v, "createdAt")?;
        Some(Self {
            id: uuid_field(v, "id")?,
            name: v["name"].as_str()?.to_string(),
            slug: v["slug"].as_str()?.to_string(),
            lifecycle_status: v["lifecycleStatus"].as_str()?.to_string(),
            created_by: uuid_field(v, "createdBy"),
            created_at,
            updated_at: time_field(v, "updatedAt").unwrap_or(created_at),
        })
    }

    /// Local projection status: the projection table knows ACTIVE and SUSPENDED only, so
    /// anything that is not ACTIVE (SUSPENDED, ARCHIVED) is projected as SUSPENDED.
    pub fn projected_status(&self) -> &'static str {
        if self.lifecycle_status == "ACTIVE" {
            "ACTIVE"
        } else {
            "SUSPENDED"
        }
    }
}

/// One non-removed membership from a member list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityMember {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: Option<i64>,
}

impl AuthorityMember {
    pub fn from_json(v: &Value) -> Option<Self> {
        let created_at = time_field(v, "createdAt")?;
        Some(Self {
            id: uuid_field(v, "id")?,
            organization_id: uuid_field(v, "organizationId")?,
            user_id: uuid_field(v, "userId")?,
            role: v["role"].as_str()?.to_string(),
            status: v["status"].as_str()?.to_string(),
            created_at,
            updated_at: time_field(v, "updatedAt").unwrap_or(created_at),
            version: v["version"].as_i64(),
        })
    }
}

/// One of the caller's organizations (`GET /internal/v1/users/{userId}/organizations`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserOrganization {
    pub organization_id: Uuid,
    pub name: String,
    pub slug: String,
    pub lifecycle_status: String,
    pub role: String,
    pub membership_status: String,
    /// The organization's provisioning state (`None` when the answer omits it).
    pub provisioning_state: Option<String>,
    /// The membership's effective capabilities under the organization policy and status.
    pub capabilities: Vec<String>,
}

impl UserOrganization {
    fn from_json(v: &Value) -> Option<Self> {
        let org = &v["organization"];
        let membership = &v["membership"];
        Some(Self {
            organization_id: uuid_field(org, "id")?,
            name: org["name"].as_str()?.to_string(),
            slug: org["slug"].as_str()?.to_string(),
            lifecycle_status: org["lifecycleStatus"].as_str()?.to_string(),
            role: membership["role"].as_str()?.to_string(),
            membership_status: membership["status"].as_str()?.to_string(),
            provisioning_state: org["provisioningState"].as_str().map(str::to_string),
            // Missing → none (default deny; never widened by an incomplete answer).
            capabilities: string_list(&v["capabilities"]).unwrap_or_default(),
        })
    }

    pub fn grants_meeting_membership(&self) -> bool {
        self.membership_status == "ACTIVE"
            && self.lifecycle_status == "ACTIVE"
            && self.role != "GUEST"
    }
}

/// One role of `GET /internal/v1/organizations/{id}/roles/capabilities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleCapabilities {
    pub role: String,
    /// What the role matrix grants (before policy and status).
    pub capabilities: Vec<String>,
    /// What the role holds now under the organization's policy and status.
    pub effective_capabilities: Vec<String>,
}

/// The role / capability matrix of one organization under its current policy and status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationRoleMatrix {
    pub organization_id: Uuid,
    pub lifecycle_status: String,
    pub policy_revision: i64,
    /// The whole capability vocabulary (default deny).
    pub capabilities: Vec<String>,
    pub roles: Vec<RoleCapabilities>,
}

impl OrganizationRoleMatrix {
    pub fn from_json(v: &Value) -> Option<Self> {
        let roles = v["roles"]
            .as_array()?
            .iter()
            .map(|r| {
                Some(RoleCapabilities {
                    role: r["role"].as_str()?.to_string(),
                    capabilities: string_list(&r["capabilities"])?,
                    effective_capabilities: string_list(&r["effectiveCapabilities"])?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            organization_id: uuid_field(v, "organizationId")?,
            lifecycle_status: v["lifecycleStatus"].as_str()?.to_string(),
            policy_revision: v["policyRevision"].as_i64().unwrap_or(0),
            capabilities: string_list(&v["capabilities"])?,
            roles,
        })
    }

    pub fn role(&self, role: &str) -> Option<&RoleCapabilities> {
        self.roles.iter().find(|r| r.role == role)
    }
}

/// The organization-level constraints of `GET …/policies/effective` that the workspace
/// context needs (owner services combine them with their own policy: stricter wins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveOrganizationPolicy {
    pub organization_id: Uuid,
    pub lifecycle_status: String,
    pub provisioning_state: Option<String>,
    pub revision: i64,
    pub meetings_enabled: bool,
    pub messaging_enabled: bool,
    pub calling_enabled: bool,
}

impl EffectiveOrganizationPolicy {
    /// A feature flag that is missing or not a boolean refuses the whole answer: a
    /// malformed policy never enables anything.
    pub fn from_json(v: &Value) -> Option<Self> {
        let features = &v["constraints"]["enabledFeatures"];
        Some(Self {
            organization_id: uuid_field(v, "organizationId")?,
            lifecycle_status: v["lifecycleStatus"].as_str()?.to_string(),
            provisioning_state: v["provisioningState"].as_str().map(str::to_string),
            revision: v["revision"].as_i64().unwrap_or(0),
            meetings_enabled: features["meetings"].as_bool()?,
            messaging_enabled: features["messaging"].as_bool()?,
            calling_enabled: features["calling"].as_bool()?,
        })
    }
}

/// The organization's Calling service binding (ids only — Platform Access re-verifies the
/// credential live on every forwarded request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallingServiceBinding {
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
}

/// `POST /internal/v1/organizations/scopes/resolve`: the VERIFIED technical scope of an
/// organization (only answered when the organization is ACTIVE and provisioning READY).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOrganizationScope {
    pub organization_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub mapping_version: i64,
    /// `None` until an operator binds the organization's Calling credential.
    pub calling: Option<CallingServiceBinding>,
    /// `None` until an operator binds the organization's Messaging service account (same
    /// shape: ids only, re-verified by Platform Access at use).
    pub messaging: Option<CallingServiceBinding>,
}

impl ResolvedOrganizationScope {
    /// A missing id, or a present but malformed binding, refuses the whole answer.
    pub fn from_json(v: &Value) -> Option<Self> {
        let binding = |service: &str| -> Option<Option<CallingServiceBinding>> {
            match &v["serviceBindings"][service] {
                Value::Null => Some(None),
                b => Some(Some(CallingServiceBinding {
                    service_account_id: uuid_field(b, "serviceAccountId")?,
                    credential_id: uuid_field(b, "credentialId")?,
                })),
            }
        };
        let calling = binding("calling")?;
        let messaging = binding("messaging")?;
        Some(Self {
            organization_id: uuid_field(v, "organizationId")?,
            tenant_id: uuid_field(v, "tenantId")?,
            application_id: uuid_field(v, "applicationId")?,
            environment_id: uuid_field(v, "environmentId")?,
            mapping_version: v["mappingVersion"].as_i64().unwrap_or(0),
            calling,
            messaging,
        })
    }
}

/// A JSON array of strings (`None` for anything else).
fn string_list(v: &Value) -> Option<Vec<String>> {
    v.as_array()?
        .iter()
        .map(|c| c.as_str().map(str::to_string))
        .collect()
}

fn uuid_field(v: &Value, key: &str) -> Option<Uuid> {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::try_parse(s).ok())
}

fn time_field(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// Meeting-domain view of an Organizations role: the legacy contract knows OWNER, ADMIN
/// and MEMBER, so MANAGER (and GUEST, where a list must show it) map to MEMBER.
pub fn legacy_role(role: &str) -> crate::domain::OrganizationRole {
    use crate::domain::OrganizationRole;
    match role {
        "OWNER" => OrganizationRole::Owner,
        "ADMIN" => OrganizationRole::Admin,
        _ => OrganizationRole::Member,
    }
}

// ---------------------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------------------

/// Per-instance counters (also mirrored to the process-wide `/internal/metrics`).
#[derive(Debug, Default)]
pub struct AuthorityStats {
    pub shadow_comparisons: AtomicU64,
    pub shadow_mismatches: AtomicU64,
    pub shadow_failures: AtomicU64,
    pub shadow_skipped: AtomicU64,
    pub authority_failures: AtomicU64,
    pub relay_unavailable: AtomicU64,
    pub projection_upserts: AtomicU64,
    pub projection_failures: AtomicU64,
}

impl AuthorityStats {
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

fn bump(local: &AtomicU64, global: &AtomicU64) {
    local.fetch_add(1, Ordering::Relaxed);
    inc(global);
}

// ---------------------------------------------------------------------------------------
// The authority
// ---------------------------------------------------------------------------------------

/// Organizations integration for one meeting-service process (`shadow` / `authoritative`).
pub struct OrganizationsAuthority {
    mode: AuthorityMode,
    client: OrganizationsClient,
    pub stats: Arc<AuthorityStats>,
    shadow_permits: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for OrganizationsAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrganizationsAuthority")
            .field("mode", &self.mode)
            .finish()
    }
}

pub type SharedOrganizationsAuthority = Arc<OrganizationsAuthority>;

impl OrganizationsAuthority {
    pub fn new(mode: AuthorityMode, client: OrganizationsClient) -> Self {
        Self {
            mode,
            client,
            stats: Arc::new(AuthorityStats::default()),
            shadow_permits: Arc::new(tokio::sync::Semaphore::new(SHADOW_CONCURRENCY)),
        }
    }

    /// `None` in `legacy` mode. `shadow` / `authoritative` require a complete client
    /// configuration (`CF_ORGANIZATIONS_BASE_URL`, `CF_RUNTIME_ENVIRONMENT`,
    /// `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`); anything missing or
    /// invalid is an error (startup stops). Errors never contain secret material.
    pub fn from_env() -> Result<Option<SharedOrganizationsAuthority>, String> {
        let mode = AuthorityMode::parse(
            &std::env::var("ORGANIZATIONS_AUTHORITY_MODE").unwrap_or_default(),
        )?;
        if mode == AuthorityMode::Legacy {
            return Ok(None);
        }
        let required = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    format!(
                        "{k} is required with ORGANIZATIONS_AUTHORITY_MODE={}",
                        mode.as_str()
                    )
                })
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
        let timeout_ms = std::env::var("CF_ORGANIZATIONS_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(3000)
            .clamp(500, 10_000);
        let client = OrganizationsClient::new(
            signer,
            &required("CF_ORGANIZATIONS_BASE_URL")?,
            Duration::from_millis(timeout_ms),
        )?;
        Ok(Some(Arc::new(Self::new(mode, client))))
    }

    pub fn mode(&self) -> AuthorityMode {
        self.mode
    }

    pub fn is_authoritative(&self) -> bool {
        self.mode == AuthorityMode::Authoritative
    }

    /// One call; a failure to reach Organizations is counted.
    pub async fn send(&self, call: &Call<'_>) -> Result<Reply, Unavailable> {
        let result = self.client.send(call).await;
        match &result {
            Err(Unavailable(reason)) => {
                bump(
                    &self.stats.relay_unavailable,
                    &OM.organizations_relay_unavailable,
                );
                tracing::warn!(reason = *reason, "Organizations unavailable");
            }
            Ok(reply) if reply.status >= 500 => {
                bump(
                    &self.stats.relay_unavailable,
                    &OM.organizations_relay_unavailable,
                );
                tracing::warn!(
                    status = reply.status,
                    "Organizations answered with a server error"
                );
            }
            Ok(_) => {}
        }
        result
    }

    /// A typed read: 2xx → body; 4xx → [`AuthorityError::Refused`]; else unavailable.
    async fn read(&self, call: &Call<'_>) -> Result<Value, AuthorityError> {
        let reply = self.send(call).await?;
        if reply.is_success() {
            Ok(reply.body)
        } else if (400..500).contains(&reply.status) && !is_internal_refusal(&reply) {
            Err(AuthorityError::Refused(reply))
        } else {
            Err(AuthorityError::Unavailable(Unavailable("refused")))
        }
    }

    /// `POST /internal/v1/organizations/memberships/resolve` (service authorization, no
    /// actor): the user's ACTIVE memberships with role and organization status.
    pub async fn resolve_memberships(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ResolvedMembership>, AuthorityError> {
        let body = self
            .read(
                &Call::new(
                    Method::POST,
                    "/internal/v1/organizations/memberships/resolve",
                )
                .body(json!({ "userId": user_id })),
            )
            .await?;
        if body["userId"]
            .as_str()
            .is_some_and(|u| u != user_id.to_string())
        {
            return Err(AuthorityError::Unavailable(Unavailable(
                "answer for another user",
            )));
        }
        let items = body["memberships"]
            .as_array()
            .ok_or(AuthorityError::Unavailable(Unavailable(
                "malformed resolve answer",
            )))?;
        items
            .iter()
            .map(|m| {
                ResolvedMembership::from_json(m).ok_or(AuthorityError::Unavailable(Unavailable(
                    "malformed membership",
                )))
            })
            .collect()
    }

    /// `GET /internal/v1/organizations/{id}` without an actor: the authoritative snapshot
    /// (`None` when Organizations does not know the organization).
    pub async fn organization_snapshot(
        &self,
        id: Uuid,
    ) -> Result<Option<OrganizationSnapshot>, AuthorityError> {
        match self
            .read(&Call::new(
                Method::GET,
                format!("/internal/v1/organizations/{id}"),
            ))
            .await
        {
            Ok(body) => OrganizationSnapshot::from_json(&body)
                .filter(|s| s.id == id)
                .map(Some)
                .ok_or(AuthorityError::Unavailable(Unavailable(
                    "malformed organization",
                ))),
            Err(AuthorityError::Refused(r)) if r.status == 404 => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Every non-removed membership of `organization` (bounded paging). With an actor,
    /// Organizations requires `organization.members.read`; without one it is a service
    /// read (meeting-service checks platform administration first).
    pub async fn members(
        &self,
        organization: Uuid,
        actor: Option<&ActingUser>,
    ) -> Result<Vec<AuthorityMember>, AuthorityError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_MEMBER_PAGES {
            let mut query = vec![("limit".to_string(), "200".to_string())];
            if let Some(c) = &cursor {
                query.push(("cursor".to_string(), c.clone()));
            }
            let mut call = Call::new(
                Method::GET,
                format!("/internal/v1/organizations/{organization}/members"),
            )
            .query(query);
            call.actor = actor;
            let body = self.read(&call).await?;
            let items =
                body["items"]
                    .as_array()
                    .ok_or(AuthorityError::Unavailable(Unavailable(
                        "malformed member list",
                    )))?;
            for item in items {
                out.push(
                    AuthorityMember::from_json(item)
                        .ok_or(AuthorityError::Unavailable(Unavailable("malformed member")))?,
                );
            }
            match body["nextCursor"].as_str() {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => return Ok(out),
            }
        }
        Ok(out)
    }

    /// `GET /internal/v1/users/{userId}/organizations` for the acting user.
    pub async fn user_organizations(
        &self,
        actor: &ActingUser,
    ) -> Result<Vec<UserOrganization>, AuthorityError> {
        let body = self
            .read(
                &Call::new(
                    Method::GET,
                    format!("/internal/v1/users/{}/organizations", actor.user_id),
                )
                .actor(actor),
            )
            .await?;
        let items = body["items"]
            .as_array()
            .ok_or(AuthorityError::Unavailable(Unavailable(
                "malformed organizations",
            )))?;
        items
            .iter()
            .map(|i| {
                UserOrganization::from_json(i).ok_or(AuthorityError::Unavailable(Unavailable(
                    "malformed organization",
                )))
            })
            .collect()
    }

    /// `GET /internal/v1/organizations/{id}/roles/capabilities` for the acting user
    /// (Organizations requires `organization.read`; non-members get 404).
    pub async fn role_matrix(
        &self,
        organization: Uuid,
        actor: &ActingUser,
    ) -> Result<OrganizationRoleMatrix, AuthorityError> {
        let body = self
            .read(
                &Call::new(
                    Method::GET,
                    format!("/internal/v1/organizations/{organization}/roles/capabilities"),
                )
                .actor(actor),
            )
            .await?;
        OrganizationRoleMatrix::from_json(&body)
            .filter(|m| m.organization_id == organization)
            .ok_or(AuthorityError::Unavailable(Unavailable(
                "malformed role matrix",
            )))
    }

    /// `GET /internal/v1/organizations/{id}/policies/effective` for the acting user.
    pub async fn effective_policy(
        &self,
        organization: Uuid,
        actor: &ActingUser,
    ) -> Result<EffectiveOrganizationPolicy, AuthorityError> {
        let body = self
            .read(
                &Call::new(
                    Method::GET,
                    format!("/internal/v1/organizations/{organization}/policies/effective"),
                )
                .actor(actor),
            )
            .await?;
        EffectiveOrganizationPolicy::from_json(&body)
            .filter(|p| p.organization_id == organization)
            .ok_or(AuthorityError::Unavailable(Unavailable(
                "malformed effective policy",
            )))
    }

    /// `POST /internal/v1/organizations/scopes/resolve` for the acting user (Organizations
    /// requires an ACTIVE membership; `409 PROVISIONING_PENDING` until the scope is READY).
    pub async fn resolve_scope(
        &self,
        organization: Uuid,
        actor: &ActingUser,
    ) -> Result<ResolvedOrganizationScope, AuthorityError> {
        let body = self
            .read(
                &Call::new(Method::POST, "/internal/v1/organizations/scopes/resolve")
                    .actor(actor)
                    .body(json!({ "organizationId": organization, "capability": "primary" })),
            )
            .await?;
        ResolvedOrganizationScope::from_json(&body)
            .filter(|s| s.organization_id == organization)
            .ok_or(AuthorityError::Unavailable(Unavailable(
                "malformed scope resolution",
            )))
    }

    /// Upserts the local read-only projection row from an authoritative snapshot (the
    /// `meeting_spaces.organization_id` foreign key, the pre-join name and the profile
    /// policy read it). Name, slug and status follow the authority; `profile_policy` is
    /// local and untouched. A slug still held locally by another row (a renamed legacy
    /// organization) gets a unique placeholder: the local slug is never served after
    /// cutover.
    pub async fn project<C: ConnectionTrait>(
        &self,
        conn: &C,
        snapshot: &OrganizationSnapshot,
    ) -> Result<(), sea_orm::DbErr> {
        let placeholder = format!("x-{}", snapshot.id.simple());
        let valid_slug = snapshot.slug.len() <= 100
            && snapshot
                .slug
                .starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && snapshot
                .slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        let name: String = snapshot.name.chars().take(200).collect();
        let mut slug = if valid_slug {
            snapshot.slug.clone()
        } else {
            placeholder.clone()
        };
        for attempt in 0..2 {
            let result = conn
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO organizations (id, name, slug, status, created_by, created_at, updated_at, profile_policy) \
                     VALUES ($1, $2, $3, $4, (SELECT u.id FROM users u WHERE u.id = $5), $6, $7, '{}'::jsonb) \
                     ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug, status = EXCLUDED.status, \
                       updated_at = GREATEST(organizations.updated_at, EXCLUDED.updated_at)",
                    [
                        snapshot.id.into(),
                        name.clone().into(),
                        slug.clone().into(),
                        snapshot.projected_status().into(),
                        snapshot.created_by.into(),
                        snapshot.created_at.into(),
                        snapshot.updated_at.into(),
                    ],
                ))
                .await;
            match result {
                Ok(_) => {
                    bump(
                        &self.stats.projection_upserts,
                        &OM.organizations_projection_upserts,
                    );
                    return Ok(());
                }
                Err(e)
                    if attempt == 0 && crate::services::meeting_core::is_unique_violation(&e) =>
                {
                    slug = placeholder.clone();
                }
                Err(e) => {
                    bump(
                        &self.stats.projection_failures,
                        &OM.organizations_projection_failures,
                    );
                    return Err(e);
                }
            }
        }
        unreachable!("the second attempt always returns")
    }

    /// Best-effort projection of an organization document returned by a relayed call.
    pub async fn project_document<C: ConnectionTrait>(&self, conn: &C, body: &Value) {
        if let Some(snapshot) = OrganizationSnapshot::from_json(body) {
            if let Err(e) = self.project(conn, &snapshot).await {
                tracing::warn!(organization_id = %snapshot.id, "organization projection failed: {e}");
            }
        }
    }

    /// Ensures the projection row of `organization` (authoritative snapshot). Unknown to
    /// Organizations → `ORGANIZATION_NOT_FOUND`; unreachable → `SERVICE_UNAVAILABLE`.
    pub async fn ensure_projection<C: ConnectionTrait>(
        &self,
        conn: &C,
        organization: Uuid,
    ) -> Result<OrganizationSnapshot, ApiError> {
        match self.organization_snapshot(organization).await {
            Ok(Some(snapshot)) => {
                self.project(conn, &snapshot).await?;
                Ok(snapshot)
            }
            Ok(None) => Err(ApiError::domain(
                ErrorCode::OrganizationNotFound,
                "Organization not found",
            )),
            Err(AuthorityError::Refused(reply)) => Err(relayed_error(&reply)),
            Err(AuthorityError::Unavailable(_)) => Err(unavailable()),
        }
    }

    /// The acting user's verified organizations (ACTIVE, non-GUEST memberships of ACTIVE
    /// organizations) with each one's CF Meet creation time, from the local projection
    /// (refreshed with the authority's name) or, for an organization not yet projected,
    /// from the authoritative snapshot.
    pub async fn my_organizations(
        &self,
        db: &DatabaseConnection,
        actor: &ActingUser,
    ) -> Result<Vec<(UserOrganization, DateTime<Utc>)>, ApiError> {
        let items: Vec<UserOrganization> = match self.user_organizations(actor).await {
            Ok(items) => items
                .into_iter()
                .filter(UserOrganization::grants_meeting_membership)
                .collect(),
            Err(AuthorityError::Refused(reply)) => return Err(relayed_error(&reply)),
            Err(AuthorityError::Unavailable(_)) => return Err(unavailable()),
        };
        let ids: Vec<Uuid> = items.iter().map(|i| i.organization_id).collect();
        let rows = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id, created_at FROM organizations WHERE id = ANY($1)",
                [ids.into()],
            ))
            .await?;
        let mut created = BTreeMap::new();
        for r in rows {
            let id: Uuid = r.try_get("", "id")?;
            let at: DateTime<Utc> = r.try_get("", "created_at")?;
            created.insert(id, at);
        }
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let created_at = match created.get(&item.organization_id) {
                Some(at) => {
                    // Keep the projection's name and status current (best effort).
                    let refreshed = db
                        .execute(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "UPDATE organizations SET name = $2, status = 'ACTIVE' \
                             WHERE id = $1 AND (name <> $2 OR status <> 'ACTIVE')",
                            [
                                item.organization_id.into(),
                                item.name.chars().take(200).collect::<String>().into(),
                            ],
                        ))
                        .await;
                    if refreshed.is_err() {
                        bump(
                            &self.stats.projection_failures,
                            &OM.organizations_projection_failures,
                        );
                    }
                    *at
                }
                None => {
                    self.ensure_projection(db, item.organization_id)
                        .await?
                        .created_at
                }
            };
            out.push((item, created_at));
        }
        Ok(out)
    }

    // -- directory --------------------------------------------------------------------

    async fn authoritative_organizations(&self, user_id: Uuid) -> Vec<Uuid> {
        match self.resolve_memberships(user_id).await {
            Ok(memberships) => {
                let ids: BTreeSet<Uuid> = memberships
                    .iter()
                    .filter(|m| m.grants_meeting_membership())
                    .map(|m| m.organization_id)
                    .collect();
                ids.into_iter().collect()
            }
            // Fail closed: an unreachable or refusing authority grants no organization.
            Err(e) => {
                bump(
                    &self.stats.authority_failures,
                    &OM.organizations_authority_failures,
                );
                let reason = match &e {
                    AuthorityError::Unavailable(Unavailable(r)) => r.to_string(),
                    AuthorityError::Refused(r) => format!("HTTP {}", r.status),
                };
                tracing::error!(%user_id, %reason, "organization membership unavailable; granting none");
                vec![]
            }
        }
    }

    /// Background shadow comparison of the legacy answer (organization → role) with the
    /// authority's. Bounded; never blocks or affects the caller.
    fn compare_in_background(self: &Arc<Self>, user_id: Uuid, legacy: BTreeMap<Uuid, String>) {
        let Ok(permit) = self.shadow_permits.clone().try_acquire_owned() else {
            bump(&self.stats.shadow_skipped, &OM.organizations_shadow_skipped);
            return;
        };
        let this = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            this.compare(user_id, &legacy).await;
        });
    }

    async fn compare(&self, user_id: Uuid, legacy: &BTreeMap<Uuid, String>) {
        let authority = match self.resolve_memberships(user_id).await {
            Ok(m) => m,
            Err(_) => {
                bump(
                    &self.stats.shadow_failures,
                    &OM.organizations_shadow_failures,
                );
                tracing::warn!(%user_id, "organization shadow comparison could not reach Organizations");
                return;
            }
        };
        let authoritative: BTreeMap<Uuid, String> = authority
            .iter()
            .filter(|m| m.status == "ACTIVE" && m.organization_status == "ACTIVE")
            .map(|m| (m.organization_id, m.role.clone()))
            .collect();
        bump(
            &self.stats.shadow_comparisons,
            &OM.organizations_shadow_comparisons,
        );
        let report = shadow_diff(legacy, &authoritative);
        if !report.is_empty() {
            bump(
                &self.stats.shadow_mismatches,
                &OM.organizations_shadow_mismatches,
            );
            tracing::warn!(
                target: "organizations_shadow",
                %user_id,
                missing_in_authority = ?report.missing_in_authority,
                extra_in_authority = ?report.extra_in_authority,
                role_mismatch = ?report.role_mismatch,
                "organization shadow mismatch"
            );
        }
    }
}

/// Differences between the legacy and the authoritative membership maps (ids only).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ShadowDiff {
    pub missing_in_authority: Vec<Uuid>,
    pub extra_in_authority: Vec<Uuid>,
    pub role_mismatch: Vec<Uuid>,
}

impl ShadowDiff {
    pub fn is_empty(&self) -> bool {
        self.missing_in_authority.is_empty()
            && self.extra_in_authority.is_empty()
            && self.role_mismatch.is_empty()
    }
}

pub fn shadow_diff(
    legacy: &BTreeMap<Uuid, String>,
    authority: &BTreeMap<Uuid, String>,
) -> ShadowDiff {
    let mut diff = ShadowDiff::default();
    for (org, role) in legacy {
        match authority.get(org) {
            None => diff.missing_in_authority.push(*org),
            Some(r) if r != role => diff.role_mismatch.push(*org),
            Some(_) => {}
        }
    }
    for org in authority.keys() {
        if !legacy.contains_key(org) {
            diff.extra_in_authority.push(*org);
        }
    }
    diff
}

fn is_internal_refusal(reply: &Reply) -> bool {
    matches!(
        reply.error_code(),
        Some("ACTOR_NOT_ALLOWED" | "INTERNAL_AUTHENTICATION_FAILED" | "INTERNAL_PERMISSION_DENIED")
    )
}

/// `SERVICE_UNAVAILABLE` for an unreachable Organizations Service.
pub fn unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::ServiceUnavailable,
        "Organizations is temporarily unavailable; retry later",
    )
}

/// The public error for an Organizations error answer: known contract codes keep their
/// code; refused service credentials and server errors are `SERVICE_UNAVAILABLE` (an
/// internal authentication problem is never shown to the caller).
pub fn relayed_error(reply: &Reply) -> ApiError {
    let code = reply.error_code();
    if reply.status >= 500 || is_internal_refusal(reply) {
        if is_internal_refusal(reply) {
            tracing::error!(code = ?code, "Organizations refused meeting-service's service credential");
        }
        return unavailable();
    }
    let mapped = match code {
        Some("VALIDATION_ERROR") => Some(ErrorCode::ValidationError),
        Some("ORGANIZATION_NOT_FOUND") => Some(ErrorCode::OrganizationNotFound),
        Some("MEMBERSHIP_REQUIRED") => Some(ErrorCode::MembershipRequired),
        Some("PERMISSION_DENIED") => Some(ErrorCode::PermissionDenied),
        Some("ORGANIZATION_SUSPENDED") => Some(ErrorCode::OrganizationSuspended),
        Some("ORGANIZATION_ARCHIVED") => Some(ErrorCode::OrganizationArchived),
        Some("LAST_OWNER") => Some(ErrorCode::LastOwner),
        Some("VERSION_CONFLICT") => Some(ErrorCode::VersionConflict),
        Some("SLUG_TAKEN") => Some(ErrorCode::SlugTaken),
        Some("INVITE_EXPIRED") => Some(ErrorCode::InviteExpired),
        Some("INVITATION_INVALID") => Some(ErrorCode::InvitationInvalid),
        Some("IDEMPOTENCY_CONFLICT") => Some(ErrorCode::IdempotencyConflict),
        Some("PROVISIONING_PENDING") => Some(ErrorCode::ProvisioningPending),
        Some("PAYLOAD_TOO_LARGE") => Some(ErrorCode::RequestTooLarge),
        Some("RATE_LIMITED") => {
            return ApiError::rate_limited(
                reply
                    .error_message()
                    .unwrap_or_else(|| "Too many requests".into()),
                reply.retry_after.unwrap_or(60) as u64,
            )
        }
        _ => None,
    };
    let code = mapped.unwrap_or(match reply.status {
        400 | 422 => ErrorCode::ValidationError,
        401 | 403 => ErrorCode::PermissionDenied,
        404 => ErrorCode::OrganizationNotFound,
        409 => ErrorCode::VersionConflict,
        410 => ErrorCode::InviteExpired,
        413 => ErrorCode::RequestTooLarge,
        429 => {
            return ApiError::rate_limited(
                "Too many requests",
                reply.retry_after.unwrap_or(60) as u64,
            )
        }
        _ => return unavailable(),
    });
    let message = reply.error_message().unwrap_or_else(|| match code {
        ErrorCode::OrganizationNotFound => "Organization not found".into(),
        ErrorCode::PermissionDenied => "The operation is not permitted".into(),
        _ => "The request was refused".into(),
    });
    ApiError::domain(code, message)
}

// ---------------------------------------------------------------------------------------
// Directories
// ---------------------------------------------------------------------------------------

/// The legacy membership map of `user` (organization → role), same rule as the legacy
/// directory.
async fn legacy_memberships(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<BTreeMap<Uuid, String>, sea_orm::DbErr> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT m.organization_id, m.role
                 FROM organization_memberships m
                 JOIN organizations o ON o.id = m.organization_id
                 JOIN users u ON u.id = m.user_id
                WHERE m.user_id = $1 AND m.status = 'ACTIVE' AND o.status = 'ACTIVE'
                  AND u.status::text <> 'inactive'"#,
            [user_id.into()],
        ))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some((
                r.try_get_by_index::<Uuid>(0).ok()?,
                r.try_get_by_index::<String>(1).ok()?,
            ))
        })
        .collect())
}

/// `shadow`: the legacy answer, compared in the background.
pub struct ShadowOrganizationDirectory {
    db: DatabaseConnection,
    authority: SharedOrganizationsAuthority,
}

#[async_trait]
impl OrganizationDirectory for ShadowOrganizationDirectory {
    async fn organizations_for_user(&self, user_id: Uuid) -> Vec<Uuid> {
        match legacy_memberships(&self.db, user_id).await {
            Ok(legacy) => {
                let ids = legacy.keys().copied().collect();
                self.authority.compare_in_background(user_id, legacy);
                ids
            }
            // Same fail-closed rule as the legacy directory; nothing to compare.
            Err(e) => {
                tracing::error!("organization directory unavailable: {e}");
                vec![]
            }
        }
    }
}

/// `authoritative`: Organizations' answer, failing closed.
pub struct AuthoritativeOrganizationDirectory {
    authority: SharedOrganizationsAuthority,
}

#[async_trait]
impl OrganizationDirectory for AuthoritativeOrganizationDirectory {
    async fn organizations_for_user(&self, user_id: Uuid) -> Vec<Uuid> {
        self.authority.authoritative_organizations(user_id).await
    }
}

/// The organization directory for the configured mode.
pub fn directory_for(
    db: &DatabaseConnection,
    authority: Option<&SharedOrganizationsAuthority>,
) -> SharedOrganizationDirectory {
    match authority {
        Some(a) if a.mode() == AuthorityMode::Shadow => Arc::new(ShadowOrganizationDirectory {
            db: db.clone(),
            authority: a.clone(),
        }),
        Some(a) if a.mode() == AuthorityMode::Authoritative => {
            Arc::new(AuthoritativeOrganizationDirectory {
                authority: a.clone(),
            })
        }
        _ => Arc::new(DbOrganizationDirectory::new(db.clone())),
    }
}

/// The people a caller may see as colleagues: ACTIVE, non-GUEST members of the caller's
/// verified organizations. In `authoritative` mode this is read from Organizations (with
/// the caller as actor); any organization that cannot be read contributes nobody.
pub async fn authoritative_colleagues(
    authority: &OrganizationsAuthority,
    actor: &RegisteredActor,
    organizations: &[Uuid],
) -> Vec<Uuid> {
    let acting = ActingUser::of(actor);
    let mut people = BTreeSet::new();
    for org in organizations.iter().take(50) {
        match authority.members(*org, Some(&acting)).await {
            Ok(members) => people.extend(
                members
                    .iter()
                    .filter(|m| m.status == "ACTIVE" && m.role != "GUEST")
                    .map(|m| m.user_id),
            ),
            Err(_) => {
                bump(
                    &authority.stats.authority_failures,
                    &OM.organizations_authority_failures,
                );
                tracing::warn!(organization_id = %org, "organization colleagues unavailable; omitted");
            }
        }
    }
    people.remove(&actor.user_id);
    people.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer(env: RuntimeEnvironment) -> Arc<ServiceTokenSigner> {
        Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            env,
            ServiceSigningKey::generate(KeyId::parse("meeting-service-unit-1").unwrap()),
            TokenTtl::default(),
            Arc::new(SystemClock),
        ))
    }

    #[test]
    fn modes_parse_strictly() {
        assert_eq!(AuthorityMode::parse("").unwrap(), AuthorityMode::Legacy);
        assert_eq!(
            AuthorityMode::parse("legacy").unwrap(),
            AuthorityMode::Legacy
        );
        assert_eq!(
            AuthorityMode::parse(" Shadow ").unwrap(),
            AuthorityMode::Shadow
        );
        assert_eq!(
            AuthorityMode::parse("authoritative").unwrap(),
            AuthorityMode::Authoritative
        );
        assert!(AuthorityMode::parse("on").is_err());
    }

    #[test]
    fn only_allowlisted_routes_are_callable() {
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        for (m, p) in [
            (Method::POST, "/internal/v1/organizations".to_string()),
            (Method::GET, format!("/internal/v1/organizations/{id}")),
            (
                Method::PATCH,
                format!("/internal/v1/organizations/{id}/members/{other}"),
            ),
            (
                Method::POST,
                format!("/internal/v1/organizations/{id}/members/me/leave"),
            ),
            (
                Method::POST,
                "/internal/v1/organizations/memberships/resolve".to_string(),
            ),
            (Method::GET, "/internal/v1/admin/organizations".to_string()),
            (
                Method::GET,
                format!("/internal/v1/organizations/{id}/roles/capabilities"),
            ),
            (
                Method::GET,
                format!("/internal/v1/organizations/{id}/policies/effective"),
            ),
        ] {
            assert!(is_allowlisted(&m, &p), "{m} {p}");
        }
        for (m, p) in [
            (Method::POST, "/internal/v1/migration/import".to_string()),
            (
                Method::GET,
                "/internal/v1/migration/reconciliation".to_string(),
            ),
            (
                Method::POST,
                "/internal/v1/organizations/authorization/evaluate".to_string(),
            ),
            (Method::DELETE, format!("/internal/v1/organizations/{id}")),
            // Policy reads / writes stay outside the relay (only the effective read).
            (
                Method::GET,
                format!("/internal/v1/organizations/{id}/policies"),
            ),
            (
                Method::PATCH,
                format!("/internal/v1/organizations/{id}/policies"),
            ),
            (
                Method::GET,
                "/internal/v1/organizations/not-a-uuid".to_string(),
            ),
            (
                Method::GET,
                format!("/internal/v1/organizations/{}", id.simple()),
            ),
            (
                Method::GET,
                format!("/internal/v1/organizations/{id}/../migration"),
            ),
            (Method::GET, format!("/internal/v1/organizations/{id}/")),
        ] {
            assert!(!is_allowlisted(&m, &p), "{m} {p}");
        }
    }

    #[test]
    fn acting_user_comes_from_the_verified_account_only() {
        let mut actor = RegisteredActor {
            user_id: Uuid::new_v4(),
            display_name: "a".into(),
            verified_email: Some("Alice@Example.COM".into()),
            platform_admin: false,
            revocation_checked: true,
        };
        let acting = ActingUser::of(&actor);
        assert_eq!(acting.user_id(), actor.user_id);
        assert_eq!(acting.verified_email.as_deref(), Some("alice@example.com"));
        actor.verified_email = None;
        assert_eq!(ActingUser::of(&actor).verified_email, None);
        actor.verified_email = Some("bad\r\nheader@x".into());
        assert_eq!(ActingUser::of(&actor).verified_email, None);
    }

    #[test]
    fn client_requires_https_outside_local_and_test() {
        let ok = OrganizationsClient::new(
            signer(RuntimeEnvironment::Test),
            "http://127.0.0.1:9",
            Duration::from_secs(1),
        );
        assert!(ok.is_ok());
        let err = OrganizationsClient::new(
            signer(RuntimeEnvironment::Production),
            "http://organizations:8080",
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.contains("https"), "{err}");
        for bad in ["not a url", "https://user:pw@h", "https://h/?q=1"] {
            assert!(OrganizationsClient::new(
                signer(RuntimeEnvironment::Test),
                bad,
                Duration::from_secs(1)
            )
            .is_err());
        }
    }

    #[test]
    fn membership_and_snapshot_parsing() {
        let org = Uuid::new_v4();
        let m = ResolvedMembership::from_json(&json!({
            "membershipId": Uuid::new_v4(), "organizationId": org, "role": "MANAGER",
            "status": "ACTIVE", "organizationStatus": "ACTIVE", "capabilities": []
        }))
        .unwrap();
        assert!(m.grants_meeting_membership());
        let mut guest = m.clone();
        guest.role = "GUEST".into();
        assert!(!guest.grants_meeting_membership());
        let mut suspended = m.clone();
        suspended.organization_status = "SUSPENDED".into();
        assert!(!suspended.grants_meeting_membership());

        let s = OrganizationSnapshot::from_json(&json!({
            "id": org, "name": "Acme", "slug": "acme", "lifecycleStatus": "ARCHIVED",
            "createdBy": null, "createdAt": "2026-10-04T12:00:00Z", "updatedAt": "2026-10-05T12:00:00Z"
        }))
        .unwrap();
        assert_eq!(s.projected_status(), "SUSPENDED");
        assert_eq!(s.created_by, None);
        assert!(OrganizationSnapshot::from_json(&json!({ "id": org })).is_none());
        assert_eq!(
            legacy_role("MANAGER"),
            crate::domain::OrganizationRole::Member
        );
        assert_eq!(legacy_role("OWNER"), crate::domain::OrganizationRole::Owner);
    }

    #[test]
    fn shadow_diff_reports_ids_only() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let legacy = BTreeMap::from([(a, "OWNER".to_string()), (b, "MEMBER".to_string())]);
        let same = legacy.clone();
        assert!(shadow_diff(&legacy, &same).is_empty());
        let authority = BTreeMap::from([(a, "ADMIN".to_string()), (c, "MEMBER".to_string())]);
        let d = shadow_diff(&legacy, &authority);
        assert_eq!(d.role_mismatch, vec![a]);
        assert_eq!(d.missing_in_authority, vec![b]);
        assert_eq!(d.extra_in_authority, vec![c]);
    }

    #[test]
    fn relayed_errors_never_expose_internal_authentication() {
        let reply = |status: u16, code: &str| Reply {
            status,
            body: json!({"schemaVersion":1,"error":{"code":code,"message":"m","requestId":"r"}}),
            retry_after: None,
            replayed: None,
        };
        for (status, code, expected) in [
            (
                404,
                "ORGANIZATION_NOT_FOUND",
                ErrorCode::OrganizationNotFound,
            ),
            (409, "LAST_OWNER", ErrorCode::LastOwner),
            (409, "SLUG_TAKEN", ErrorCode::SlugTaken),
            (403, "ACTOR_NOT_ALLOWED", ErrorCode::ServiceUnavailable),
            (
                401,
                "INTERNAL_AUTHENTICATION_FAILED",
                ErrorCode::ServiceUnavailable,
            ),
            (500, "INTERNAL_ERROR", ErrorCode::ServiceUnavailable),
            (404, "INVITATION_INVALID", ErrorCode::InvitationInvalid),
        ] {
            assert_eq!(
                relayed_error(&reply(status, code)).code(),
                Some(expected),
                "{code}"
            );
        }
        assert_eq!(
            relayed_error(&reply(429, "RATE_LIMITED")).code(),
            Some(ErrorCode::RateLimited)
        );
    }
}

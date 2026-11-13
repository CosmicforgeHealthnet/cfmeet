//! Organizations ↔ Platform Access provisioning saga (Blueprint v1.0 Section 7, ADR 0056
//! §4, ADR 0058).
//!
//! Every ACTIVE organization in provisioning state PENDING gets ONE workflow row
//! (`organization_provisioning`, stable idempotency key `org-scope:{organizationId}:primary`)
//! that the worker drives PENDING → IN_PROGRESS → READY | FAILED:
//!
//! 1. `REQUESTED` — enqueued (the worker backfills imported / existing organizations too);
//! 2. Platform Access `PUT /internal/v1/organization-scopes/{org}/primary` creates or
//!    returns the organization's FIRST_PARTY tenant + first-party application +
//!    PRODUCTION environment (idempotent on the organization id);
//! 3. `SCOPE_CREATED` — checkpoint: the returned ids are recorded on the mapping row in
//!    state VERIFYING (a different triple than an existing mapping, or a tenant already
//!    mapped to another organization, fails the workflow: no split-brain mapping);
//! 4. Platform Access `GET` reads the triple back; Organizations verifies it SERVER-SIDE
//!    (FIRST_PARTY, reserved slug, ownership chain, PRODUCTION, same ids) and resumes the
//!    tenant if needed;
//! 5. `VERIFIED` — the mapping becomes READY (`verified_at`, evidence, `mapping_version`),
//!    the organization READY, and `scope.mapping_changed` + `organization.provisioned`
//!    are emitted in the same transaction.
//!
//! Never a distributed transaction and never another service's database: each step is a
//! signed call or a local transaction with a lease check; a lost lease abandons the step.
//! Platform Access unavailable → capped exponential backoff, then FAILED
//! (`PLATFORM_ACCESS_UNAVAILABLE`) after [`MAX_ATTEMPTS`]; a refusal → FAILED at once
//! (`PLATFORM_SCOPE_CONFLICT`, `PLATFORM_ACCESS_REFUSED`). A FAILED workflow suspends any
//! tenant it recorded (compensation, retried until applied) and waits for an operator
//! retry (`POST /internal/v1/admin/organizations/{id}/provisioning/retry`).
//!
//! Lifecycle: suspending / archiving an organization marks its mapping SUSPENDED and the
//! tenant's desired status SUSPENDED in the SAME transaction (fail closed locally: scope
//! resolution refuses at once); [`sync_tenant_status`] applies it to Platform Access with
//! retries. Reactivation reverses it. [`reconcile`] periodically re-reads READY mappings
//! and records drift as FAILED. Provisioning is OFF by default
//! (`ORGANIZATIONS_PROVISIONING_ENABLED`); while off, organizations stay PENDING and the
//! provisioning read says so.

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, FromQueryResult,
    TransactionTrait,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    domain::LifecycleStatus,
    store::{self, Event, OpContext},
};

/// The organization-scope capability (ADR 0056 §4: one triple per organization).
pub const CAPABILITY: &str = "primary";
/// Attempts (Platform Access unavailable) before a workflow FAILS.
pub const MAX_ATTEMPTS: i32 = 8;
/// How long a claimed workflow / tenant-status sync stays leased.
pub const LEASE_SECONDS: i64 = 120;
/// READY mappings are re-verified against Platform Access after this long.
pub const RECONCILE_AFTER_SECONDS: i64 = 6 * 3600;
/// How long a workflow waits for a suspended organization before looking again.
pub const SUSPENDED_WAIT_SECONDS: i64 = 300;

/// Capped exponential backoff after `attempts` attempts: 5 s, 10 s, 20 s … ≤ 15 min.
pub fn backoff_seconds(attempts: i32) -> i64 {
    (5_i64 << (attempts.clamp(1, 16) - 1)).min(900)
}

/// The stable idempotency key of an organization's workflow.
pub fn idempotency_key(organization: Uuid, capability: &str) -> String {
    format!("org-scope:{organization}:{capability}")
}

// ---------------------------------------------------------------------------------------
// Platform Access port
// ---------------------------------------------------------------------------------------

/// The facts Platform Access reports about an organization scope (contract
/// `OrganizationScope` of `cfmeet.platform-access.internal-api` 1.6.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeFacts {
    pub organization_ref: Uuid,
    pub capability: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub tenant_classification: String,
    pub tenant_status: String,
    pub tenant_revision: i64,
    pub environment_kind: String,
    pub verified: bool,
    pub active: bool,
}

impl ScopeFacts {
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = |k: &str| Uuid::parse_str(v.get(k)?.as_str()?).ok();
        let s = |a: &str, b: &str| Some(v.get(a)?.get(b)?.as_str()?.to_string());
        let facts = Self {
            organization_ref: id("organizationRef")?,
            capability: v.get("capability")?.as_str()?.to_string(),
            tenant_id: id("tenantId")?,
            application_id: id("applicationId")?,
            environment_id: id("environmentId")?,
            tenant_classification: s("tenant", "classification")?,
            tenant_status: s("tenant", "status")?,
            tenant_revision: v.get("tenant")?.get("revision")?.as_i64()?,
            environment_kind: s("environment", "kind")?,
            verified: v.get("verified")?.as_bool()?,
            active: v.get("active")?.as_bool()?,
        };
        // The nested ids must agree with the top-level ones.
        let nested = |a: &str, b: &str| v.get(a).and_then(|o| o.get(b)).and_then(Value::as_str);
        let consistent = nested("tenant", "id") == Some(facts.tenant_id.to_string().as_str())
            && nested("environment", "id") == Some(facts.environment_id.to_string().as_str())
            && nested("application", "id") == Some(facts.application_id.to_string().as_str())
            && matches!(facts.tenant_status.as_str(), "ACTIVE" | "SUSPENDED");
        consistent.then_some(facts)
    }

    /// Server-side verification: exactly the FIRST_PARTY / PRODUCTION triple Section 7
    /// provisions, for this organization and capability.
    pub fn verifies(&self, organization: Uuid, capability: &str) -> bool {
        self.organization_ref == organization
            && self.capability == capability
            && self.verified
            && self.tenant_classification == "FIRST_PARTY"
            && self.environment_kind == "PRODUCTION"
    }

    pub fn same_triple(&self, tenant: Uuid, application: Uuid, environment: Uuid) -> bool {
        self.tenant_id == tenant
            && self.application_id == application
            && self.environment_id == environment
    }

    fn evidence(&self) -> Value {
        json!({
            "verifiedBy": cf_service_auth::services::PLATFORM_ACCESS,
            "tenantClassification": self.tenant_classification,
            "environmentKind": self.environment_kind,
            "tenantRevision": self.tenant_revision,
            "tenantStatus": self.tenant_status,
        })
    }
}

/// A Platform Access failure, classified for the saga (never a body or a URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    /// Retryable: transport failure, timeout, 5xx, 429, malformed answer.
    Unavailable(String),
    /// Final: Platform Access refused (stable code).
    Refused { status: u16, code: String },
}

impl PortError {
    pub fn class(&self) -> String {
        match self {
            PortError::Unavailable(_) => "PLATFORM_ACCESS_UNAVAILABLE".into(),
            PortError::Refused { status: 409, .. } => "PLATFORM_SCOPE_CONFLICT".into(),
            PortError::Refused { .. } => "PLATFORM_ACCESS_REFUSED".into(),
        }
    }

    /// The class of a failed service-binding call (ADR 0058 A2).
    pub fn binding_class(&self) -> String {
        match self {
            PortError::Refused { status: 409, .. } => "SERVICE_BINDING_CONFLICT".into(),
            other => other.class(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Service bindings (ADR 0058 Amendment A2)
// ---------------------------------------------------------------------------------------

/// The consuming services whose binding the saga provisions, in order.
pub const BINDING_SERVICES: &[&str] = &["calling", "messaging"];

/// The exact credential scopes of each automatic binding (the closed vocabulary Platform
/// Access enforces; verified here independently).
pub fn binding_scopes(service: &str) -> Option<&'static [&'static str]> {
    match service {
        "calling" => Some(&[
            "calls.read",
            "enterprise.config.read",
            "enterprise.config.write",
        ]),
        "messaging" => Some(&["messages.read"]),
        _ => None,
    }
}

/// What Platform Access reports about one organization service binding
/// (`OrganizationServiceBinding` of the Platform Access internal API; never a secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingFacts {
    pub organization_ref: Uuid,
    pub capability: String,
    pub service: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub required_scopes: Vec<String>,
    pub scopes: Vec<String>,
    pub service_account_status: String,
    pub credential_status: String,
    pub credential_expires_at: Option<DateTime<Utc>>,
    pub authenticates: bool,
    pub revision: i64,
    pub verified: bool,
    pub active: bool,
    pub revoked: bool,
}

impl BindingFacts {
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = |k: &str| Uuid::parse_str(v.get(k)?.as_str()?).ok();
        let nested = |a: &str, b: &str| v.get(a)?.get(b);
        let strings = |k: &str| -> Option<Vec<String>> {
            let mut out = v
                .get(k)?
                .as_array()?
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?;
            out.sort();
            Some(out)
        };
        let revoked = match v.get("revokedAt") {
            None | Some(Value::Null) => false,
            Some(Value::String(_)) => true,
            Some(_) => return None,
        };
        let facts = Self {
            organization_ref: id("organizationRef")?,
            capability: v.get("capability")?.as_str()?.to_string(),
            service: v.get("service")?.as_str()?.to_string(),
            tenant_id: id("tenantId")?,
            application_id: id("applicationId")?,
            environment_id: id("environmentId")?,
            service_account_id: id("serviceAccountId")?,
            credential_id: id("credentialId")?,
            required_scopes: strings("requiredScopes")?,
            scopes: strings("scopes")?,
            service_account_status: nested("serviceAccount", "status")?.as_str()?.to_string(),
            credential_status: nested("credential", "status")?.as_str()?.to_string(),
            credential_expires_at: match nested("credential", "expiresAt") {
                None | Some(Value::Null) => None,
                Some(t) => Some(
                    DateTime::parse_from_rfc3339(t.as_str()?)
                        .ok()?
                        .with_timezone(&Utc),
                ),
            },
            authenticates: nested("credential", "authenticates")?.as_bool()?,
            revision: v.get("revision")?.as_i64()?,
            verified: v.get("verified")?.as_bool()?,
            active: v.get("active")?.as_bool()?,
            revoked,
        };
        // The nested ids must agree with the top-level ones.
        let consistent = nested("serviceAccount", "id").and_then(Value::as_str)
            == Some(facts.service_account_id.to_string().as_str())
            && nested("credential", "id").and_then(Value::as_str)
                == Some(facts.credential_id.to_string().as_str());
        consistent.then_some(facts)
    }

    /// Server-side verification: an ACTIVE, verified binding of exactly this
    /// organization, capability and service, inside the recorded triple, carrying exactly
    /// the service's scopes.
    pub fn verifies(
        &self,
        organization: Uuid,
        capability: &str,
        service: &str,
        triple: (Uuid, Uuid, Uuid),
    ) -> bool {
        let Some(expected) = binding_scopes(service) else {
            return false;
        };
        let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        self.organization_ref == organization
            && self.capability == capability
            && self.service == service
            && (self.tenant_id, self.application_id, self.environment_id) == triple
            && self.verified
            && self.active
            && self.authenticates
            && !self.revoked
            && self.service_account_status == "ACTIVE"
            && self.credential_status == "ACTIVE"
            && self.scopes == expected
            && self.required_scopes == expected
    }

    fn same_ids(&self, other: &BindingFacts) -> bool {
        self.service_account_id == other.service_account_id
            && self.credential_id == other.credential_id
    }
}

/// The Platform Access operations the saga uses.
#[async_trait]
pub trait PlatformScopes: Send + Sync {
    /// Creates or returns the organization's scope (idempotent).
    async fn provision(
        &self,
        organization: Uuid,
        capability: &str,
    ) -> Result<ScopeFacts, PortError>;
    /// Reads the scope back (`None`: Platform Access has none).
    async fn read(
        &self,
        organization: Uuid,
        capability: &str,
    ) -> Result<Option<ScopeFacts>, PortError>;
    /// Suspends (`true`) or resumes the organization's tenant.
    async fn set_status(
        &self,
        organization: Uuid,
        capability: &str,
        suspended: bool,
    ) -> Result<ScopeFacts, PortError>;

    /// Whether the saga provisions the organization's service bindings automatically
    /// (`ORGANIZATIONS_SERVICE_BINDINGS_ENABLED`; ADR 0058 A2). Off by default.
    fn bindings_enabled(&self) -> bool {
        false
    }

    /// Creates or returns (renewing a credential close to expiry) the organization's
    /// binding of `service` (idempotent).
    async fn ensure_binding(
        &self,
        _organization: Uuid,
        _capability: &str,
        _service: &str,
    ) -> Result<BindingFacts, PortError> {
        Err(not_supported())
    }

    /// Reads a binding back (`None`: Platform Access has none).
    async fn read_binding(
        &self,
        _organization: Uuid,
        _capability: &str,
        _service: &str,
    ) -> Result<Option<BindingFacts>, PortError> {
        Err(not_supported())
    }

    /// Issues a successor credential and revokes the predecessor.
    async fn rotate_binding(
        &self,
        _organization: Uuid,
        _capability: &str,
        _service: &str,
    ) -> Result<BindingFacts, PortError> {
        Err(not_supported())
    }

    /// Revokes the binding's credential and service account (`None`: never bound).
    async fn revoke_binding(
        &self,
        _organization: Uuid,
        _capability: &str,
        _service: &str,
    ) -> Result<Option<BindingFacts>, PortError> {
        Err(not_supported())
    }
}

fn not_supported() -> PortError {
    PortError::Refused {
        status: 501,
        code: "NOT_SUPPORTED".into(),
    }
}

/// Platform Access over signed internal HTTP (`platform-identity.provision-first-party`;
/// service bindings: `platform-identity.bind-organization-services`).
pub struct HttpPlatformScopes {
    client: Arc<InternalServiceClient>,
    bindings: bool,
}

impl HttpPlatformScopes {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self {
            client,
            bindings: false,
        }
    }

    /// Enables the automatic service bindings (ADR 0058 A2).
    pub fn with_bindings(mut self, enabled: bool) -> Self {
        self.bindings = enabled;
        self
    }

    fn path(organization: Uuid, capability: &str) -> String {
        format!("/internal/v1/organization-scopes/{organization}/{capability}")
    }

    fn binding_path(organization: Uuid, capability: &str, service: &str) -> String {
        format!(
            "{}/service-bindings/{service}",
            Self::path(organization, capability)
        )
    }

    /// One signed call; a 2xx body parsed by `parse` (an unparseable answer is retryable).
    async fn call_with<T>(
        &self,
        method: axum::http::Method,
        path: &str,
        body: Option<&Value>,
        parse: fn(&Value) -> Option<T>,
    ) -> Result<(u16, Option<T>, Option<String>), PortError> {
        let request = OutboundServiceRequest::new_operation(ServiceId::platform_access());
        let response = self
            .client
            .exchange_json(&request, method, path, body)
            .await
            .map_err(|e: InternalCallError| {
                tracing::warn!(error = %e, "platform access call failed");
                PortError::Unavailable("TRANSPORT".into())
            })?;
        let status = response.status();
        if (200..300).contains(&status) {
            let facts = response
                .json::<Value>()
                .ok()
                .as_ref()
                .and_then(parse)
                .ok_or_else(|| PortError::Unavailable("INVALID_RESPONSE".into()))?;
            return Ok((status, Some(facts), None));
        }
        Ok((status, None, response.error_code()))
    }

    async fn call(
        &self,
        method: axum::http::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Option<ScopeFacts>, Option<String>), PortError> {
        self.call_with(method, path, body, ScopeFacts::from_json)
            .await
    }

    async fn binding_call(
        &self,
        method: axum::http::Method,
        path: &str,
    ) -> Result<(u16, Option<BindingFacts>, Option<String>), PortError> {
        let body = (method != axum::http::Method::GET && method != axum::http::Method::DELETE)
            .then(|| json!({}));
        self.call_with(method, path, body.as_ref(), BindingFacts::from_json)
            .await
    }

    fn refusal(status: u16, code: Option<String>) -> PortError {
        if status == 429 || status >= 500 {
            PortError::Unavailable(format!("HTTP_{status}"))
        } else {
            PortError::Refused {
                status,
                code: code.unwrap_or_else(|| "UNKNOWN".into()),
            }
        }
    }
}

#[async_trait]
impl PlatformScopes for HttpPlatformScopes {
    async fn provision(
        &self,
        organization: Uuid,
        capability: &str,
    ) -> Result<ScopeFacts, PortError> {
        match self
            .call(
                axum::http::Method::PUT,
                &Self::path(organization, capability),
                None,
            )
            .await?
        {
            (_, Some(f), _) => Ok(f),
            (status, None, code) => Err(Self::refusal(status, code)),
        }
    }

    async fn read(
        &self,
        organization: Uuid,
        capability: &str,
    ) -> Result<Option<ScopeFacts>, PortError> {
        match self
            .call(
                axum::http::Method::GET,
                &Self::path(organization, capability),
                None,
            )
            .await?
        {
            (_, Some(f), _) => Ok(Some(f)),
            (404, None, _) => Ok(None),
            (status, None, code) => Err(Self::refusal(status, code)),
        }
    }

    async fn set_status(
        &self,
        organization: Uuid,
        capability: &str,
        suspended: bool,
    ) -> Result<ScopeFacts, PortError> {
        let body = json!({ "status": if suspended { "SUSPENDED" } else { "ACTIVE" } });
        match self
            .call(
                axum::http::Method::POST,
                &format!("{}/status", Self::path(organization, capability)),
                Some(&body),
            )
            .await?
        {
            (_, Some(f), _) => Ok(f),
            (status, None, code) => Err(Self::refusal(status, code)),
        }
    }

    fn bindings_enabled(&self) -> bool {
        self.bindings
    }

    async fn ensure_binding(
        &self,
        organization: Uuid,
        capability: &str,
        service: &str,
    ) -> Result<BindingFacts, PortError> {
        match self
            .binding_call(
                axum::http::Method::PUT,
                &Self::binding_path(organization, capability, service),
            )
            .await?
        {
            (_, Some(f), _) => Ok(f),
            (status, None, code) => Err(Self::refusal(status, code)),
        }
    }

    async fn read_binding(
        &self,
        organization: Uuid,
        capability: &str,
        service: &str,
    ) -> Result<Option<BindingFacts>, PortError> {
        match self
            .binding_call(
                axum::http::Method::GET,
                &Self::binding_path(organization, capability, service),
            )
            .await?
        {
            (_, Some(f), _) => Ok(Some(f)),
            (404, None, _) => Ok(None),
            (status, None, code) => Err(Self::refusal(status, code)),
        }
    }

    async fn rotate_binding(
        &self,
        organization: Uuid,
        capability: &str,
        service: &str,
    ) -> Result<BindingFacts, PortError> {
        match self
            .binding_call(
                axum::http::Method::POST,
                &format!(
                    "{}/rotate",
                    Self::binding_path(organization, capability, service)
                ),
            )
            .await?
        {
            (_, Some(f), _) => Ok(f),
            (status, None, code) => Err(Self::refusal(status, code)),
        }
    }

    async fn revoke_binding(
        &self,
        organization: Uuid,
        capability: &str,
        service: &str,
    ) -> Result<Option<BindingFacts>, PortError> {
        match self
            .binding_call(
                axum::http::Method::DELETE,
                &Self::binding_path(organization, capability, service),
            )
            .await?
        {
            (_, Some(f), _) => Ok(Some(f)),
            (404, None, _) => Ok(None),
            (status, None, code) => Err(Self::refusal(status, code)),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------------------

pub const WORKFLOW_COLUMNS: &str = "workflow_id, organization_id, idempotency_key, capability, state, step, attempts, last_error_class, next_attempt_at, version, created_at, updated_at";

#[derive(Debug, Clone, FromQueryResult)]
pub struct WorkflowRow {
    pub workflow_id: Uuid,
    pub organization_id: Uuid,
    pub idempotency_key: String,
    pub capability: String,
    pub state: String,
    pub step: Option<String>,
    pub attempts: i32,
    pub last_error_class: Option<String>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const MAPPING_COLUMNS: &str = "id, organization_id, capability, tenant_id, application_id, environment_id, mapping_state, mapping_version, verified_at, tenant_status_desired, tenant_status_applied, sync_attempts, next_sync_at, last_sync_error_class, verification_evidence, created_at, updated_at";

#[derive(Debug, Clone, FromQueryResult)]
pub struct MappingRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub capability: String,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub mapping_state: String,
    pub mapping_version: i64,
    pub verified_at: Option<DateTime<Utc>>,
    pub tenant_status_desired: Option<String>,
    pub tenant_status_applied: Option<String>,
    pub sync_attempts: i32,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub last_sync_error_class: Option<String>,
    pub verification_evidence: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MappingRow {
    /// Whether the tenant's status in Platform Access is the one Organizations wants.
    pub fn in_sync(&self) -> bool {
        self.tenant_status_desired.is_none()
            || self.tenant_status_desired == self.tenant_status_applied
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "capability": self.capability,
            "mappingState": self.mapping_state,
            "mappingVersion": self.mapping_version,
            "tenantId": self.tenant_id,
            "applicationId": self.application_id,
            "environmentId": self.environment_id,
            "verifiedAt": self.verified_at.map(store::rfc3339),
            "tenantStatusDesired": self.tenant_status_desired,
            "tenantStatusApplied": self.tenant_status_applied,
            "syncAttempts": self.sync_attempts,
            "lastSyncErrorClass": self.last_sync_error_class,
        })
    }

    fn event(&self) -> Event<'static> {
        Event {
            organization_id: self.organization_id,
            aggregate_type: "scope",
            aggregate_id: self.id,
            aggregate_version: self.mapping_version,
            event_type: "scope.mapping_changed",
            payload: json!({
                "capability": self.capability,
                "tenantId": self.tenant_id,
                "applicationId": self.application_id,
                "environmentId": self.environment_id,
                "mappingState": self.mapping_state,
                "mappingVersion": self.mapping_version,
            }),
        }
    }
}

pub async fn workflow<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
) -> Result<Option<WorkflowRow>, DbErr> {
    db::one(
        c,
        &format!("SELECT {WORKFLOW_COLUMNS} FROM organization_provisioning WHERE organization_id = $1 ORDER BY created_at DESC LIMIT 1"),
        vec![organization.into()],
    )
    .await
}

/// The current (non-retired) mapping of an organization and capability.
pub async fn mapping<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    capability: &str,
    lock: bool,
) -> Result<Option<MappingRow>, DbErr> {
    db::one(
        c,
        &format!(
            "SELECT {MAPPING_COLUMNS} FROM organization_platform_scopes WHERE organization_id = $1 AND capability = $2 AND mapping_state <> 'RETIRED'{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![organization.into(), capability.into()],
    )
    .await
}

fn system() -> OpContext {
    OpContext::system(cf_service_auth::services::ORGANIZATIONS)
}

fn provisioning_event(w: &WorkflowRow, event_type: &'static str, payload: Value) -> Event<'static> {
    Event {
        organization_id: w.organization_id,
        aggregate_type: "provisioning",
        aggregate_id: w.workflow_id,
        aggregate_version: w.version,
        event_type,
        payload,
    }
}

/// Sets the organization's provisioning state; `true` when it changed.
async fn set_org_state<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    state: &str,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    Ok(db::exec(
        c,
        "UPDATE organizations SET provisioning_state = $2, updated_at = $3 WHERE id = $1 AND provisioning_state <> $2",
        vec![organization.into(), state.into(), now.into()],
    )
    .await?
        == 1)
}

/// Writes `sql` (`SET …` of the workflow, `version = version + 1` included by the caller)
/// and returns the updated row.
async fn update_workflow<C: ConnectionTrait>(
    c: &C,
    workflow_id: Uuid,
    set: &str,
    mut values: Vec<sea_orm::Value>,
) -> Result<WorkflowRow, DbErr> {
    values.insert(0, workflow_id.into());
    db::one(
        c,
        &format!("UPDATE organization_provisioning SET {set} WHERE workflow_id = $1 RETURNING {WORKFLOW_COLUMNS}"),
        values,
    )
    .await?
    .ok_or_else(|| DbErr::Custom("provisioning workflow vanished".into()))
}

async fn update_mapping<C: ConnectionTrait>(
    c: &C,
    id: Uuid,
    set: &str,
    mut values: Vec<sea_orm::Value>,
) -> Result<MappingRow, DbErr> {
    values.insert(0, id.into());
    db::one(
        c,
        &format!("UPDATE organization_platform_scopes SET {set} WHERE id = $1 RETURNING {MAPPING_COLUMNS}"),
        values,
    )
    .await?
    .ok_or_else(|| DbErr::Custom("organization scope mapping vanished".into()))
}

// ---------------------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProvisioningReport {
    pub enqueued: u64,
    pub ready: u64,
    pub failed: u64,
    pub retried: u64,
    pub waiting: u64,
    pub cancelled: u64,
    pub lease_lost: u64,
    pub local_errors: u64,
    pub synced: u64,
    pub sync_failed: u64,
    pub reconciled: u64,
    pub drifted: u64,
    /// Organizations whose service bindings were checked (backfill / renewal).
    pub bindings_checked: u64,
    /// Service-binding drift detected (`SERVICE_BINDING_DRIFT`).
    pub bindings_drifted: u64,
    /// Binding revocations applied at Platform Access / still failing.
    pub revocations_applied: u64,
    pub revocations_failed: u64,
}

/// One provisioning pass: enqueue, advance due workflows, apply tenant statuses,
/// reconcile. Every step is idempotent and multi-instance safe.
pub async fn run_once(
    db: &DatabaseConnection,
    port: &dyn PlatformScopes,
) -> Result<ProvisioningReport, DbErr> {
    let mut report = ProvisioningReport {
        enqueued: enqueue(db, 100).await?,
        ..Default::default()
    };
    let lease = Uuid::new_v4();
    for w in claim(db, lease, 20).await? {
        // A local failure abandons this step only: the lease expires and the workflow is
        // reclaimed (Platform Access is idempotent, so the retry confirms the same triple).
        match advance(db, port, &w, lease).await {
            Ok(Outcome::Ready) => report.ready += 1,
            Ok(Outcome::Failed) => report.failed += 1,
            Ok(Outcome::Retry) => report.retried += 1,
            Ok(Outcome::Waiting) => report.waiting += 1,
            Ok(Outcome::Cancelled) => report.cancelled += 1,
            Ok(Outcome::LeaseLost) => report.lease_lost += 1,
            Err(e) => {
                tracing::warn!(organization_id = %w.organization_id, summary = %db::summarize_db_error(&e), "provisioning step failed locally");
                report.local_errors += 1;
            }
        }
    }
    let (synced, sync_failed) = sync_tenant_status(db, port, 50).await?;
    report.synced = synced;
    report.sync_failed = sync_failed;
    let (reconciled, drifted) = reconcile(db, port, 20).await?;
    report.reconciled = reconciled;
    report.drifted = drifted;
    if port.bindings_enabled() {
        let (checked, drifted) = reconcile_bindings(db, port, 20).await?;
        report.bindings_checked = checked;
        report.bindings_drifted = drifted;
    }
    // Revocations recorded locally (archive / operator removal of an automatic binding)
    // are applied even if automation was switched off since: never abandoned.
    let (applied, failed) = sync_binding_revocations(db, port, 50).await?;
    report.revocations_applied = applied;
    report.revocations_failed = failed;
    Ok(report)
}

/// Creates the workflow of every ACTIVE, PENDING organization that has none
/// (`ON CONFLICT DO NOTHING` on the stable key and the one-open-workflow index: duplicate
/// requests and concurrent workers create exactly one).
pub async fn enqueue(db: &DatabaseConnection, limit: i64) -> Result<u64, DbErr> {
    let txn = db.begin().await?;
    let now = db::now(&txn).await?;
    let rows: Vec<WorkflowRow> = db::all(
        &txn,
        &format!(
            "INSERT INTO organization_provisioning (workflow_id, organization_id, idempotency_key, capability, state, step, attempts, next_attempt_at, created_at, updated_at, version) \
             SELECT gen_random_uuid(), o.id, 'org-scope:' || o.id::text || ':' || $2, $2, 'PENDING', 'REQUESTED', 0, $3, $3, $3, 1 \
             FROM organizations o \
             WHERE o.provisioning_state = 'PENDING' AND o.lifecycle_status = 'ACTIVE' \
               AND NOT EXISTS (SELECT 1 FROM organization_provisioning w WHERE w.organization_id = o.id) \
             ORDER BY o.created_at LIMIT $1 \
             ON CONFLICT DO NOTHING RETURNING {WORKFLOW_COLUMNS}"
        ),
        vec![limit.into(), CAPABILITY.into(), now.into()],
    )
    .await?;
    let ctx = system();
    for w in &rows {
        store::emit(
            &txn,
            &ctx,
            now,
            provisioning_event(
                w,
                "organization.provisioning_changed",
                json!({ "provisioningState": "PENDING", "workflowState": w.state, "step": w.step }),
            ),
        )
        .await?;
        store::audit(
            &txn,
            &ctx,
            now,
            Some(w.organization_id),
            "provisioning.requested",
            "organization",
            Some(w.organization_id),
            json!({ "workflowId": w.workflow_id, "capability": w.capability }),
        )
        .await?;
    }
    txn.commit().await?;
    Ok(rows.len() as u64)
}

/// Leases up to `limit` due workflows (`FOR UPDATE SKIP LOCKED`; an expired lease is
/// reclaimed) and counts the attempt.
pub async fn claim(
    db: &DatabaseConnection,
    lease: Uuid,
    limit: i64,
) -> Result<Vec<WorkflowRow>, DbErr> {
    let txn = db.begin().await?;
    let rows: Vec<WorkflowRow> = db::all(
        &txn,
        &format!(
            "WITH due AS (SELECT workflow_id FROM organization_provisioning \
                 WHERE state IN ('PENDING', 'IN_PROGRESS') AND COALESCE(next_attempt_at, now()) <= now() \
                   AND (lease_until IS NULL OR lease_until <= now()) \
                 ORDER BY next_attempt_at NULLS FIRST, created_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
             UPDATE organization_provisioning w SET state = 'IN_PROGRESS', attempts = w.attempts + 1, lease_id = $1, \
                 lease_until = now() + make_interval(secs => $3), updated_at = now() \
             FROM due WHERE w.workflow_id = due.workflow_id RETURNING {}",
            WORKFLOW_COLUMNS
                .split(", ")
                .map(|c| format!("w.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        vec![lease.into(), limit.into(), (LEASE_SECONDS as f64).into()],
    )
    .await?;
    txn.commit().await?;
    Ok(rows)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ready,
    Failed,
    Retry,
    Waiting,
    Cancelled,
    LeaseLost,
}

/// Locks the organization, then the workflow (still held by `lease`): the lock order of
/// every writer (lifecycle changes lock the organization first too).
async fn locked(
    txn: &DatabaseTransaction,
    w: &WorkflowRow,
    lease: Uuid,
) -> Result<Option<(store::OrgRow, WorkflowRow)>, DbErr> {
    let Some(org) = store::org(txn, w.organization_id, true).await? else {
        return Ok(None);
    };
    let row: Option<WorkflowRow> = db::one(
        txn,
        &format!("SELECT {WORKFLOW_COLUMNS} FROM organization_provisioning WHERE workflow_id = $1 AND lease_id = $2 AND state = 'IN_PROGRESS' FOR UPDATE"),
        vec![w.workflow_id.into(), lease.into()],
    )
    .await?;
    Ok(row.map(|r| (org, r)))
}

/// Drives one claimed workflow as far as it can go now.
pub async fn advance(
    db: &DatabaseConnection,
    port: &dyn PlatformScopes,
    w: &WorkflowRow,
    lease: Uuid,
) -> Result<Outcome, DbErr> {
    let Some(org) = store::org(db, w.organization_id, false).await? else {
        return settle_failed(db, w, lease, "ORGANIZATION_MISSING", None).await;
    };
    let bindings = port.bindings_enabled();
    if bindings && org.status() != LifecycleStatus::Archived && in_binding_phase(w) {
        // The scope is already verified and recorded: resume with the bindings.
        return advance_bindings(db, port, w, lease).await;
    }
    let requested = w.step.as_deref() == Some("SCOPE_CREATED");
    match org.status() {
        LifecycleStatus::Archived => return settle_cancelled(db, w, lease).await,
        // Nothing is created for a suspended organization; once a scope exists the
        // lifecycle keeps its tenant suspended and verification may finish.
        LifecycleStatus::Suspended if !requested => return settle_wait(db, w, lease).await,
        _ => {}
    }
    let facts = match port.provision(w.organization_id, &w.capability).await {
        Ok(f) => f,
        Err(e) => return settle_error(db, w, lease, &e).await,
    };
    if !facts.verifies(w.organization_id, &w.capability) {
        return settle_failed(db, w, lease, "SCOPE_VERIFICATION_FAILED", Some(&facts)).await;
    }
    match checkpoint(db, w, lease, &facts).await? {
        Checkpoint::Recorded => {}
        Checkpoint::LeaseLost => return Ok(Outcome::LeaseLost),
        Checkpoint::Mismatch => {
            return settle_failed(db, w, lease, "SCOPE_MISMATCH", None).await;
        }
    }
    let mut read = match port.read(w.organization_id, &w.capability).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return settle_failed(db, w, lease, "SCOPE_VERIFICATION_FAILED", None).await;
        }
        Err(e) => return settle_error(db, w, lease, &e).await,
    };
    if !read.verifies(w.organization_id, &w.capability)
        || !read.same_triple(facts.tenant_id, facts.application_id, facts.environment_id)
    {
        return settle_failed(db, w, lease, "SCOPE_VERIFICATION_FAILED", None).await;
    }
    // READY is only recorded for an ACTIVE tenant of an ACTIVE organization (a tenant a
    // previous failure compensated is resumed first).
    let active_now = store::org(db, w.organization_id, false)
        .await?
        .is_some_and(|o| o.status() == LifecycleStatus::Active);
    if active_now && read.tenant_status != "ACTIVE" {
        read = match port
            .set_status(w.organization_id, &w.capability, false)
            .await
        {
            Ok(f)
                if f.tenant_status == "ACTIVE"
                    && f.same_triple(
                        facts.tenant_id,
                        facts.application_id,
                        facts.environment_id,
                    ) =>
            {
                f
            }
            Ok(_) => {
                return settle_error(
                    db,
                    w,
                    lease,
                    &PortError::Unavailable("TENANT_NOT_RESUMED".into()),
                )
                .await
            }
            Err(e) => return settle_error(db, w, lease, &e).await,
        };
    }
    if !bindings {
        return finish(db, w, lease, &read).await;
    }
    // ADR 0058 A2: the organization becomes READY only once its service bindings are
    // provisioned and verified too.
    match scope_verified(db, w, lease, &read).await? {
        Some(outcome) => Ok(outcome),
        None => advance_bindings(db, port, w, lease).await,
    }
}

enum Checkpoint {
    Recorded,
    LeaseLost,
    Mismatch,
}

/// `SCOPE_CREATED`: records the triple Platform Access returned on the mapping (VERIFYING).
async fn checkpoint(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    facts: &ScopeFacts,
) -> Result<Checkpoint, DbErr> {
    let txn = db.begin().await?;
    let Some((_org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Checkpoint::LeaseLost);
    };
    let now = db::now(&txn).await?;
    let ctx = system();
    // A tenant already mapped to ANOTHER organization is never mapped twice.
    let foreign = db::scalar_i64(
        &txn,
        "SELECT count(*)::bigint AS n FROM organization_platform_scopes WHERE tenant_id = $1 AND organization_id <> $2 AND mapping_state <> 'RETIRED'",
        vec![facts.tenant_id.into(), w.organization_id.into()],
    )
    .await?;
    if foreign > 0 {
        return Ok(Checkpoint::Mismatch);
    }
    match mapping(&txn, w.organization_id, &w.capability, true).await? {
        Some(m) => {
            if let (Some(t), Some(a), Some(e)) = (m.tenant_id, m.application_id, m.environment_id) {
                if !facts.same_triple(t, a, e) {
                    return Ok(Checkpoint::Mismatch);
                }
            }
            if m.mapping_state != "READY" {
                update_mapping(
                    &txn,
                    m.id,
                    "tenant_id = $2, application_id = $3, environment_id = $4, mapping_state = 'VERIFYING', tenant_status_applied = $5, updated_at = $6",
                    vec![
                        facts.tenant_id.into(),
                        facts.application_id.into(),
                        facts.environment_id.into(),
                        facts.tenant_status.clone().into(),
                        now.into(),
                    ],
                )
                .await?;
            }
        }
        None => {
            db::exec(
                &txn,
                "INSERT INTO organization_platform_scopes (id, organization_id, capability, tenant_id, application_id, environment_id, mapping_state, mapping_version, tenant_status_applied, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'VERIFYING', 1, $7, $8, $8)",
                vec![
                    Uuid::new_v4().into(),
                    w.organization_id.into(),
                    w.capability.clone().into(),
                    facts.tenant_id.into(),
                    facts.application_id.into(),
                    facts.environment_id.into(),
                    facts.tenant_status.clone().into(),
                    now.into(),
                ],
            )
            .await?;
        }
    }
    let after = update_workflow(
        &txn,
        current.workflow_id,
        "step = 'SCOPE_CREATED', last_error_class = NULL, version = version + 1, updated_at = $2",
        vec![now.into()],
    )
    .await?;
    let changed = set_org_state(&txn, w.organization_id, "IN_PROGRESS", now).await?;
    store::emit(
        &txn,
        &ctx,
        now,
        provisioning_event(
            &after,
            "organization.provisioning_changed",
            json!({ "provisioningState": "IN_PROGRESS", "workflowState": after.state, "step": after.step, "organizationStateChanged": changed }),
        ),
    )
    .await?;
    txn.commit().await?;
    Ok(Checkpoint::Recorded)
}

/// `VERIFIED`: the mapping becomes READY (SUSPENDED for a suspended organization), the
/// organization READY; `scope.mapping_changed` and `organization.provisioned` are emitted.
async fn finish(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    facts: &ScopeFacts,
) -> Result<Outcome, DbErr> {
    let txn = db.begin().await?;
    let Some((org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let now = db::now(&txn).await?;
    let ctx = system();
    let Some(m) = mapping(&txn, w.organization_id, &w.capability, true).await? else {
        return Ok(Outcome::LeaseLost);
    };
    if !m
        .tenant_id
        .zip(m.application_id)
        .zip(m.environment_id)
        .is_some_and(|((t, a), e)| facts.same_triple(t, a, e))
    {
        drop(txn);
        return settle_failed(db, w, lease, "SCOPE_MISMATCH", None).await;
    }
    let (state, desired) = match org.status() {
        LifecycleStatus::Active => ("READY", "ACTIVE"),
        _ => ("SUSPENDED", "SUSPENDED"),
    };
    let after_mapping = update_mapping(
        &txn,
        m.id,
        "mapping_state = $2, mapping_version = mapping_version + 1, verified_at = $3, verification_evidence = $4, tenant_status_desired = $5, tenant_status_applied = $6, sync_attempts = 0, next_sync_at = $3, last_sync_error_class = NULL, updated_at = $3",
        vec![
            state.into(),
            now.into(),
            facts.evidence().into(),
            desired.into(),
            facts.tenant_status.clone().into(),
        ],
    )
    .await?;
    let after = update_workflow(
        &txn,
        current.workflow_id,
        "state = 'READY', step = 'VERIFIED', lease_id = NULL, lease_until = NULL, next_attempt_at = NULL, last_error_class = NULL, version = version + 1, updated_at = $2",
        vec![now.into()],
    )
    .await?;
    set_org_state(&txn, w.organization_id, "READY", now).await?;
    store::emit(&txn, &ctx, now, after_mapping.event()).await?;
    store::emit(
        &txn,
        &ctx,
        now,
        provisioning_event(
            &after,
            "organization.provisioned",
            json!({
                "provisioningState": "READY",
                "scopes": [{
                    "capability": after_mapping.capability,
                    "tenantId": after_mapping.tenant_id,
                    "applicationId": after_mapping.application_id,
                    "environmentId": after_mapping.environment_id,
                    "mappingVersion": after_mapping.mapping_version,
                }],
            }),
        ),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(w.organization_id),
        "provisioning.ready",
        "organization",
        Some(w.organization_id),
        json!({
            "workflowId": after.workflow_id,
            "capability": after_mapping.capability,
            "tenantId": after_mapping.tenant_id,
            "applicationId": after_mapping.application_id,
            "environmentId": after_mapping.environment_id,
            "mappingVersion": after_mapping.mapping_version,
            "mappingState": after_mapping.mapping_state,
        }),
    )
    .await?;
    txn.commit().await?;
    Ok(Outcome::Ready)
}

// ---------------------------------------------------------------------------------------
// Service-binding phase of the saga (ADR 0058 Amendment A2)
// ---------------------------------------------------------------------------------------

/// Workflow step recorded once the scope is verified (bindings still to do).
pub const STEP_SCOPE_VERIFIED: &str = "SCOPE_VERIFIED";

/// The step recorded after `service`'s binding was verified and recorded.
pub fn binding_step(service: &str) -> String {
    format!("BINDING_{}_VERIFIED", service.to_ascii_uppercase())
}

fn in_binding_phase(w: &WorkflowRow) -> bool {
    w.step
        .as_deref()
        .is_some_and(|s| s == STEP_SCOPE_VERIFIED || s.starts_with("BINDING_"))
}

/// `SCOPE_VERIFIED` (bindings enabled): the mapping is recorded exactly as [`finish`]
/// records it — unless an earlier pass (an operator retry) already did, then nothing is
/// rewritten or re-emitted — and the workflow stays IN_PROGRESS under the same lease.
/// `None`: continue with the bindings.
async fn scope_verified(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    facts: &ScopeFacts,
) -> Result<Option<Outcome>, DbErr> {
    let txn = db.begin().await?;
    let Some((org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Some(Outcome::LeaseLost));
    };
    let now = db::now(&txn).await?;
    let ctx = system();
    let Some(m) = mapping(&txn, w.organization_id, &w.capability, true).await? else {
        return Ok(Some(Outcome::LeaseLost));
    };
    if !m
        .tenant_id
        .zip(m.application_id)
        .zip(m.environment_id)
        .is_some_and(|((t, a), e)| facts.same_triple(t, a, e))
    {
        drop(txn);
        return settle_failed(db, w, lease, "SCOPE_MISMATCH", None)
            .await
            .map(Some);
    }
    let (state, desired) = match org.status() {
        LifecycleStatus::Active => ("READY", "ACTIVE"),
        _ => ("SUSPENDED", "SUSPENDED"),
    };
    let already = m.mapping_state == state
        && m.verified_at.is_some()
        && m.tenant_status_desired.as_deref() == Some(desired);
    if !already {
        let after_mapping = update_mapping(
            &txn,
            m.id,
            "mapping_state = $2, mapping_version = mapping_version + 1, verified_at = $3, verification_evidence = $4, tenant_status_desired = $5, tenant_status_applied = $6, sync_attempts = 0, next_sync_at = $3, last_sync_error_class = NULL, updated_at = $3",
            vec![
                state.into(),
                now.into(),
                facts.evidence().into(),
                desired.into(),
                facts.tenant_status.clone().into(),
            ],
        )
        .await?;
        store::emit(&txn, &ctx, now, after_mapping.event()).await?;
    }
    let after = update_workflow(
        &txn,
        current.workflow_id,
        "step = $2, last_error_class = NULL, version = version + 1, updated_at = $3",
        vec![STEP_SCOPE_VERIFIED.into(), now.into()],
    )
    .await?;
    store::emit(
        &txn,
        &ctx,
        now,
        provisioning_event(
            &after,
            "organization.provisioning_changed",
            json!({ "provisioningState": "IN_PROGRESS", "workflowState": after.state, "step": after.step }),
        ),
    )
    .await?;
    txn.commit().await?;
    Ok(None)
}

/// The service bindings: for each service, Platform Access `PUT` (create or return),
/// `GET` read-back, server-side verification, then the ids are recorded (one local
/// transaction per service, checkpointed in the workflow step). Only when every service
/// is bound does the organization become READY ([`finish_bindings`]). An OPERATOR binding
/// is respected and a binding revoked locally (archive / operator) is never recreated.
async fn advance_bindings(
    db: &DatabaseConnection,
    port: &dyn PlatformScopes,
    w: &WorkflowRow,
    lease: Uuid,
) -> Result<Outcome, DbErr> {
    let Some(org) = store::org(db, w.organization_id, false).await? else {
        return settle_failed(db, w, lease, "ORGANIZATION_MISSING", None).await;
    };
    match org.status() {
        LifecycleStatus::Archived => return settle_cancelled(db, w, lease).await,
        LifecycleStatus::Suspended => {
            return settle_wait_keep_step(db, w, lease, SUSPENDED_WAIT_SECONDS).await
        }
        LifecycleStatus::Active => {}
    }
    let Some(m) = mapping(db, w.organization_id, &w.capability, false).await? else {
        return settle_failed(db, w, lease, "SCOPE_MISMATCH", None).await;
    };
    let Some(triple) = m
        .tenant_id
        .zip(m.application_id)
        .zip(m.environment_id)
        .map(|((t, a), e)| (t, a, e))
    else {
        return settle_failed(db, w, lease, "SCOPE_MISMATCH", None).await;
    };
    // Credentials can only be issued under an ACTIVE tenant: wait for a pending resume.
    if m.mapping_state != "READY"
        || m.tenant_status_desired.as_deref() != Some("ACTIVE")
        || !m.in_sync()
    {
        return settle_wait_keep_step(db, w, lease, backoff_seconds(1)).await;
    }
    for service in BINDING_SERVICES {
        if !automation_applies(db, w.organization_id, &w.capability, service).await? {
            continue;
        }
        let put = match port
            .ensure_binding(w.organization_id, &w.capability, service)
            .await
        {
            Ok(f) => f,
            Err(e) => return settle_binding_error(db, w, lease, &e).await,
        };
        if !put.verifies(w.organization_id, &w.capability, service, triple) {
            return settle_failed(db, w, lease, "SERVICE_BINDING_VERIFICATION_FAILED", None).await;
        }
        let read = match port
            .read_binding(w.organization_id, &w.capability, service)
            .await
        {
            Ok(Some(f)) => f,
            Ok(None) => {
                return settle_failed(db, w, lease, "SERVICE_BINDING_VERIFICATION_FAILED", None)
                    .await
            }
            Err(e) => return settle_binding_error(db, w, lease, &e).await,
        };
        if !read.verifies(w.organization_id, &w.capability, service, triple) || !read.same_ids(&put)
        {
            return settle_failed(db, w, lease, "SERVICE_BINDING_VERIFICATION_FAILED", None).await;
        }
        let txn = db.begin().await?;
        let Some((_org, current)) = locked(&txn, w, lease).await? else {
            return Ok(Outcome::LeaseLost);
        };
        let now = db::now(&txn).await?;
        upsert_automatic_binding(
            &txn,
            &system(),
            now,
            w.organization_id,
            &w.capability,
            service,
            &read,
        )
        .await?;
        update_workflow(
            &txn,
            current.workflow_id,
            "step = $2, last_error_class = NULL, version = version + 1, updated_at = $3",
            vec![binding_step(service).into(), now.into()],
        )
        .await?;
        txn.commit().await?;
    }
    finish_bindings(db, w, lease).await
}

/// Whether automation manages `service`'s binding of this organization: not when an
/// operator recorded one (respected) or a binding was revoked locally (archive /
/// operator removal: never recreated automatically).
async fn automation_applies<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    capability: &str,
    service: &str,
) -> Result<bool, DbErr> {
    let manual = db::scalar_i64(
        c,
        "SELECT count(*)::bigint AS n FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3 AND (source = 'OPERATOR' OR revoked_at IS NOT NULL)",
        vec![organization.into(), capability.into(), service.into()],
    )
    .await?;
    Ok(manual == 0)
}

/// How recording an automatic binding changed the local row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingChange {
    Inserted,
    Rotated,
    Unchanged,
    /// An operator binding or a locally revoked binding: left untouched.
    Skipped,
}

#[derive(Debug, FromQueryResult)]
struct LocalBinding {
    service_account_id: Uuid,
    credential_id: Uuid,
    source: String,
    revoked_at: Option<DateTime<Utc>>,
    version: i64,
}

/// Records Platform Access's verified binding facts (ids, verification time, credential
/// expiry) as an AUTOMATIC binding and audits a creation or a credential change. Never
/// touches an OPERATOR binding or a locally revoked one.
pub async fn upsert_automatic_binding<C: ConnectionTrait>(
    c: &C,
    ctx: &OpContext,
    now: DateTime<Utc>,
    organization: Uuid,
    capability: &str,
    service: &str,
    facts: &BindingFacts,
) -> Result<BindingChange, DbErr> {
    let current: Option<LocalBinding> = db::one(
        c,
        "SELECT service_account_id, credential_id, source, revoked_at, version FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3 FOR UPDATE",
        vec![organization.into(), capability.into(), service.into()],
    )
    .await?;
    let change = match &current {
        None => {
            db::exec(
                c,
                "INSERT INTO organization_scope_service_bindings \
                   (organization_id, capability, service, service_account_id, credential_id, bound_by, created_at, updated_at, version, source, verified_at, credential_expires_at) \
                 VALUES ($1, $2, $3, $4, $5, NULL, $6, $6, 1, 'AUTOMATIC', $6, $7)",
                vec![
                    organization.into(),
                    capability.into(),
                    service.into(),
                    facts.service_account_id.into(),
                    facts.credential_id.into(),
                    now.into(),
                    facts.credential_expires_at.into(),
                ],
            )
            .await?;
            BindingChange::Inserted
        }
        Some(b) if b.source != "AUTOMATIC" || b.revoked_at.is_some() => BindingChange::Skipped,
        Some(b) => {
            let changed = b.service_account_id != facts.service_account_id
                || b.credential_id != facts.credential_id;
            db::exec(
                c,
                "UPDATE organization_scope_service_bindings SET service_account_id = $4, credential_id = $5, verified_at = $6, credential_expires_at = $7, \
                   version = version + $8, updated_at = CASE WHEN $8 = 1 THEN $6 ELSE updated_at END \
                 WHERE organization_id = $1 AND capability = $2 AND service = $3",
                vec![
                    organization.into(),
                    capability.into(),
                    service.into(),
                    facts.service_account_id.into(),
                    facts.credential_id.into(),
                    now.into(),
                    facts.credential_expires_at.into(),
                    (changed as i64).into(),
                ],
            )
            .await?;
            if changed {
                BindingChange::Rotated
            } else {
                BindingChange::Unchanged
            }
        }
    };
    let action = match change {
        BindingChange::Inserted => "scope.service_binding_provisioned",
        BindingChange::Rotated => "scope.service_binding_rotated",
        BindingChange::Unchanged | BindingChange::Skipped => return Ok(change),
    };
    let version = current.as_ref().map(|b| b.version + 1).unwrap_or(1);
    store::audit(
        c,
        ctx,
        now,
        Some(organization),
        action,
        "organization",
        Some(organization),
        json!({
            "capability": capability,
            "service": service,
            "source": "AUTOMATIC",
            "serviceAccountId": facts.service_account_id,
            "credentialId": facts.credential_id,
            "previousCredentialId": current.as_ref().map(|b| b.credential_id),
            "credentialExpiresAt": facts.credential_expires_at.map(store::rfc3339),
            "platformRevision": facts.revision,
            "version": version,
        }),
    )
    .await?;
    Ok(change)
}

/// Every binding is recorded: the workflow and the organization become READY and
/// `organization.provisioned` is emitted (the same payload as without bindings).
async fn finish_bindings(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
) -> Result<Outcome, DbErr> {
    let txn = db.begin().await?;
    let Some((_org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let now = db::now(&txn).await?;
    let ctx = system();
    let Some(m) = mapping(&txn, w.organization_id, &w.capability, false).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let services: Vec<String> = db::all::<ServiceName, _>(
        &txn,
        "SELECT service FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND revoked_at IS NULL ORDER BY service",
        vec![w.organization_id.into(), w.capability.clone().into()],
    )
    .await?
    .into_iter()
    .map(|r| r.service)
    .collect();
    let after = update_workflow(
        &txn,
        current.workflow_id,
        "state = 'READY', step = 'VERIFIED', lease_id = NULL, lease_until = NULL, next_attempt_at = NULL, last_error_class = NULL, version = version + 1, updated_at = $2",
        vec![now.into()],
    )
    .await?;
    set_org_state(&txn, w.organization_id, "READY", now).await?;
    db::exec(
        &txn,
        "UPDATE organization_platform_scopes SET bindings_next_check_at = $2 + make_interval(secs => $3), bindings_check_attempts = 0, bindings_last_error_class = NULL WHERE id = $1",
        vec![m.id.into(), now.into(), (RECONCILE_AFTER_SECONDS as f64).into()],
    )
    .await?;
    store::emit(
        &txn,
        &ctx,
        now,
        provisioning_event(
            &after,
            "organization.provisioned",
            json!({
                "provisioningState": "READY",
                "scopes": [{
                    "capability": m.capability,
                    "tenantId": m.tenant_id,
                    "applicationId": m.application_id,
                    "environmentId": m.environment_id,
                    "mappingVersion": m.mapping_version,
                }],
            }),
        ),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(w.organization_id),
        "provisioning.ready",
        "organization",
        Some(w.organization_id),
        json!({
            "workflowId": after.workflow_id,
            "capability": m.capability,
            "tenantId": m.tenant_id,
            "applicationId": m.application_id,
            "environmentId": m.environment_id,
            "mappingVersion": m.mapping_version,
            "mappingState": m.mapping_state,
            "serviceBindings": services,
        }),
    )
    .await?;
    txn.commit().await?;
    Ok(Outcome::Ready)
}

#[derive(Debug, FromQueryResult)]
struct ServiceName {
    service: String,
}

async fn settle_binding_error(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    e: &PortError,
) -> Result<Outcome, DbErr> {
    match e {
        PortError::Unavailable(_) if w.attempts < MAX_ATTEMPTS => {
            settle_retry(db, w, lease, &e.binding_class()).await
        }
        _ => settle_failed(db, w, lease, &e.binding_class(), None).await,
    }
}

/// Waits without counting the attempt and without losing the checkpointed step.
async fn settle_wait_keep_step(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    seconds: i64,
) -> Result<Outcome, DbErr> {
    let txn = db.begin().await?;
    let Some((_org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let now = db::now(&txn).await?;
    update_workflow(
        &txn,
        current.workflow_id,
        "lease_id = NULL, lease_until = NULL, attempts = GREATEST(attempts - 1, 0), next_attempt_at = $2 + make_interval(secs => $3), updated_at = $2",
        vec![now.into(), (seconds as f64).into()],
    )
    .await?;
    txn.commit().await?;
    Ok(Outcome::Waiting)
}

async fn settle_error(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    e: &PortError,
) -> Result<Outcome, DbErr> {
    match e {
        PortError::Unavailable(_) if w.attempts < MAX_ATTEMPTS => {
            settle_retry(db, w, lease, &e.class()).await
        }
        _ => settle_failed(db, w, lease, &e.class(), None).await,
    }
}

/// Backs off (capped exponential) and keeps the workflow IN_PROGRESS.
async fn settle_retry(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    class: &str,
) -> Result<Outcome, DbErr> {
    let txn = db.begin().await?;
    let Some((_org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let now = db::now(&txn).await?;
    update_workflow(
        &txn,
        current.workflow_id,
        "lease_id = NULL, lease_until = NULL, last_error_class = $2, next_attempt_at = $3 + make_interval(secs => $4), updated_at = $3",
        vec![
            class.into(),
            now.into(),
            (backoff_seconds(current.attempts) as f64).into(),
        ],
    )
    .await?;
    txn.commit().await?;
    Ok(Outcome::Retry)
}

/// The organization is suspended before anything was created: look again later (the
/// attempt is not counted).
async fn settle_wait(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
) -> Result<Outcome, DbErr> {
    let txn = db.begin().await?;
    let Some((_org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let now = db::now(&txn).await?;
    update_workflow(
        &txn,
        current.workflow_id,
        "lease_id = NULL, lease_until = NULL, attempts = GREATEST(attempts - 1, 0), step = 'WAITING_FOR_ACTIVE_ORGANIZATION', next_attempt_at = $2 + make_interval(secs => $3), updated_at = $2",
        vec![now.into(), (SUSPENDED_WAIT_SECONDS as f64).into()],
    )
    .await?;
    txn.commit().await?;
    Ok(Outcome::Waiting)
}

async fn settle_cancelled(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
) -> Result<Outcome, DbErr> {
    let txn = db.begin().await?;
    let Some((_org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let now = db::now(&txn).await?;
    let after = update_workflow(
        &txn,
        current.workflow_id,
        "state = 'CANCELLED', lease_id = NULL, lease_until = NULL, next_attempt_at = NULL, version = version + 1, updated_at = $2",
        vec![now.into()],
    )
    .await?;
    store::emit(
        &txn,
        &system(),
        now,
        provisioning_event(
            &after,
            "organization.provisioning_changed",
            json!({ "workflowState": "CANCELLED", "step": after.step, "errorClass": "ORGANIZATION_ARCHIVED" }),
        ),
    )
    .await?;
    txn.commit().await?;
    Ok(Outcome::Cancelled)
}

/// FAILED with an error class: the organization FAILED, any recorded tenant marked for
/// suspension (compensation, applied by [`sync_tenant_status`] with retries).
async fn settle_failed(
    db: &DatabaseConnection,
    w: &WorkflowRow,
    lease: Uuid,
    class: &str,
    created: Option<&ScopeFacts>,
) -> Result<Outcome, DbErr> {
    let txn = db.begin().await?;
    let Some((_org, current)) = locked(&txn, w, lease).await? else {
        return Ok(Outcome::LeaseLost);
    };
    let now = db::now(&txn).await?;
    let ctx = system();
    let after = update_workflow(
        &txn,
        current.workflow_id,
        "state = 'FAILED', lease_id = NULL, lease_until = NULL, next_attempt_at = NULL, last_error_class = $2, version = version + 1, updated_at = $3",
        vec![class.into(), now.into()],
    )
    .await?;
    set_org_state(&txn, w.organization_id, "FAILED", now).await?;
    match mapping(&txn, w.organization_id, &w.capability, true).await? {
        Some(m) if m.mapping_state != "READY" && m.tenant_id.is_some() => {
            let failed = update_mapping(
                &txn,
                m.id,
                "mapping_state = 'FAILED', mapping_version = mapping_version + 1, tenant_status_desired = 'SUSPENDED', sync_attempts = 0, next_sync_at = $2, updated_at = $2",
                vec![now.into()],
            )
            .await?;
            store::emit(&txn, &ctx, now, failed.event()).await?;
        }
        Some(_) => {}
        None => {
            // Platform Access created a triple we refuse: record it (FAILED) only so the
            // compensation can suspend it, unless that tenant is mapped elsewhere.
            if let Some(f) = created {
                let foreign = db::scalar_i64(
                    &txn,
                    "SELECT count(*)::bigint AS n FROM organization_platform_scopes WHERE tenant_id = $1 AND mapping_state <> 'RETIRED'",
                    vec![f.tenant_id.into()],
                )
                .await?;
                if foreign == 0 {
                    let row: MappingRow = db::one(
                        &txn,
                        &format!(
                            "INSERT INTO organization_platform_scopes (id, organization_id, capability, tenant_id, application_id, environment_id, mapping_state, mapping_version, tenant_status_desired, tenant_status_applied, next_sync_at, created_at, updated_at) \
                             VALUES ($1, $2, $3, $4, $5, $6, 'FAILED', 1, 'SUSPENDED', $7, $8, $8, $8) RETURNING {MAPPING_COLUMNS}"
                        ),
                        vec![
                            Uuid::new_v4().into(),
                            w.organization_id.into(),
                            w.capability.clone().into(),
                            f.tenant_id.into(),
                            f.application_id.into(),
                            f.environment_id.into(),
                            f.tenant_status.clone().into(),
                            now.into(),
                        ],
                    )
                    .await?
                    .ok_or_else(|| DbErr::Custom("mapping insert returned nothing".into()))?;
                    store::emit(&txn, &ctx, now, row.event()).await?;
                }
            }
        }
    }
    store::emit(
        &txn,
        &ctx,
        now,
        provisioning_event(
            &after,
            "organization.provisioning_changed",
            json!({ "provisioningState": "FAILED", "workflowState": "FAILED", "step": after.step, "errorClass": class }),
        ),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(w.organization_id),
        "provisioning.failed",
        "organization",
        Some(w.organization_id),
        json!({ "workflowId": after.workflow_id, "errorClass": class, "attempts": after.attempts, "step": after.step }),
    )
    .await?;
    txn.commit().await?;
    tracing::warn!(organization_id = %w.organization_id, error_class = class, "organization provisioning failed");
    Ok(Outcome::Failed)
}

// ---------------------------------------------------------------------------------------
// Tenant status synchronization (suspension / resumption / compensation)
// ---------------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct SyncRow {
    id: Uuid,
    organization_id: Uuid,
    capability: String,
    tenant_status_desired: String,
    sync_attempts: i32,
}

/// Applies every due desired tenant status to Platform Access (`(applied, failed)`).
/// Failures back off and are retried forever: a suspension (or a compensation) is never
/// abandoned, and scope resolution refuses until it is applied.
pub async fn sync_tenant_status(
    db: &DatabaseConnection,
    port: &dyn PlatformScopes,
    limit: i64,
) -> Result<(u64, u64), DbErr> {
    let due: Vec<SyncRow> = db::all(
        db,
        "WITH due AS (SELECT id FROM organization_platform_scopes \
             WHERE tenant_id IS NOT NULL AND tenant_status_desired IS NOT NULL \
               AND tenant_status_desired IS DISTINCT FROM tenant_status_applied \
               AND COALESCE(next_sync_at, now()) <= now() \
             ORDER BY next_sync_at NULLS FIRST LIMIT $1 FOR UPDATE SKIP LOCKED) \
         UPDATE organization_platform_scopes s SET next_sync_at = now() + make_interval(secs => $2) \
         FROM due WHERE s.id = due.id \
         RETURNING s.id, s.organization_id, s.capability, s.tenant_status_desired, s.sync_attempts",
        vec![limit.into(), (LEASE_SECONDS as f64).into()],
    )
    .await?;
    let (mut ok, mut failed) = (0, 0);
    for row in due {
        let suspended = row.tenant_status_desired == "SUSPENDED";
        let outcome = port
            .set_status(row.organization_id, &row.capability, suspended)
            .await;
        let txn = db.begin().await?;
        let now = db::now(&txn).await?;
        match outcome {
            Ok(facts) => {
                let n = db::exec(
                    &txn,
                    "UPDATE organization_platform_scopes SET tenant_status_applied = $2, sync_attempts = 0, last_sync_error_class = NULL, next_sync_at = NULL, updated_at = $4 WHERE id = $1 AND tenant_status_desired = $3",
                    vec![row.id.into(), facts.tenant_status.clone().into(), row.tenant_status_desired.clone().into(), now.into()],
                )
                .await?;
                if n == 1 {
                    store::audit(
                        &txn,
                        &system(),
                        now,
                        Some(row.organization_id),
                        "scope.tenant_status_applied",
                        "scope",
                        Some(row.id),
                        json!({ "capability": row.capability, "tenantStatus": facts.tenant_status }),
                    )
                    .await?;
                }
                ok += 1;
            }
            Err(PortError::Refused { status: 404, .. }) => {
                // Platform Access has no such scope: nothing can act through it.
                db::exec(
                    &txn,
                    "UPDATE organization_platform_scopes SET tenant_status_applied = tenant_status_desired, last_sync_error_class = 'SCOPE_NOT_FOUND', next_sync_at = NULL, updated_at = $2 WHERE id = $1",
                    vec![row.id.into(), now.into()],
                )
                .await?;
                ok += 1;
            }
            Err(e) => {
                let attempts = row.sync_attempts + 1;
                db::exec(
                    &txn,
                    "UPDATE organization_platform_scopes SET sync_attempts = $2, last_sync_error_class = $3, next_sync_at = $4 + make_interval(secs => $5), updated_at = $4 WHERE id = $1",
                    vec![
                        row.id.into(),
                        attempts.into(),
                        e.class().into(),
                        now.into(),
                        (backoff_seconds(attempts) as f64).into(),
                    ],
                )
                .await?;
                failed += 1;
            }
        }
        txn.commit().await?;
    }
    Ok((ok, failed))
}

// ---------------------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------------------

/// Re-verifies READY / SUSPENDED mappings verified longer than
/// [`RECONCILE_AFTER_SECONDS`] ago (`(reconciled, drifted)`). A missing or different
/// triple is drift: the mapping and the organization become FAILED (`SCOPE_DRIFT`).
pub async fn reconcile(
    db: &DatabaseConnection,
    port: &dyn PlatformScopes,
    limit: i64,
) -> Result<(u64, u64), DbErr> {
    let due: Vec<MappingRow> = db::all(
        db,
        &format!(
            "WITH due AS (SELECT id FROM organization_platform_scopes \
                 WHERE mapping_state IN ('READY', 'SUSPENDED') AND verified_at IS NOT NULL \
                   AND verified_at < now() - make_interval(secs => $2) \
                   AND tenant_status_desired IS NOT DISTINCT FROM tenant_status_applied \
                   AND COALESCE(next_sync_at, now()) <= now() \
                 ORDER BY verified_at LIMIT $1 FOR UPDATE SKIP LOCKED) \
             UPDATE organization_platform_scopes s SET next_sync_at = now() + make_interval(secs => $3) \
             FROM due WHERE s.id = due.id RETURNING {}",
            MAPPING_COLUMNS
                .split(", ")
                .map(|c| format!("s.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        vec![
            limit.into(),
            (RECONCILE_AFTER_SECONDS as f64).into(),
            (LEASE_SECONDS as f64).into(),
        ],
    )
    .await?;
    let (mut ok, mut drifted) = (0, 0);
    for m in due {
        let outcome = port.read(m.organization_id, &m.capability).await;
        let txn = db.begin().await?;
        let now = db::now(&txn).await?;
        let matches = |f: &ScopeFacts| {
            f.verifies(m.organization_id, &m.capability)
                && m.tenant_id
                    .zip(m.application_id)
                    .zip(m.environment_id)
                    .is_some_and(|((t, a), e)| f.same_triple(t, a, e))
        };
        match outcome {
            Ok(Some(f)) if matches(&f) => {
                // A status changed behind our back is re-applied by the sync step.
                db::exec(
                    &txn,
                    "UPDATE organization_platform_scopes SET verified_at = $2, verification_evidence = $3, tenant_status_applied = $4, next_sync_at = NULL, updated_at = $2 WHERE id = $1",
                    vec![m.id.into(), now.into(), f.evidence().into(), f.tenant_status.clone().into()],
                )
                .await?;
                ok += 1;
            }
            Ok(_) => {
                store::org(&txn, m.organization_id, true).await?;
                let ctx = system();
                let failed = update_mapping(
                    &txn,
                    m.id,
                    "mapping_state = 'FAILED', mapping_version = mapping_version + 1, tenant_status_desired = 'SUSPENDED', next_sync_at = $2, updated_at = $2",
                    vec![now.into()],
                )
                .await?;
                store::emit(&txn, &ctx, now, failed.event()).await?;
                set_org_state(&txn, m.organization_id, "FAILED", now).await?;
                if let Some(w) = workflow(&txn, m.organization_id).await? {
                    let after = update_workflow(
                        &txn,
                        w.workflow_id,
                        "state = 'FAILED', lease_id = NULL, lease_until = NULL, last_error_class = 'SCOPE_DRIFT', version = version + 1, updated_at = $2",
                        vec![now.into()],
                    )
                    .await?;
                    store::emit(
                        &txn,
                        &ctx,
                        now,
                        provisioning_event(
                            &after,
                            "organization.provisioning_changed",
                            json!({ "provisioningState": "FAILED", "workflowState": "FAILED", "step": after.step, "errorClass": "SCOPE_DRIFT" }),
                        ),
                    )
                    .await?;
                }
                store::audit(
                    &txn,
                    &ctx,
                    now,
                    Some(m.organization_id),
                    "provisioning.drift_detected",
                    "scope",
                    Some(m.id),
                    json!({ "capability": m.capability, "errorClass": "SCOPE_DRIFT" }),
                )
                .await?;
                drifted += 1;
            }
            Err(_) => {
                db::exec(
                    &txn,
                    "UPDATE organization_platform_scopes SET next_sync_at = $2 + make_interval(secs => $3) WHERE id = $1",
                    vec![m.id.into(), now.into(), (backoff_seconds(4) as f64).into()],
                )
                .await?;
            }
        }
        txn.commit().await?;
    }
    Ok((ok, drifted))
}

// ---------------------------------------------------------------------------------------
// Service-binding reconciliation and revocation (ADR 0058 Amendment A2)
// ---------------------------------------------------------------------------------------

/// Checks the service bindings of READY organizations whose check is due (`(checked,
/// drifted)`): a missing automatic binding is created (backfill), an existing one is
/// re-ensured (Platform Access renews a credential close to expiry) and verified again.
/// A recorded automatic binding Platform Access no longer verifies is drift: it is
/// removed locally at once (fail closed) and the organization FAILS with
/// `SERVICE_BINDING_DRIFT` until an operator retries. Platform Access unavailable (or a
/// backfill refusal) backs off.
pub async fn reconcile_bindings(
    db: &DatabaseConnection,
    port: &dyn PlatformScopes,
    limit: i64,
) -> Result<(u64, u64), DbErr> {
    let due: Vec<MappingRow> = db::all(
        db,
        &format!(
            "WITH due AS (SELECT s.id FROM organization_platform_scopes s JOIN organizations o ON o.id = s.organization_id \
                 WHERE s.mapping_state = 'READY' AND s.verified_at IS NOT NULL AND s.tenant_id IS NOT NULL \
                   AND s.tenant_status_desired = 'ACTIVE' AND s.tenant_status_applied = 'ACTIVE' \
                   AND o.lifecycle_status = 'ACTIVE' AND o.provisioning_state = 'READY' \
                   AND COALESCE(s.bindings_next_check_at, now()) <= now() \
                 ORDER BY s.bindings_next_check_at NULLS FIRST LIMIT $1 FOR UPDATE OF s SKIP LOCKED) \
             UPDATE organization_platform_scopes s SET bindings_next_check_at = now() + make_interval(secs => $2) \
             FROM due WHERE s.id = due.id RETURNING {}",
            MAPPING_COLUMNS
                .split(", ")
                .map(|c| format!("s.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        vec![limit.into(), (LEASE_SECONDS as f64).into()],
    )
    .await?;
    let (mut checked, mut drifted) = (0, 0);
    'mappings: for m in due {
        let Some(triple) = m
            .tenant_id
            .zip(m.application_id)
            .zip(m.environment_id)
            .map(|((t, a), e)| (t, a, e))
        else {
            continue;
        };
        for service in BINDING_SERVICES {
            if !automation_applies(db, m.organization_id, &m.capability, service).await? {
                continue;
            }
            let recorded = db::scalar_i64(
                db,
                "SELECT count(*)::bigint AS n FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3 AND source = 'AUTOMATIC' AND revoked_at IS NULL",
                vec![m.organization_id.into(), m.capability.clone().into(), (*service).into()],
            )
            .await?
                > 0;
            let checked_facts = match port
                .ensure_binding(m.organization_id, &m.capability, service)
                .await
            {
                Ok(put) if put.verifies(m.organization_id, &m.capability, service, triple) => {
                    match port
                        .read_binding(m.organization_id, &m.capability, service)
                        .await
                    {
                        Ok(Some(read))
                            if read.verifies(m.organization_id, &m.capability, service, triple)
                                && read.same_ids(&put) =>
                        {
                            Ok(read)
                        }
                        Ok(_) => Err(PortError::Refused {
                            status: 422,
                            code: "SERVICE_BINDING_VERIFICATION_FAILED".into(),
                        }),
                        Err(e) => Err(e),
                    }
                }
                Ok(_) => Err(PortError::Refused {
                    status: 422,
                    code: "SERVICE_BINDING_VERIFICATION_FAILED".into(),
                }),
                Err(e) => Err(e),
            };
            match checked_facts {
                Ok(facts) => {
                    let txn = db.begin().await?;
                    let now = db::now(&txn).await?;
                    upsert_automatic_binding(
                        &txn,
                        &system(),
                        now,
                        m.organization_id,
                        &m.capability,
                        service,
                        &facts,
                    )
                    .await?;
                    txn.commit().await?;
                }
                Err(e @ PortError::Unavailable(_)) => {
                    binding_check_backoff(db, &m, &e.binding_class()).await?;
                    continue 'mappings;
                }
                Err(e) if !recorded => {
                    // Backfill refused: nothing was ever bound, nothing can drift.
                    let class = match &e {
                        PortError::Refused { status: 422, .. } => {
                            "SERVICE_BINDING_VERIFICATION_FAILED".to_string()
                        }
                        other => other.binding_class(),
                    };
                    binding_check_backoff(db, &m, &class).await?;
                    continue 'mappings;
                }
                Err(_) => {
                    binding_drift(db, &m, service).await?;
                    drifted += 1;
                    continue 'mappings;
                }
            }
        }
        db::exec(
            db,
            "UPDATE organization_platform_scopes SET bindings_next_check_at = now() + make_interval(secs => $2), bindings_check_attempts = 0, bindings_last_error_class = NULL WHERE id = $1",
            vec![m.id.into(), (RECONCILE_AFTER_SECONDS as f64).into()],
        )
        .await?;
        checked += 1;
    }
    Ok((checked, drifted))
}

async fn binding_check_backoff(
    db: &DatabaseConnection,
    m: &MappingRow,
    class: &str,
) -> Result<(), DbErr> {
    db::exec(
        db,
        "UPDATE organization_platform_scopes SET bindings_check_attempts = bindings_check_attempts + 1, bindings_last_error_class = $2, \
           bindings_next_check_at = now() + make_interval(secs => LEAST(900, 5 * power(2, LEAST(bindings_check_attempts, 16))::bigint)) WHERE id = $1",
        vec![m.id.into(), class.into()],
    )
    .await?;
    Ok(())
}

/// A recorded automatic binding no longer verifies: removed locally (scope resolution
/// stops offering it now), the organization and its workflow FAILED
/// (`SERVICE_BINDING_DRIFT`), audited. The tenant is NOT suspended: the scope itself is
/// intact. An operator retry re-provisions the binding.
async fn binding_drift(
    db: &DatabaseConnection,
    m: &MappingRow,
    service: &str,
) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    store::org(&txn, m.organization_id, true).await?;
    let now = db::now(&txn).await?;
    let ctx = system();
    db::exec(
        &txn,
        "DELETE FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3 AND source = 'AUTOMATIC' AND revoked_at IS NULL",
        vec![m.organization_id.into(), m.capability.clone().into(), service.into()],
    )
    .await?;
    set_org_state(&txn, m.organization_id, "FAILED", now).await?;
    if let Some(w) = workflow(&txn, m.organization_id).await? {
        let after = update_workflow(
            &txn,
            w.workflow_id,
            "state = 'FAILED', lease_id = NULL, lease_until = NULL, last_error_class = 'SERVICE_BINDING_DRIFT', version = version + 1, updated_at = $2",
            vec![now.into()],
        )
        .await?;
        store::emit(
            &txn,
            &ctx,
            now,
            provisioning_event(
                &after,
                "organization.provisioning_changed",
                json!({ "provisioningState": "FAILED", "workflowState": "FAILED", "step": after.step, "errorClass": "SERVICE_BINDING_DRIFT" }),
            ),
        )
        .await?;
    }
    db::exec(
        &txn,
        "UPDATE organization_platform_scopes SET bindings_last_error_class = 'SERVICE_BINDING_DRIFT', bindings_next_check_at = NULL WHERE id = $1",
        vec![m.id.into()],
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(m.organization_id),
        "provisioning.drift_detected",
        "scope",
        Some(m.id),
        json!({ "capability": m.capability, "service": service, "errorClass": "SERVICE_BINDING_DRIFT" }),
    )
    .await?;
    txn.commit().await?;
    tracing::warn!(organization_id = %m.organization_id, service, "organization service binding drift");
    Ok(())
}

/// Marks the organization's AUTOMATIC bindings (all services, or one) revoked in the
/// caller's transaction: scope resolution stops returning them immediately and
/// [`sync_binding_revocations`] revokes them at Platform Access. Returns the services.
pub async fn request_binding_revocation(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    organization: Uuid,
    service: Option<&str>,
) -> Result<Vec<String>, DbErr> {
    #[derive(FromQueryResult)]
    struct Revoked {
        service: String,
        service_account_id: Uuid,
        credential_id: Uuid,
        version: i64,
    }
    let rows: Vec<Revoked> = db::all(
        txn,
        "UPDATE organization_scope_service_bindings SET revoked_at = $2, revocation_next_at = $2, revocation_attempts = 0, revocation_last_error_class = NULL, version = version + 1, updated_at = $2 \
         WHERE organization_id = $1 AND source = 'AUTOMATIC' AND revoked_at IS NULL AND ($3::text IS NULL OR service = $3) \
         RETURNING service, service_account_id, credential_id, version",
        vec![organization.into(), now.into(), service.map(str::to_string).into()],
    )
    .await?;
    for r in &rows {
        store::audit(
            txn,
            ctx,
            now,
            Some(organization),
            "scope.service_binding_revoked",
            "organization",
            Some(organization),
            json!({
                "capability": CAPABILITY,
                "service": r.service,
                "source": "AUTOMATIC",
                "serviceAccountId": r.service_account_id,
                "credentialId": r.credential_id,
                "version": r.version,
                "platformAccess": "PENDING",
            }),
        )
        .await?;
    }
    Ok(rows.into_iter().map(|r| r.service).collect())
}

#[derive(Debug, FromQueryResult)]
struct RevocationRow {
    organization_id: Uuid,
    capability: String,
    service: String,
    service_account_id: Uuid,
    credential_id: Uuid,
    revocation_attempts: i32,
}

/// Applies every due binding revocation to Platform Access (`(applied, failed)`);
/// failures back off and are retried forever.
pub async fn sync_binding_revocations(
    db: &DatabaseConnection,
    port: &dyn PlatformScopes,
    limit: i64,
) -> Result<(u64, u64), DbErr> {
    let due: Vec<RevocationRow> = db::all(
        db,
        "WITH due AS (SELECT organization_id, capability, service FROM organization_scope_service_bindings \
             WHERE revoked_at IS NOT NULL AND revocation_applied_at IS NULL AND COALESCE(revocation_next_at, now()) <= now() \
             ORDER BY revocation_next_at NULLS FIRST LIMIT $1 FOR UPDATE SKIP LOCKED) \
         UPDATE organization_scope_service_bindings b SET revocation_next_at = now() + make_interval(secs => $2) \
         FROM due WHERE b.organization_id = due.organization_id AND b.capability = due.capability AND b.service = due.service \
         RETURNING b.organization_id, b.capability, b.service, b.service_account_id, b.credential_id, b.revocation_attempts",
        vec![limit.into(), (LEASE_SECONDS as f64).into()],
    )
    .await?;
    let (mut ok, mut failed) = (0, 0);
    for row in due {
        let outcome = match port
            .revoke_binding(row.organization_id, &row.capability, &row.service)
            .await
        {
            Ok(Some(f)) if f.active || f.authenticates => {
                Err("SERVICE_BINDING_NOT_REVOKED".to_string())
            }
            Ok(_) => Ok(()),
            Err(e) => Err(e.binding_class()),
        };
        let txn = db.begin().await?;
        let now = db::now(&txn).await?;
        match outcome {
            Ok(()) => {
                db::exec(
                    &txn,
                    "UPDATE organization_scope_service_bindings SET revocation_applied_at = $4, revocation_next_at = NULL, revocation_last_error_class = NULL, updated_at = $4 \
                     WHERE organization_id = $1 AND capability = $2 AND service = $3",
                    vec![row.organization_id.into(), row.capability.clone().into(), row.service.clone().into(), now.into()],
                )
                .await?;
                store::audit(
                    &txn,
                    &system(),
                    now,
                    Some(row.organization_id),
                    "scope.service_binding_revocation_applied",
                    "organization",
                    Some(row.organization_id),
                    json!({
                        "capability": row.capability,
                        "service": row.service,
                        "serviceAccountId": row.service_account_id,
                        "credentialId": row.credential_id,
                    }),
                )
                .await?;
                ok += 1;
            }
            Err(class) => {
                let attempts = row.revocation_attempts + 1;
                db::exec(
                    &txn,
                    "UPDATE organization_scope_service_bindings SET revocation_attempts = $4, revocation_last_error_class = $5, revocation_next_at = $6 + make_interval(secs => $7) \
                     WHERE organization_id = $1 AND capability = $2 AND service = $3",
                    vec![
                        row.organization_id.into(),
                        row.capability.clone().into(),
                        row.service.clone().into(),
                        attempts.into(),
                        class.into(),
                        now.into(),
                        (backoff_seconds(attempts) as f64).into(),
                    ],
                )
                .await?;
                failed += 1;
            }
        }
        txn.commit().await?;
    }
    Ok((ok, failed))
}

// ---------------------------------------------------------------------------------------
// Lifecycle, operator retry and reads (called from the HTTP layer)
// ---------------------------------------------------------------------------------------

/// Applies an organization lifecycle change to its scope mappings and workflows in the
/// change's own transaction (the organization row is already locked): suspension /
/// archival marks the tenant for suspension and the mapping SUSPENDED (fail closed now);
/// reactivation restores verified mappings; archival cancels open workflows.
pub async fn on_lifecycle(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    organization: Uuid,
    status: LifecycleStatus,
) -> Result<(), DbErr> {
    let rows: Vec<MappingRow> = db::all(
        txn,
        &format!("SELECT {MAPPING_COLUMNS} FROM organization_platform_scopes WHERE organization_id = $1 AND tenant_id IS NOT NULL AND mapping_state <> 'RETIRED' FOR UPDATE"),
        vec![organization.into()],
    )
    .await?;
    for m in rows {
        let verified = m.verified_at.is_some();
        let (state, desired) = match status {
            LifecycleStatus::Active if verified && m.mapping_state == "SUSPENDED" => {
                ("READY", Some("ACTIVE"))
            }
            LifecycleStatus::Active => (m.mapping_state.as_str(), None),
            _ if m.mapping_state == "READY" => ("SUSPENDED", Some("SUSPENDED")),
            _ => (m.mapping_state.as_str(), Some("SUSPENDED")),
        };
        let Some(desired) = desired else { continue };
        let state_changed = state != m.mapping_state;
        let after = update_mapping(
            txn,
            m.id,
            "mapping_state = $2, mapping_version = mapping_version + $3, tenant_status_desired = $4, sync_attempts = 0, next_sync_at = $5, updated_at = $5",
            vec![
                state.to_string().into(),
                (state_changed as i64).into(),
                desired.into(),
                now.into(),
            ],
        )
        .await?;
        if state_changed {
            store::emit(txn, ctx, now, after.event()).await?;
        }
    }
    match status {
        LifecycleStatus::Archived => {
            // ADR 0058 A2: automatic bindings stop resolving now; Platform Access revokes
            // their credentials through the revocation sync (retried until applied).
            request_binding_revocation(txn, ctx, now, organization, None).await?;
            let cancelled: Vec<WorkflowRow> = db::all(
                txn,
                &format!("UPDATE organization_provisioning SET state = 'CANCELLED', lease_id = NULL, lease_until = NULL, next_attempt_at = NULL, version = version + 1, updated_at = $2 WHERE organization_id = $1 AND state IN ('PENDING', 'IN_PROGRESS') RETURNING {WORKFLOW_COLUMNS}"),
                vec![organization.into(), now.into()],
            )
            .await?;
            for w in &cancelled {
                store::emit(
                    txn,
                    ctx,
                    now,
                    provisioning_event(
                        w,
                        "organization.provisioning_changed",
                        json!({ "workflowState": "CANCELLED", "step": w.step, "errorClass": "ORGANIZATION_ARCHIVED" }),
                    ),
                )
                .await?;
            }
        }
        LifecycleStatus::Active => {
            // A workflow waiting for the organization is due again now.
            db::exec(
                txn,
                "UPDATE organization_provisioning SET next_attempt_at = $2 WHERE organization_id = $1 AND state IN ('PENDING', 'IN_PROGRESS') AND lease_id IS NULL",
                vec![organization.into(), now.into()],
            )
            .await?;
        }
        LifecycleStatus::Suspended => {}
    }
    Ok(())
}

/// Operator retry: a FAILED / CANCELLED workflow becomes PENDING (attempts reset, due
/// now) and the organization PENDING; a failed mapping keeps its ids (the retry must
/// confirm the same triple) and stops forcing suspension. READY and in-flight workflows
/// are left as they are (an in-flight one is made due now). `true` when it changed.
pub async fn request_retry(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    organization: Uuid,
) -> Result<bool, DbErr> {
    let Some(w) = workflow(txn, organization).await? else {
        // Nothing enqueued yet: the worker enqueues PENDING organizations itself; a FAILED
        // one without a workflow (not produced by this service) becomes PENDING.
        return set_org_state(txn, organization, "PENDING", now).await;
    };
    match w.state.as_str() {
        "FAILED" | "CANCELLED" => {}
        "PENDING" | "IN_PROGRESS" => {
            db::exec(
                txn,
                "UPDATE organization_provisioning SET next_attempt_at = $2 WHERE workflow_id = $1 AND lease_id IS NULL",
                vec![w.workflow_id.into(), now.into()],
            )
            .await?;
            return Ok(false);
        }
        _ => return Ok(false),
    }
    let after = update_workflow(
        txn,
        w.workflow_id,
        "state = 'PENDING', step = 'RETRY_REQUESTED', attempts = 0, last_error_class = NULL, next_attempt_at = $2, lease_id = NULL, lease_until = NULL, version = version + 1, updated_at = $2",
        vec![now.into()],
    )
    .await?;
    if let Some(m) = mapping(txn, organization, &w.capability, true).await? {
        if m.mapping_state == "FAILED" {
            let row = update_mapping(
                txn,
                m.id,
                "mapping_state = 'PENDING', mapping_version = mapping_version + 1, tenant_status_desired = NULL, sync_attempts = 0, next_sync_at = NULL, updated_at = $2",
                vec![now.into()],
            )
            .await?;
            store::emit(txn, ctx, now, row.event()).await?;
        }
    }
    set_org_state(txn, organization, "PENDING", now).await?;
    store::emit(
        txn,
        ctx,
        now,
        provisioning_event(
            &after,
            "organization.provisioning_changed",
            json!({ "provisioningState": "PENDING", "workflowState": "PENDING", "step": after.step }),
        ),
    )
    .await?;
    Ok(true)
}

/// The truthful provisioning document (`Provisioning` of the internal contract).
pub async fn provisioning_json<C: ConnectionTrait>(
    c: &C,
    org: &store::OrgRow,
    enabled: bool,
) -> Result<Value, DbErr> {
    let w = workflow(c, org.id).await?;
    let m = mapping(c, org.id, CAPABILITY, false).await?;
    Ok(json!({
        "organizationId": org.id,
        "provisioningState": org.provisioning_state,
        "provisioningEnabled": enabled,
        "workflowId": w.as_ref().map(|w| w.workflow_id),
        "workflowState": w.as_ref().map(|w| w.state.clone()),
        "step": w.as_ref().and_then(|w| w.step.clone()),
        "attempts": w.as_ref().map(|w| w.attempts).unwrap_or(0),
        "lastErrorClass": w.as_ref().and_then(|w| w.last_error_class.clone()),
        "nextAttemptAt": w.as_ref().and_then(|w| w.next_attempt_at).map(store::rfc3339),
        "updatedAt": store::rfc3339(w.as_ref().map(|w| w.updated_at).unwrap_or(org.updated_at)),
        "scopes": m.iter().map(MappingRow::to_json).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_capped_exponential() {
        assert_eq!(backoff_seconds(0), 5);
        assert_eq!(backoff_seconds(1), 5);
        assert_eq!(backoff_seconds(2), 10);
        assert_eq!(backoff_seconds(4), 40);
        assert_eq!(backoff_seconds(8), 640);
        assert_eq!(backoff_seconds(9), 900);
        assert_eq!(backoff_seconds(100), 900);
    }

    #[test]
    fn idempotency_keys_are_stable_and_bounded() {
        let org = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        assert_eq!(
            idempotency_key(org, CAPABILITY),
            "org-scope:00000000-0000-4000-8000-000000000001:primary"
        );
        assert!(idempotency_key(org, CAPABILITY).len() <= 128);
    }

    fn facts_json(org: Uuid, classification: &str, kind: &str) -> Value {
        let (t, a, e) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        json!({
            "organizationRef": org, "capability": "primary",
            "tenantId": t, "applicationId": a, "environmentId": e,
            "tenant": { "id": t, "slug": "x", "classification": classification, "status": "ACTIVE", "revision": 1 },
            "application": { "id": a, "tenantId": t, "slug": "organization", "status": "ACTIVE", "revision": 1 },
            "environment": { "id": e, "tenantId": t, "applicationId": a, "kind": kind, "status": "ACTIVE", "revision": 1 },
            "verified": true, "active": true, "requestedBy": "cfmeet.organizations", "createdAt": "2026-11-07T00:00:00Z",
        })
    }

    #[test]
    fn facts_are_verified_server_side() {
        let org = Uuid::new_v4();
        let ok = ScopeFacts::from_json(&facts_json(org, "FIRST_PARTY", "PRODUCTION")).unwrap();
        assert!(ok.verifies(org, CAPABILITY));
        assert!(
            !ok.verifies(Uuid::new_v4(), CAPABILITY),
            "another organization"
        );
        assert!(!ok.verifies(org, "other"));
        let external = ScopeFacts::from_json(&facts_json(org, "EXTERNAL", "PRODUCTION")).unwrap();
        assert!(!external.verifies(org, CAPABILITY));
        let sandbox = ScopeFacts::from_json(&facts_json(org, "FIRST_PARTY", "SANDBOX")).unwrap();
        assert!(!sandbox.verifies(org, CAPABILITY));
        // Inconsistent nested ids are not facts.
        let mut bad = facts_json(org, "FIRST_PARTY", "PRODUCTION");
        bad["tenant"]["id"] = json!(Uuid::new_v4());
        assert!(ScopeFacts::from_json(&bad).is_none());
        assert!(ScopeFacts::from_json(&json!({})).is_none());
    }

    #[test]
    fn port_errors_classify_retryable_and_final() {
        assert_eq!(
            PortError::Unavailable("x".into()).class(),
            "PLATFORM_ACCESS_UNAVAILABLE"
        );
        assert_eq!(
            PortError::Refused {
                status: 409,
                code: "CONFLICT".into()
            }
            .class(),
            "PLATFORM_SCOPE_CONFLICT"
        );
        assert_eq!(
            PortError::Refused {
                status: 403,
                code: "INTERNAL_PERMISSION_DENIED".into()
            }
            .class(),
            "PLATFORM_ACCESS_REFUSED"
        );
    }
}

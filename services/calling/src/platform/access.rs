//! Calling's client of the shared identity authority, Platform Access (ADR 0032).
//!
//! Platform Access owns tenants, applications, environments, service accounts, API
//! credentials, scopes and principals and is the only service that verifies a customer
//! credential. Calling reaches it ONLY through its authenticated internal contract
//! (`cfmeet.platform-access.internal-api`, service tokens from `packages/service-auth`) —
//! never its database. The wire types below are Calling's own reading of that contract
//! (unknown fields are tolerated so compatible contract additions never break Calling).
//!
//! Every failure to reach the authority is [`AuthorityError::Unavailable`]: customer
//! requests then fail closed (503), never open.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::domain::{
    ApplicationStatus, CredentialStatus, EnvironmentKind, EnvironmentStatus, PrincipalKind,
    PrincipalStatus, ServiceAccountStatus, TenantClassification, TenantStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantSnapshot {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub classification: TenantClassification,
    pub status: TenantStatus,
    pub billing_currency: String,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub status: ApplicationStatus,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub kind: EnvironmentKind,
    pub status: EnvironmentStatus,
    pub live_telecom_allowed: bool,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceAccountSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: ServiceAccountStatus,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A credential without any verifier material (the contract has no such field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSnapshot {
    pub id: Uuid,
    pub public_id: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub label: Option<String>,
    pub last_four: String,
    pub status: CredentialStatus,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub rotated_from_credential_id: Option<Uuid>,
    pub superseded_by_credential_id: Option<Uuid>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub supersession_valid_until: Option<DateTime<Utc>>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalSnapshot {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: PrincipalKind,
    pub external_reference: String,
    pub linked_service_account_id: Option<Uuid>,
    pub status: PrincipalStatus,
    pub tombstoned_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationScopeSnapshot {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub scope: String,
    pub granted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialScopeSnapshot {
    pub credential_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub scope: String,
    pub granted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WireAuthorityMode {
    Full,
    SafetyOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WireSuspendedLink {
    Tenant,
    Application,
    Environment,
}

/// The trusted authority of an authenticated credential.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityChain {
    pub authority_mode: WireAuthorityMode,
    pub suspended_link: Option<WireSuspendedLink>,
    pub tenant: TenantSnapshot,
    pub application: ApplicationSnapshot,
    pub environment: EnvironmentSnapshot,
    pub service_account: ServiceAccountSnapshot,
    pub credential: CredentialSnapshot,
    pub scopes: Vec<String>,
    pub principal_restriction: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Throttle {
    pub retry_after_seconds: u64,
    pub threshold_crossed: bool,
}

/// Result of presenting a credential to the authority.
#[derive(Debug, Clone)]
pub enum AuthenticationOutcome {
    Authenticated(Box<AuthorityChain>),
    Rejected {
        /// Bounded reason code for operator logs (e.g. `secret_mismatch`).
        reason: String,
        public_id: Option<String>,
        throttle: Option<Throttle>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "outcome")]
enum AuthenticateWire {
    #[serde(rename = "AUTHENTICATED")]
    Authenticated { authority: Box<AuthorityChain> },
    #[serde(rename = "REJECTED")]
    Rejected {
        reason: String,
        #[serde(default, rename = "publicId")]
        public_id: Option<String>,
        #[serde(default)]
        throttle: Option<Throttle>,
    },
}

/// One change of the identity change feed.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityChange {
    pub sequence: i64,
    pub entity_kind: String,
    pub operation: String,
    pub entity_key: String,
    pub revision: Option<i64>,
    pub occurred_at: DateTime<Utc>,
    pub snapshot: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePage {
    pub changes: Vec<IdentityChange>,
    pub next_after: i64,
    pub has_more: bool,
}

/// Platform Access's receipt of an imported identity bundle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReceipt {
    pub id: Uuid,
    pub source_service: String,
    pub bundle_digest: String,
    pub verifier_digest: String,
    pub counts: BTreeMap<String, u64>,
    pub imported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TombstoneOutcome {
    pub already_tombstoned: bool,
    pub principal: PrincipalSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    /// The authority could not be reached or answered with a server error: fail closed.
    #[error("platform access unavailable ({0})")]
    Unavailable(String),
    /// The authority refused the request (bounded error code).
    #[error("platform access refused the request ({0})")]
    Refused(String),
    /// Not found in the caller's scope.
    #[error("not found")]
    NotFound,
}

impl From<InternalCallError> for AuthorityError {
    fn from(err: InternalCallError) -> Self {
        match err {
            InternalCallError::Status { status: 404, .. } => AuthorityError::NotFound,
            InternalCallError::Status {
                status,
                code: Some(code),
                ..
            } if (400..500).contains(&status) => AuthorityError::Refused(code),
            other => AuthorityError::Unavailable(other.to_string()),
        }
    }
}

/// Calling's view of the identity authority (a trait so the composition root chooses
/// the transport; tests run the real Platform Access binary behind it).
#[async_trait]
pub trait IdentityAuthority: Send + Sync {
    /// Verifies a presented customer credential.
    async fn authenticate(
        &self,
        credential: &str,
        correlation: &CorrelationId,
    ) -> Result<AuthenticationOutcome, AuthorityError>;

    /// Registers (idempotently) the TELEPHONE_ENDPOINT principal of a protected public
    /// party in the signed delegated scope; the reference is the opaque ContactReference.
    async fn ensure_telephone_endpoint(
        &self,
        scope: &DelegatedPlatformContext,
        contact_reference: Uuid,
    ) -> Result<PrincipalSnapshot, AuthorityError>;

    /// Tombstones a principal on a named operator's retention decision.
    async fn tombstone_principal(
        &self,
        principal_id: Uuid,
        operator: &str,
    ) -> Result<TombstoneOutcome, AuthorityError>;

    /// A page of the identity change feed after `after`.
    async fn changes(&self, after: i64, limit: u64) -> Result<ChangePage, AuthorityError>;

    /// The receipt of an imported identity bundle, if any.
    async fn import_receipt(
        &self,
        bundle_digest: &str,
    ) -> Result<Option<ImportReceipt>, AuthorityError>;
}

/// The authenticated HTTP transport (`CF_PLATFORM_ACCESS_BASE_URL`).
pub struct HttpIdentityAuthority {
    client: Arc<InternalServiceClient>,
}

impl HttpIdentityAuthority {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

fn target() -> ServiceId {
    ServiceId::platform_access()
}

#[async_trait]
impl IdentityAuthority for HttpIdentityAuthority {
    async fn authenticate(
        &self,
        credential: &str,
        correlation: &CorrelationId,
    ) -> Result<AuthenticationOutcome, AuthorityError> {
        /// Borrowed: the presented credential is never copied into a long-lived value
        /// and never logged.
        #[derive(Serialize)]
        struct Body<'a> {
            credential: &'a str,
        }
        let request = OutboundServiceRequest::continuing(target(), correlation);
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/credentials/authenticate",
                &Body { credential },
            )
            .await?;
        match response.json::<AuthenticateWire>()? {
            AuthenticateWire::Authenticated { authority } => {
                Ok(AuthenticationOutcome::Authenticated(authority))
            }
            AuthenticateWire::Rejected {
                reason,
                public_id,
                throttle,
            } => Ok(AuthenticationOutcome::Rejected {
                reason,
                public_id,
                throttle,
            }),
        }
    }

    async fn ensure_telephone_endpoint(
        &self,
        scope: &DelegatedPlatformContext,
        contact_reference: Uuid,
    ) -> Result<PrincipalSnapshot, AuthorityError> {
        #[derive(Deserialize)]
        struct Wire {
            principal: PrincipalSnapshot,
        }
        let request =
            OutboundServiceRequest::new_operation(target()).with_delegated_context(scope.clone());
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/principals/telephone-endpoints",
                &json!({ "contactReference": contact_reference }),
            )
            .await?;
        Ok(response.json::<Wire>()?.principal)
    }

    async fn tombstone_principal(
        &self,
        principal_id: Uuid,
        operator: &str,
    ) -> Result<TombstoneOutcome, AuthorityError> {
        let request = OutboundServiceRequest::new_operation(target());
        let response = self
            .client
            .post_json(
                &request,
                &format!("/internal/v1/principals/{principal_id}/tombstone"),
                &json!({ "operator": operator }),
            )
            .await?;
        Ok(response.json()?)
    }

    async fn changes(&self, after: i64, limit: u64) -> Result<ChangePage, AuthorityError> {
        let request = OutboundServiceRequest::new_operation(target());
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/identity/changes",
                &json!({ "after": after, "limit": limit }),
            )
            .await?;
        Ok(response.json()?)
    }

    async fn import_receipt(
        &self,
        bundle_digest: &str,
    ) -> Result<Option<ImportReceipt>, AuthorityError> {
        let request = OutboundServiceRequest::new_operation(target());
        match self
            .client
            .get(
                &request,
                &format!("/internal/v1/identity/imports/{bundle_digest}"),
            )
            .await
        {
            Ok(response) => Ok(Some(response.json()?)),
            Err(InternalCallError::Status { status: 404, .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// No authority configured: every identity operation fails closed.
pub struct UnconfiguredIdentityAuthority;

#[async_trait]
impl IdentityAuthority for UnconfiguredIdentityAuthority {
    async fn authenticate(
        &self,
        _: &str,
        _: &CorrelationId,
    ) -> Result<AuthenticationOutcome, AuthorityError> {
        Err(AuthorityError::Unavailable(
            "platform access is not configured".into(),
        ))
    }

    async fn ensure_telephone_endpoint(
        &self,
        _: &DelegatedPlatformContext,
        _: Uuid,
    ) -> Result<PrincipalSnapshot, AuthorityError> {
        Err(AuthorityError::Unavailable(
            "platform access is not configured".into(),
        ))
    }

    async fn tombstone_principal(
        &self,
        _: Uuid,
        _: &str,
    ) -> Result<TombstoneOutcome, AuthorityError> {
        Err(AuthorityError::Unavailable(
            "platform access is not configured".into(),
        ))
    }

    async fn changes(&self, _: i64, _: u64) -> Result<ChangePage, AuthorityError> {
        Err(AuthorityError::Unavailable(
            "platform access is not configured".into(),
        ))
    }

    async fn import_receipt(&self, _: &str) -> Result<Option<ImportReceipt>, AuthorityError> {
        Err(AuthorityError::Unavailable(
            "platform access is not configured".into(),
        ))
    }
}

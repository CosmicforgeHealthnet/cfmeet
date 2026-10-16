//! Calling's port to Numbering (Part IV Workstream L), the authority over Digital
//! Communications Numbers (DCNs). Calling never reads Numbering's database and never
//! sees a number: it handles CF Meet references and signed routing grants only.
//!
//! * `POST /internal/v1/routing/grant-verifications` (`number-grant.verify`): Calling
//!   verifies the INBOUND_VOICE grant Telephony relayed with an inbound DCN call against
//!   Numbering's CURRENT state (audience Calling, not consumed — inbound grants are not
//!   single-use).
//! * `GET /internal/v1/routing/number-assignments/{id}` (`number-caller-identity.authorize`,
//!   signed delegated context): the delegated scope's current assignment and voice target
//!   of a DCN, so Calling applies its enterprise membership policy BEFORE asking for a
//!   caller-identity grant.
//! * `POST /internal/v1/routing/caller-identity-grants` (`number-caller-identity.authorize`):
//!   a single-use CALLER_IDENTITY grant (audience Telephony) for ONE origination.
//!
//! Without `CF_NUMBERING_BASE_URL` every DCN operation fails closed; application calls,
//! relay-number calls and enterprise calls never depend on Numbering.

use std::fmt;

use async_trait::async_trait;
use cf_service_auth::{
    client::InternalCallError, CorrelationId, DelegatedPlatformContext, OutboundServiceRequest,
    ServiceId,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::internal_auth::ServiceAuthRuntime;

/// Why Numbering could not answer usefully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberingError {
    /// Not reachable / not configured / malformed answer: fail closed.
    Unavailable,
    /// Numbering refused (unknown number, another scope's number, not usable now...).
    Refused(String),
}

/// The customer scope a DCN is assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DcnScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

/// A routing target (kind + CF Meet reference).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcnTarget {
    pub kind: String,
    pub target_ref: Uuid,
}

/// An INBOUND_VOICE grant Numbering verified against current state.
#[derive(Debug, Clone)]
pub struct VerifiedInboundGrant {
    pub grant_id: Uuid,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub scope: DcnScope,
    pub target: DcnTarget,
}

/// The delegated scope's current assignment of a DCN (no number).
#[derive(Debug, Clone)]
pub struct DcnAssignmentView {
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub ownership_class: String,
    pub owner_kind: String,
    pub owner_ref: Uuid,
    pub voice: Option<DcnTarget>,
    pub routing_revision: i64,
    pub number_active: bool,
    pub voice_outbound: bool,
}

/// A single-use CALLER_IDENTITY grant for Telephony (a bearer token: never logged).
pub struct CallerIdentityGrant {
    pub grant: String,
    pub grant_id: Uuid,
}

impl fmt::Debug for CallerIdentityGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallerIdentityGrant")
            .field("grant_id", &self.grant_id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait NumberingPort: Send + Sync {
    /// Verifies an inbound voice grant addressed to Calling (current state, not consumed).
    async fn verify_inbound_voice(
        &self,
        grant: &str,
        correlation_id: &CorrelationId,
    ) -> Result<VerifiedInboundGrant, NumberingError>;

    /// The delegated scope's current assignment of `number_resource_id`.
    async fn current_assignment(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        number_resource_id: Uuid,
    ) -> Result<DcnAssignmentView, NumberingError>;

    /// A single-use CALLER_IDENTITY grant for `operation_ref` (the delegated context names
    /// the initiating principal).
    async fn presentation_grant(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        number_resource_id: Uuid,
        operation_ref: &str,
    ) -> Result<CallerIdentityGrant, NumberingError>;
}

/// Not configured: every DCN operation fails closed.
pub struct UnconfiguredNumbering;

#[async_trait]
impl NumberingPort for UnconfiguredNumbering {
    async fn verify_inbound_voice(
        &self,
        _: &str,
        _: &CorrelationId,
    ) -> Result<VerifiedInboundGrant, NumberingError> {
        Err(NumberingError::Unavailable)
    }

    async fn current_assignment(
        &self,
        _: &DelegatedPlatformContext,
        _: &CorrelationId,
        _: Uuid,
    ) -> Result<DcnAssignmentView, NumberingError> {
        Err(NumberingError::Unavailable)
    }

    async fn presentation_grant(
        &self,
        _: &DelegatedPlatformContext,
        _: &CorrelationId,
        _: Uuid,
        _: &str,
    ) -> Result<CallerIdentityGrant, NumberingError> {
        Err(NumberingError::Unavailable)
    }
}

/// The authenticated HTTP adapter (service token audience `cfmeet.numbering`).
#[derive(Clone)]
pub struct HttpNumbering {
    service_auth: ServiceAuthRuntime,
}

impl fmt::Debug for HttpNumbering {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpNumbering").finish_non_exhaustive()
    }
}

impl HttpNumbering {
    pub fn new(service_auth: ServiceAuthRuntime) -> Self {
        Self { service_auth }
    }
}

fn failed(error: InternalCallError) -> NumberingError {
    match error {
        InternalCallError::Status {
            status: 400..=499,
            code,
            ..
        } => NumberingError::Refused(code.unwrap_or_else(|| "REFUSED".into())),
        other => {
            // Never log URLs, bodies or grants: the error type carries none.
            tracing::warn!(error = %other, "numbering unavailable; failing closed");
            NumberingError::Unavailable
        }
    }
}

fn uuid_at(v: &Value, k: &str) -> Result<Uuid, NumberingError> {
    v[k].as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(NumberingError::Unavailable)
}

fn target_at(v: &Value) -> Option<DcnTarget> {
    Some(DcnTarget {
        kind: v["kind"].as_str()?.to_string(),
        target_ref: Uuid::parse_str(v["ref"].as_str()?).ok()?,
    })
}

#[async_trait]
impl NumberingPort for HttpNumbering {
    async fn verify_inbound_voice(
        &self,
        grant: &str,
        correlation_id: &CorrelationId,
    ) -> Result<VerifiedInboundGrant, NumberingError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::numbering(), correlation_id);
        let v: Value = self
            .service_auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/routing/grant-verifications",
                &json!({ "grant": grant, "consume": false }),
            )
            .await
            .map_err(failed)?
            .json()
            .map_err(|_| NumberingError::Unavailable)?;
        if v["valid"] != true {
            return Err(NumberingError::Refused(
                v["reason"].as_str().unwrap_or("GRANT_INVALID").to_string(),
            ));
        }
        if v["grantKind"] != "INBOUND_VOICE" {
            return Err(NumberingError::Refused("GRANT_INVALID".into()));
        }
        let scope = &v["scope"];
        Ok(VerifiedInboundGrant {
            grant_id: uuid_at(&v, "grantId")?,
            number_resource_id: uuid_at(&v, "numberResourceId")?,
            assignment_id: uuid_at(&v, "assignmentId")?,
            routing_revision: v["routingRevision"]
                .as_i64()
                .ok_or(NumberingError::Unavailable)?,
            scope: DcnScope {
                tenant_id: uuid_at(scope, "tenantId")?,
                application_id: uuid_at(scope, "applicationId")?,
                environment_id: uuid_at(scope, "environmentId")?,
            },
            target: target_at(&v["target"]).ok_or(NumberingError::Unavailable)?,
        })
    }

    async fn current_assignment(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        number_resource_id: Uuid,
    ) -> Result<DcnAssignmentView, NumberingError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::numbering(), correlation_id)
            .with_delegated_context(delegated.clone());
        let v: Value = self
            .service_auth
            .client()
            .get(
                &outbound,
                &format!("/internal/v1/routing/number-assignments/{number_resource_id}"),
            )
            .await
            .map_err(failed)?
            .json()
            .map_err(|_| NumberingError::Unavailable)?;
        Ok(DcnAssignmentView {
            number_resource_id: uuid_at(&v, "numberResourceId")?,
            assignment_id: uuid_at(&v, "assignmentId")?,
            ownership_class: v["ownershipClass"]
                .as_str()
                .ok_or(NumberingError::Unavailable)?
                .to_string(),
            owner_kind: v["ownerKind"]
                .as_str()
                .ok_or(NumberingError::Unavailable)?
                .to_string(),
            owner_ref: uuid_at(&v, "ownerRef")?,
            voice: target_at(&v["voice"]),
            routing_revision: v["routingRevision"]
                .as_i64()
                .ok_or(NumberingError::Unavailable)?,
            number_active: v["numberActive"] == true,
            voice_outbound: v["outbound"]["voice"] == true,
        })
    }

    async fn presentation_grant(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        number_resource_id: Uuid,
        operation_ref: &str,
    ) -> Result<CallerIdentityGrant, NumberingError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::numbering(), correlation_id)
            .with_delegated_context(delegated.clone());
        let v: Value = self
            .service_auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/routing/caller-identity-grants",
                &json!({ "numberResourceId": number_resource_id, "operationRef": operation_ref }),
            )
            .await
            .map_err(failed)?
            .json()
            .map_err(|_| NumberingError::Unavailable)?;
        let grant = v["grant"].as_str().ok_or(NumberingError::Unavailable)?;
        let well_formed = grant.len() <= 4096
            && grant.split('.').count() == 3
            && grant
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        if !well_formed
            || v["grantKind"] != "CALLER_IDENTITY"
            || uuid_at(&v, "numberResourceId")? != number_resource_id
        {
            tracing::warn!("numbering returned a malformed caller-identity grant; failing closed");
            return Err(NumberingError::Unavailable);
        }
        Ok(CallerIdentityGrant {
            grant: grant.to_string(),
            grant_id: uuid_at(&v, "grantId")?,
        })
    }
}

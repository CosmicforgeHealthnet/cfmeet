//! The SMS Gateway's port to Numbering, the authority over numbers. The gateway never
//! reads Numbering's database and never replicates assignments, KYC or lifecycle: every
//! decision is asked of Numbering at the moment it matters.
//!
//! * `POST /internal/v1/routing/sms-destination-resolutions` (`number-sms-route.resolve`)
//!   — the destination of a received message: owner scope, SMS routing target, routing
//!   revision and an INBOUND_SMS grant (audience the SMS Gateway). Never the voice
//!   resolver.
//! * `POST /internal/v1/routing/sms-sender-grants` (`number-sms-sender.authorize`, signed
//!   delegated context) — a single-use SMS_SENDER grant for ONE message.
//! * `POST /internal/v1/routing/grant-verifications` (`number-grant.verify`) — consumes the
//!   sender grant for exactly that message and releases the sender number; re-verifies an
//!   INBOUND_SMS grant against CURRENT state at the final delivery boundary (a routing
//!   change, reassignment, suspension or release since the resolution refuses it).
//! * `GET /internal/v1/number-resources/{id}/routes` + `POST …/route-readiness`
//!   (`number-resource.read`, `number-route.acknowledge`) — SMS route readiness.
//!
//! Without `CF_NUMBERING_BASE_URL` (or while it is unreachable) every operation fails
//! closed.

use std::fmt;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use serde_json::{json, Value};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberingError {
    Unavailable,
    /// Numbering refused (stable reason / error code).
    Refused(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

/// Numbering's routing of a received message's destination.
#[derive(Clone)]
pub enum SmsDestination {
    Routed {
        number_resource_id: Uuid,
        assignment_id: Uuid,
        routing_revision: i64,
        scope: Scope,
        target_kind: String,
        target_ref: Uuid,
        grant_id: Uuid,
        /// The signed INBOUND_SMS grant (re-verified before the message is stored).
        grant: Zeroizing<String>,
    },
    /// Assigned, but SMS routes nowhere.
    NotRouted { reason: String },
    /// Not routable now (stable reason: unknown, not active, …).
    Unavailable { reason: String },
}

impl fmt::Debug for SmsDestination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmsDestination::Routed {
                number_resource_id, ..
            } => write!(f, "Routed({number_resource_id})"),
            SmsDestination::NotRouted { reason } => write!(f, "NotRouted({reason})"),
            SmsDestination::Unavailable { reason } => write!(f, "Unavailable({reason})"),
        }
    }
}

/// A sender number Numbering released for one message (plaintext in memory only).
pub struct ReleasedSender {
    pub e164: Zeroizing<String>,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub grant_id: Uuid,
    pub scope: Scope,
}

impl fmt::Debug for ReleasedSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReleasedSender")
            .field("number_resource_id", &self.number_resource_id)
            .field("grant_id", &self.grant_id)
            .finish_non_exhaustive()
    }
}

/// An INBOUND_SMS grant Numbering verified against its current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRouting {
    pub grant_id: Uuid,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub scope: Scope,
    pub target: Option<(String, Uuid)>,
}

/// Numbering's current SMS route configuration of a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmsRoute {
    pub route_ref: Uuid,
    pub route_revision: i64,
    pub status: String,
}

#[async_trait]
pub trait NumberingPort: Send + Sync {
    async fn resolve_destination(
        &self,
        destination: &str,
        correlation: &CorrelationId,
    ) -> Result<SmsDestination, NumberingError>;

    /// Re-verifies an INBOUND_SMS grant against Numbering's CURRENT state (not consumed).
    async fn verify_inbound_sms(
        &self,
        grant: &str,
        correlation: &CorrelationId,
    ) -> Result<VerifiedRouting, NumberingError>;

    /// Obtains AND consumes the single-use SMS_SENDER grant of message `operation_ref`.
    async fn release_sender(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
        number_resource_id: Uuid,
        operation_ref: &str,
    ) -> Result<ReleasedSender, NumberingError>;

    async fn sms_route(&self, number_resource_id: Uuid)
        -> Result<Option<SmsRoute>, NumberingError>;

    async fn acknowledge_sms_route(
        &self,
        number_resource_id: Uuid,
        route: &SmsRoute,
    ) -> Result<(), NumberingError>;
}

/// No Numbering configured: everything fails closed.
pub struct UnconfiguredNumbering;

#[async_trait]
impl NumberingPort for UnconfiguredNumbering {
    async fn resolve_destination(
        &self,
        _: &str,
        _: &CorrelationId,
    ) -> Result<SmsDestination, NumberingError> {
        Err(NumberingError::Unavailable)
    }

    async fn verify_inbound_sms(
        &self,
        _: &str,
        _: &CorrelationId,
    ) -> Result<VerifiedRouting, NumberingError> {
        Err(NumberingError::Unavailable)
    }

    async fn release_sender(
        &self,
        _: &DelegatedPlatformContext,
        _: &CorrelationId,
        _: Uuid,
        _: &str,
    ) -> Result<ReleasedSender, NumberingError> {
        Err(NumberingError::Unavailable)
    }

    async fn sms_route(&self, _: Uuid) -> Result<Option<SmsRoute>, NumberingError> {
        Err(NumberingError::Unavailable)
    }

    async fn acknowledge_sms_route(&self, _: Uuid, _: &SmsRoute) -> Result<(), NumberingError> {
        Err(NumberingError::Unavailable)
    }
}

/// Numbering over authenticated internal HTTP.
pub struct HttpNumbering {
    client: std::sync::Arc<InternalServiceClient>,
}

impl HttpNumbering {
    pub fn new(client: std::sync::Arc<InternalServiceClient>) -> Self {
        Self { client }
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

fn scope_at(v: &Value) -> Result<Scope, NumberingError> {
    Ok(Scope {
        tenant_id: uuid_at(v, "tenantId")?,
        application_id: uuid_at(v, "applicationId")?,
        environment_id: uuid_at(v, "environmentId")?,
    })
}

#[async_trait]
impl NumberingPort for HttpNumbering {
    async fn resolve_destination(
        &self,
        destination: &str,
        correlation: &CorrelationId,
    ) -> Result<SmsDestination, NumberingError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::numbering(), correlation);
        let v: Value = self
            .client
            .post_json(
                &outbound,
                "/internal/v1/routing/sms-destination-resolutions",
                &json!({ "destination": destination }),
            )
            .await
            .map_err(failed)?
            .json()
            .map_err(|_| NumberingError::Unavailable)?;
        let reason = || v["reason"].as_str().unwrap_or("UNAVAILABLE").to_string();
        match v["outcome"].as_str() {
            Some("ROUTED") => Ok(SmsDestination::Routed {
                number_resource_id: uuid_at(&v, "numberResourceId")?,
                assignment_id: uuid_at(&v, "assignmentId")?,
                routing_revision: v["routingRevision"]
                    .as_i64()
                    .ok_or(NumberingError::Unavailable)?,
                scope: scope_at(&v["scope"])?,
                target_kind: v["target"]["kind"]
                    .as_str()
                    .ok_or(NumberingError::Unavailable)?
                    .to_string(),
                target_ref: uuid_at(&v["target"], "ref")?,
                grant_id: uuid_at(&v["grant"], "grantId")?,
                grant: Zeroizing::new(
                    v["grant"]["grant"]
                        .as_str()
                        .filter(|g| g.len() <= 4096)
                        .ok_or(NumberingError::Unavailable)?
                        .to_string(),
                ),
            }),
            Some("NOT_ROUTED") => Ok(SmsDestination::NotRouted {
                reason: "NOT_ROUTED".into(),
            }),
            Some("UNAVAILABLE") => Ok(SmsDestination::Unavailable { reason: reason() }),
            _ => Err(NumberingError::Unavailable),
        }
    }

    async fn verify_inbound_sms(
        &self,
        grant: &str,
        correlation: &CorrelationId,
    ) -> Result<VerifiedRouting, NumberingError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::numbering(), correlation);
        let v: Value = self
            .client
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
        if v["grantKind"] != "INBOUND_SMS" {
            return Err(NumberingError::Refused("GRANT_INVALID".into()));
        }
        let target = match &v["target"] {
            Value::Null => None,
            t => Some((
                t["kind"]
                    .as_str()
                    .ok_or(NumberingError::Unavailable)?
                    .to_string(),
                uuid_at(t, "ref")?,
            )),
        };
        Ok(VerifiedRouting {
            grant_id: uuid_at(&v, "grantId")?,
            number_resource_id: uuid_at(&v, "numberResourceId")?,
            assignment_id: uuid_at(&v, "assignmentId")?,
            routing_revision: v["routingRevision"]
                .as_i64()
                .ok_or(NumberingError::Unavailable)?,
            scope: scope_at(&v["scope"])?,
            target,
        })
    }

    async fn release_sender(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
        number_resource_id: Uuid,
        operation_ref: &str,
    ) -> Result<ReleasedSender, NumberingError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::numbering(), correlation)
            .with_delegated_context(delegated.clone());
        let issued: Value = self
            .client
            .post_json(
                &outbound,
                "/internal/v1/routing/sms-sender-grants",
                &json!({ "numberResourceId": number_resource_id, "operationRef": operation_ref }),
            )
            .await
            .map_err(failed)?
            .json()
            .map_err(|_| NumberingError::Unavailable)?;
        let grant = Zeroizing::new(
            issued["grant"]
                .as_str()
                .ok_or(NumberingError::Unavailable)?
                .to_string(),
        );
        if issued["grantKind"] != "SMS_SENDER" || grant.len() > 4096 {
            return Err(NumberingError::Unavailable);
        }
        let outbound = OutboundServiceRequest::continuing(ServiceId::numbering(), correlation);
        let v: Value = self
            .client
            .post_json(
                &outbound,
                "/internal/v1/routing/grant-verifications",
                &json!({ "grant": grant.as_str(), "consume": true, "operationRef": operation_ref }),
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
        if v["grantKind"] != "SMS_SENDER" || v["consumed"] != true {
            return Err(NumberingError::Refused("GRANT_INVALID".into()));
        }
        let e164 = v["e164"]
            .as_str()
            .and_then(crate::sms::e164::normalize)
            .ok_or(NumberingError::Unavailable)?;
        let released = ReleasedSender {
            e164: Zeroizing::new(e164),
            number_resource_id: uuid_at(&v, "numberResourceId")?,
            assignment_id: uuid_at(&v, "assignmentId")?,
            routing_revision: v["routingRevision"]
                .as_i64()
                .ok_or(NumberingError::Unavailable)?,
            grant_id: uuid_at(&v, "grantId")?,
            scope: scope_at(&v["scope"])?,
        };
        if released.number_resource_id != number_resource_id {
            return Err(NumberingError::Refused("GRANT_INVALID".into()));
        }
        Ok(released)
    }

    async fn sms_route(
        &self,
        number_resource_id: Uuid,
    ) -> Result<Option<SmsRoute>, NumberingError> {
        let outbound = OutboundServiceRequest::new_operation(ServiceId::numbering());
        let v: Value = self
            .client
            .get(
                &outbound,
                &format!("/internal/v1/number-resources/{number_resource_id}/routes"),
            )
            .await
            .map_err(failed)?
            .json()
            .map_err(|_| NumberingError::Unavailable)?;
        Ok(v["routes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["family"] == "SMS")
            .and_then(|r| {
                Some(SmsRoute {
                    route_ref: Uuid::parse_str(r["routeRef"].as_str()?).ok()?,
                    route_revision: r["routeRevision"].as_i64()?,
                    status: r["status"].as_str()?.to_string(),
                })
            }))
    }

    async fn acknowledge_sms_route(
        &self,
        number_resource_id: Uuid,
        route: &SmsRoute,
    ) -> Result<(), NumberingError> {
        let outbound = OutboundServiceRequest::new_operation(ServiceId::numbering());
        self.client
            .post_json(
                &outbound,
                &format!("/internal/v1/number-resources/{number_resource_id}/route-readiness"),
                &json!({
                    "family": "SMS",
                    "routeRef": route.route_ref,
                    "routeRevision": route.route_revision,
                }),
            )
            .await
            .map(|_| ())
            .map_err(failed)
    }
}

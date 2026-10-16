//! Telephony's ports to the services it depends on (Part E §4, §165–§166).
//!
//! * [`PrivacyRelayPort`]: protected-contact resolution for ONE authorized execution
//!   (the only place a private destination becomes plaintext, held in memory only),
//!   inbound relay mapping resolution, and inbound caller protection.
//! * [`CallingPort`]: delivery of normalized events and inbound offers to Calling.
//!
//! Both are authenticated internal HTTP (service tokens; receiver-authoritative
//! permissions) in production; tests substitute deterministic fakes. Telephony never
//! touches another service's database.

use std::fmt;

use async_trait::async_trait;
use cf_service_auth::{
    client::InternalCallError, CorrelationId, DelegatedPlatformContext, OutboundServiceRequest,
    ServiceId,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{config::ServiceAuthRuntime, providers::ExecutionNumber};

/// Failure of a port call. `Denied`: the authority refused (its stable code); everything
/// else that is not an answer is `Unavailable` and fails CLOSED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    Denied(String),
    NotFound,
    Unavailable,
}

/// A resolved protected destination for one execution attempt.
pub struct ResolvedExecution {
    pub destination: ExecutionNumber,
    pub country: Option<String>,
    /// The session's authorized relay identity (selected by Privacy Relay, never by a
    /// client): the caller id this leg presents.
    pub caller_identity: Option<ExecutionNumber>,
    /// Privacy Relay's assignment reference of that identity (`callerIdentity.assignmentId`):
    /// identity evidence kept on the leg (Part H §78) — never the number itself.
    pub relay_assignment_ref: Option<Uuid>,
}

impl fmt::Debug for ResolvedExecution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedExecution")
            .field("country", &self.country)
            .finish_non_exhaustive()
    }
}

/// What Telephony asks to resolve.
#[derive(Clone)]
pub struct ResolveForExecution {
    pub delegated: DelegatedPlatformContext,
    pub correlation_id: String,
    pub contact_reference: Uuid,
    pub grant_id: Uuid,
    pub grant_secret: String,
    pub call_id: Uuid,
    pub call_leg_id: Uuid,
    pub execution_attempt_id: Uuid,
}

impl fmt::Debug for ResolveForExecution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolveForExecution")
            .field("contact_reference", &self.contact_reference)
            .field("grant_id", &self.grant_id)
            .finish_non_exhaustive()
    }
}

/// A resolved inbound relay mapping (references only).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundMapping {
    pub decision_id: Uuid,
    pub context_id: Uuid,
    pub context_version: i64,
    pub relay_session_id: Uuid,
    pub relay_session_version: i64,
    pub assignment_id: Uuid,
    pub assignment_generation: i64,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub valid_until: DateTime<Utc>,
}

/// The protected inbound caller and whom it may reach.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundCaller {
    pub contact_reference: Uuid,
    #[serde(default)]
    pub allowed_targets: Vec<Value>,
}

#[async_trait]
pub trait PrivacyRelayPort: Send + Sync {
    async fn resolve_for_execution(
        &self,
        request: &ResolveForExecution,
    ) -> Result<ResolvedExecution, PortError>;

    async fn resolve_inbound(
        &self,
        called: &ExecutionNumber,
        provider_event_reference: Option<&str>,
        observed_at: DateTime<Utc>,
        started_at: Option<DateTime<Utc>>,
        correlation_id: &str,
    ) -> Result<InboundMapping, PortError>;

    async fn protect_inbound_caller(
        &self,
        inbound_resolution_id: Uuid,
        caller: &ExecutionNumber,
        correlation_id: &str,
    ) -> Result<InboundCaller, PortError>;

    /// Part IV L: protects the caller of a DCN call in the scope of Numbering's signed
    /// INBOUND_VOICE grant (Privacy Relay verifies it; Telephony asserts no scope).
    async fn protect_dcn_caller(
        &self,
        _number_grant: &str,
        _caller: &ExecutionNumber,
        _correlation_id: &str,
    ) -> Result<Uuid, PortError> {
        Err(PortError::Unavailable)
    }
}

#[async_trait]
pub trait CallingPort: Send + Sync {
    /// Delivers one normalized event (Calling deduplicates by event id), continuing
    /// `correlation_id` (the originating command's) when there is one.
    async fn deliver_event(
        &self,
        payload: &Value,
        correlation_id: Option<&str>,
    ) -> Result<(), PortError>;

    /// Offers an inbound public call; Calling answers with the canonical Call and leg.
    async fn offer_inbound(&self, offer: &Value) -> Result<Value, PortError>;

    /// Part IV L: offers an inbound call to a DCN (Numbering's grant, no number).
    async fn offer_dcn_inbound(&self, _offer: &Value) -> Result<Value, PortError> {
        Err(PortError::Unavailable)
    }

    /// Part F: offers an internal enterprise SIP dialog (opaque device reference and
    /// dialled extension); Calling answers with the canonical Call and leg.
    async fn offer_enterprise(&self, _offer: &Value) -> Result<Value, PortError> {
        Err(PortError::Unavailable)
    }

    /// Part F: delivers ONE key of an enterprise leg's caller to Calling's IVR. Never
    /// persisted (no outbox), never logged: a lost key is like an unheard one (the IVR
    /// times out and retries).
    async fn deliver_keypad(
        &self,
        _payload: &Value,
        _correlation_id: Option<&str>,
    ) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }

    /// Part F: delivers a finished / failed edge media capture (base64 audio + integrity,
    /// or a bounded failure). Unavailable (incl. Calling's 503 when its artifact storage
    /// is down) is retried by the caller; Denied / NotFound are final.
    async fn deliver_capture(
        &self,
        _report: &Value,
        _correlation_id: Option<&str>,
    ) -> Result<Value, PortError> {
        Err(PortError::Unavailable)
    }
}

/// Not configured: everything fails closed (protected origination never resolves a
/// destination; events stay in the outbox).
pub struct Unconfigured;

#[async_trait]
impl PrivacyRelayPort for Unconfigured {
    async fn resolve_for_execution(
        &self,
        _: &ResolveForExecution,
    ) -> Result<ResolvedExecution, PortError> {
        Err(PortError::Unavailable)
    }

    async fn resolve_inbound(
        &self,
        _: &ExecutionNumber,
        _: Option<&str>,
        _: DateTime<Utc>,
        _: Option<DateTime<Utc>>,
        _: &str,
    ) -> Result<InboundMapping, PortError> {
        Err(PortError::Unavailable)
    }

    async fn protect_inbound_caller(
        &self,
        _: Uuid,
        _: &ExecutionNumber,
        _: &str,
    ) -> Result<InboundCaller, PortError> {
        Err(PortError::Unavailable)
    }
}

#[async_trait]
impl CallingPort for Unconfigured {
    async fn deliver_event(&self, _: &Value, _: Option<&str>) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }

    async fn offer_inbound(&self, _: &Value) -> Result<Value, PortError> {
        Err(PortError::Unavailable)
    }
}

fn correlation(raw: &str) -> CorrelationId {
    CorrelationId::parse(raw).unwrap_or_else(|_| CorrelationId::generate())
}

/// The correlation an outbound call continues: the originating command's, else a fresh
/// one (a leg no command touched, or a row written before correlations were stored).
fn continued(raw: Option<&str>) -> CorrelationId {
    raw.map(correlation).unwrap_or_else(CorrelationId::generate)
}

fn port_error(e: InternalCallError) -> PortError {
    match e {
        InternalCallError::Status { status: 404, .. } => PortError::NotFound,
        InternalCallError::Status { status, code, .. } if (400..500).contains(&status) => {
            PortError::Denied(code.unwrap_or_else(|| "DENIED".to_string()))
        }
        other => {
            // Never log URLs, bodies or tokens: the error type carries none.
            tracing::warn!(error = %other, "internal dependency unavailable; failing closed");
            PortError::Unavailable
        }
    }
}

/// Authenticated HTTP adapter to Privacy Relay.
#[derive(Clone)]
pub struct HttpPrivacyRelay {
    auth: ServiceAuthRuntime,
}

impl HttpPrivacyRelay {
    pub fn new(auth: ServiceAuthRuntime) -> Self {
        Self { auth }
    }
}

#[async_trait]
impl PrivacyRelayPort for HttpPrivacyRelay {
    async fn resolve_for_execution(
        &self,
        r: &ResolveForExecution,
    ) -> Result<ResolvedExecution, PortError> {
        let outbound = OutboundServiceRequest::continuing(
            ServiceId::privacy_relay(),
            &correlation(&r.correlation_id),
        )
        .with_delegated_context(r.delegated.clone());
        let body = json!({
            "grantId": r.grant_id,
            "grantSecret": r.grant_secret,
            "callId": r.call_id,
            "callLegId": r.call_leg_id,
            "executionAttemptId": r.execution_attempt_id,
        });
        let response = self
            .auth
            .client()
            .post_json(
                &outbound,
                &format!(
                    "/internal/v1/contacts/{}/resolve-for-execution",
                    r.contact_reference
                ),
                &body,
            )
            .await
            .map_err(port_error)?;
        let value: Value = response.json().map_err(|_| PortError::Unavailable)?;
        let resolution = &value["resolution"];
        let destination = resolution["e164"]
            .as_str()
            .and_then(ExecutionNumber::parse)
            .ok_or(PortError::Unavailable)?;
        if resolution["callLegId"].as_str() != Some(&r.call_leg_id.to_string()) {
            return Err(PortError::Unavailable);
        }
        let caller_identity = resolution["callerIdentity"]["e164"]
            .as_str()
            .and_then(ExecutionNumber::parse);
        let relay_assignment_ref = resolution["callerIdentity"]["assignmentId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok());
        Ok(ResolvedExecution {
            destination,
            country: resolution["country"].as_str().map(str::to_string),
            caller_identity,
            relay_assignment_ref,
        })
    }

    async fn resolve_inbound(
        &self,
        called: &ExecutionNumber,
        provider_event_reference: Option<&str>,
        observed_at: DateTime<Utc>,
        started_at: Option<DateTime<Utc>>,
        correlation_id: &str,
    ) -> Result<InboundMapping, PortError> {
        let outbound = OutboundServiceRequest::continuing(
            ServiceId::privacy_relay(),
            &correlation(correlation_id),
        );
        let mut body = json!({
            "calledRelayIdentity": called.expose(),
            "providerObservedAt": observed_at,
        });
        if let Some(r) = provider_event_reference {
            body["providerEventReference"] = json!(r);
        }
        if let Some(s) = started_at {
            body["providerStartedAt"] = json!(s);
        }
        let response = self
            .auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/relay-routing/resolve-inbound",
                &body,
            )
            .await
            .map_err(port_error)?;
        let value: Value = response.json().map_err(|_| PortError::Unavailable)?;
        serde_json::from_value(value["resolution"].clone()).map_err(|_| PortError::Unavailable)
    }

    async fn protect_inbound_caller(
        &self,
        inbound_resolution_id: Uuid,
        caller: &ExecutionNumber,
        correlation_id: &str,
    ) -> Result<InboundCaller, PortError> {
        let outbound = OutboundServiceRequest::continuing(
            ServiceId::privacy_relay(),
            &correlation(correlation_id),
        );
        let body = json!({
            "inboundResolutionId": inbound_resolution_id,
            "telephoneNumber": caller.expose(),
        });
        let response = self
            .auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/relay-routing/protect-inbound-caller",
                &body,
            )
            .await
            .map_err(port_error)?;
        let value: Value = response.json().map_err(|_| PortError::Unavailable)?;
        serde_json::from_value(value["inboundCaller"].clone()).map_err(|_| PortError::Unavailable)
    }

    async fn protect_dcn_caller(
        &self,
        number_grant: &str,
        caller: &ExecutionNumber,
        correlation_id: &str,
    ) -> Result<Uuid, PortError> {
        let outbound = OutboundServiceRequest::continuing(
            ServiceId::privacy_relay(),
            &correlation(correlation_id),
        );
        let body = json!({ "numberGrant": number_grant, "telephoneNumber": caller.expose() });
        let response = self
            .auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/relay-routing/protect-dcn-caller",
                &body,
            )
            .await
            .map_err(port_error)?;
        let value: Value = response.json().map_err(|_| PortError::Unavailable)?;
        value["inboundCaller"]["contactReference"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(PortError::Unavailable)
    }
}

/// Authenticated HTTP adapter to Calling.
#[derive(Clone)]
pub struct HttpCalling {
    auth: ServiceAuthRuntime,
}

impl HttpCalling {
    pub fn new(auth: ServiceAuthRuntime) -> Self {
        Self { auth }
    }
}

#[async_trait]
impl CallingPort for HttpCalling {
    async fn deliver_event(
        &self,
        payload: &Value,
        correlation_id: Option<&str>,
    ) -> Result<(), PortError> {
        let correlation_id = continued(correlation_id);
        let outbound = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation_id);
        self.auth
            .client()
            .post_json(&outbound, "/internal/v1/telephony/events", payload)
            .await
            .map(|_| ())
            .map_err(port_error)
    }

    async fn offer_inbound(&self, offer: &Value) -> Result<Value, PortError> {
        let correlation_id = offer["correlationId"]
            .as_str()
            .map(correlation)
            .unwrap_or_else(CorrelationId::generate);
        let outbound = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation_id);
        let response = self
            .auth
            .client()
            .post_json(&outbound, "/internal/v1/telephony/inbound-calls", offer)
            .await
            .map_err(port_error)?;
        response.json().map_err(|_| PortError::Unavailable)
    }

    async fn offer_dcn_inbound(&self, offer: &Value) -> Result<Value, PortError> {
        let correlation_id = offer["correlationId"]
            .as_str()
            .map(correlation)
            .unwrap_or_else(CorrelationId::generate);
        let outbound = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation_id);
        let response = self
            .auth
            .client()
            .post_json(&outbound, "/internal/v1/telephony/dcn-inbound-calls", offer)
            .await
            .map_err(port_error)?;
        response.json().map_err(|_| PortError::Unavailable)
    }

    async fn offer_enterprise(&self, offer: &Value) -> Result<Value, PortError> {
        let correlation_id = offer["correlationId"]
            .as_str()
            .map(correlation)
            .unwrap_or_else(CorrelationId::generate);
        let outbound = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation_id);
        let response = self
            .auth
            .client()
            .post_json(&outbound, "/internal/v1/telephony/enterprise-calls", offer)
            .await
            .map_err(port_error)?;
        response.json().map_err(|_| PortError::Unavailable)
    }

    async fn deliver_keypad(
        &self,
        payload: &Value,
        correlation_id: Option<&str>,
    ) -> Result<(), PortError> {
        let correlation_id = continued(correlation_id);
        let outbound = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation_id);
        self.auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/telephony/enterprise-keypad",
                payload,
            )
            .await
            .map(|_| ())
            .map_err(port_error)
    }

    async fn deliver_capture(
        &self,
        report: &Value,
        correlation_id: Option<&str>,
    ) -> Result<Value, PortError> {
        let correlation_id = continued(correlation_id);
        let outbound = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation_id);
        let response = self
            .auth
            .client()
            .post_json(&outbound, "/internal/v1/telephony/captures", report)
            .await
            .map_err(port_error)?;
        response.json().map_err(|_| PortError::Unavailable)
    }
}

// ---------------------------------------------------------------------------------------
// Numbering (Part IV Workstream L): DCN inbound resolution, caller-identity verification,
// voice route readiness.
// ---------------------------------------------------------------------------------------

/// The customer scope Numbering resolved a DCN to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DcnScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

/// The routing of a received DCN call (references only; the grant is a bearer token for
/// Calling, never logged).
#[derive(Clone)]
pub struct DcnRouting {
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub ownership_class: String,
    pub scope: DcnScope,
    pub target_kind: String,
    pub target_ref: Uuid,
    pub grant: String,
    pub grant_id: Uuid,
}

impl fmt::Debug for DcnRouting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DcnRouting")
            .field("number_resource_id", &self.number_resource_id)
            .field("assignment_id", &self.assignment_id)
            .field("routing_revision", &self.routing_revision)
            .field("target_kind", &self.target_kind)
            .finish_non_exhaustive()
    }
}

/// Numbering's answer for a received DCN call.
#[derive(Debug, Clone)]
pub enum DcnInbound {
    Routed(Box<DcnRouting>),
    /// Assigned, but voice routes nowhere (e.g. an offboarded line).
    NotRouted,
    /// Not routable now (Numbering's stable reason).
    Unavailable(String),
}

/// A CALLER_IDENTITY grant Numbering verified (and consumed) for one operation.
pub struct VerifiedDcnIdentity {
    pub identity: ExecutionNumber,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub grant_id: Uuid,
    pub scope: DcnScope,
}

impl fmt::Debug for VerifiedDcnIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedDcnIdentity")
            .field("number_resource_id", &self.number_resource_id)
            .field("grant_id", &self.grant_id)
            .finish_non_exhaustive()
    }
}

/// The current VOICE route configuration of a DCN (for Telephony's acknowledgement).
#[derive(Debug, Clone)]
pub struct DcnVoiceRoute {
    pub route_ref: Uuid,
    pub route_revision: i64,
    pub status: String,
}

#[async_trait]
pub trait NumberingPort: Send + Sync {
    /// `POST /internal/v1/routing/inbound-resolutions` (VOICE).
    async fn resolve_inbound_voice(
        &self,
        called: &ExecutionNumber,
        correlation_id: &str,
    ) -> Result<DcnInbound, PortError>;

    /// `POST /internal/v1/routing/grant-verifications` (consume): a refusal is
    /// `Ok(Err(reason))`.
    async fn verify_caller_identity(
        &self,
        grant: &str,
        operation_ref: &str,
        correlation_id: &str,
    ) -> Result<Result<VerifiedDcnIdentity, String>, PortError>;

    /// The DCN's canonical number and its current VOICE route configuration.
    async fn voice_route(
        &self,
        number_resource_id: Uuid,
    ) -> Result<(ExecutionNumber, Option<DcnVoiceRoute>), PortError>;

    /// `POST /internal/v1/number-resources/{id}/route-readiness` (VOICE).
    async fn acknowledge_voice_route(
        &self,
        number_resource_id: Uuid,
        route: &DcnVoiceRoute,
    ) -> Result<(), PortError>;
}

/// Not configured: every DCN operation fails closed (relay-number and enterprise paths are
/// unaffected).
pub struct UnconfiguredNumbering;

#[async_trait]
impl NumberingPort for UnconfiguredNumbering {
    async fn resolve_inbound_voice(
        &self,
        _: &ExecutionNumber,
        _: &str,
    ) -> Result<DcnInbound, PortError> {
        Err(PortError::Unavailable)
    }

    async fn verify_caller_identity(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Result<VerifiedDcnIdentity, String>, PortError> {
        Err(PortError::Unavailable)
    }

    async fn voice_route(
        &self,
        _: Uuid,
    ) -> Result<(ExecutionNumber, Option<DcnVoiceRoute>), PortError> {
        Err(PortError::Unavailable)
    }

    async fn acknowledge_voice_route(&self, _: Uuid, _: &DcnVoiceRoute) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }
}

/// Authenticated HTTP adapter to Numbering.
#[derive(Clone)]
pub struct HttpNumbering {
    auth: ServiceAuthRuntime,
}

impl HttpNumbering {
    pub fn new(auth: ServiceAuthRuntime) -> Self {
        Self { auth }
    }
}

fn uuid_at(v: &Value, k: &str) -> Result<Uuid, PortError> {
    v[k].as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(PortError::Unavailable)
}

fn scope_at(v: &Value) -> Result<DcnScope, PortError> {
    serde_json::from_value(v.clone()).map_err(|_| PortError::Unavailable)
}

#[async_trait]
impl NumberingPort for HttpNumbering {
    async fn resolve_inbound_voice(
        &self,
        called: &ExecutionNumber,
        correlation_id: &str,
    ) -> Result<DcnInbound, PortError> {
        let outbound = OutboundServiceRequest::continuing(
            ServiceId::numbering(),
            &correlation(correlation_id),
        );
        let response = self
            .auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/routing/inbound-resolutions",
                &json!({ "family": "VOICE", "e164": called.expose() }),
            )
            .await
            .map_err(port_error)?;
        let v: Value = response.json().map_err(|_| PortError::Unavailable)?;
        match v["outcome"].as_str() {
            Some("ROUTED") => Ok(DcnInbound::Routed(Box::new(DcnRouting {
                number_resource_id: uuid_at(&v, "numberResourceId")?,
                assignment_id: uuid_at(&v, "assignmentId")?,
                routing_revision: v["routingRevision"]
                    .as_i64()
                    .ok_or(PortError::Unavailable)?,
                ownership_class: v["ownershipClass"]
                    .as_str()
                    .ok_or(PortError::Unavailable)?
                    .to_string(),
                scope: scope_at(&v["scope"])?,
                target_kind: v["target"]["kind"]
                    .as_str()
                    .ok_or(PortError::Unavailable)?
                    .to_string(),
                target_ref: uuid_at(&v["target"], "ref")?,
                grant: v["grant"]["grant"]
                    .as_str()
                    .ok_or(PortError::Unavailable)?
                    .to_string(),
                grant_id: uuid_at(&v["grant"], "grantId")?,
            }))),
            Some("NOT_ROUTED") => Ok(DcnInbound::NotRouted),
            Some("UNAVAILABLE") => Ok(DcnInbound::Unavailable(
                v["reason"].as_str().unwrap_or("UNAVAILABLE").to_string(),
            )),
            _ => Err(PortError::Unavailable),
        }
    }

    async fn verify_caller_identity(
        &self,
        grant: &str,
        operation_ref: &str,
        correlation_id: &str,
    ) -> Result<Result<VerifiedDcnIdentity, String>, PortError> {
        let outbound = OutboundServiceRequest::continuing(
            ServiceId::numbering(),
            &correlation(correlation_id),
        );
        let response = self
            .auth
            .client()
            .post_json(
                &outbound,
                "/internal/v1/routing/grant-verifications",
                &json!({ "grant": grant, "consume": true, "operationRef": operation_ref }),
            )
            .await
            .map_err(port_error)?;
        let v: Value = response.json().map_err(|_| PortError::Unavailable)?;
        if v["valid"] != true {
            return Ok(Err(v["reason"]
                .as_str()
                .unwrap_or("GRANT_INVALID")
                .to_string()));
        }
        if v["grantKind"] != "CALLER_IDENTITY" || v["consumed"] != true {
            return Ok(Err("GRANT_INVALID".to_string()));
        }
        let identity = v["e164"]
            .as_str()
            .and_then(ExecutionNumber::parse)
            .ok_or(PortError::Unavailable)?;
        Ok(Ok(VerifiedDcnIdentity {
            identity,
            number_resource_id: uuid_at(&v, "numberResourceId")?,
            assignment_id: uuid_at(&v, "assignmentId")?,
            routing_revision: v["routingRevision"]
                .as_i64()
                .ok_or(PortError::Unavailable)?,
            grant_id: uuid_at(&v, "grantId")?,
            scope: scope_at(&v["scope"])?,
        }))
    }

    async fn voice_route(
        &self,
        number_resource_id: Uuid,
    ) -> Result<(ExecutionNumber, Option<DcnVoiceRoute>), PortError> {
        let outbound = OutboundServiceRequest::new_operation(ServiceId::numbering());
        let resource: Value = self
            .auth
            .client()
            .get(
                &outbound,
                &format!("/internal/v1/number-resources/{number_resource_id}"),
            )
            .await
            .map_err(port_error)?
            .json()
            .map_err(|_| PortError::Unavailable)?;
        let e164 = resource["e164"]
            .as_str()
            .and_then(ExecutionNumber::parse)
            .ok_or(PortError::Unavailable)?;
        let outbound = OutboundServiceRequest::new_operation(ServiceId::numbering());
        let routes: Value = self
            .auth
            .client()
            .get(
                &outbound,
                &format!("/internal/v1/number-resources/{number_resource_id}/routes"),
            )
            .await
            .map_err(port_error)?
            .json()
            .map_err(|_| PortError::Unavailable)?;
        let voice = routes["routes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["family"] == "VOICE")
            .and_then(|r| {
                Some(DcnVoiceRoute {
                    route_ref: Uuid::parse_str(r["routeRef"].as_str()?).ok()?,
                    route_revision: r["routeRevision"].as_i64()?,
                    status: r["status"].as_str()?.to_string(),
                })
            });
        Ok((e164, voice))
    }

    async fn acknowledge_voice_route(
        &self,
        number_resource_id: Uuid,
        route: &DcnVoiceRoute,
    ) -> Result<(), PortError> {
        let outbound = OutboundServiceRequest::new_operation(ServiceId::numbering());
        self.auth
            .client()
            .post_json(
                &outbound,
                &format!("/internal/v1/number-resources/{number_resource_id}/route-readiness"),
                &json!({
                    "family": "VOICE",
                    "routeRef": route.route_ref,
                    "routeRevision": route.route_revision,
                }),
            )
            .await
            .map(|_| ())
            .map_err(port_error)
    }
}

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

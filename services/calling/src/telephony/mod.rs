//! Calling's side of Telephony (Part E §44–§51, §165–§166).
//!
//! Telephony (`services/telephony`) executes public-network legs; Calling keeps the
//! logical Call, its canonical CallLegs, the route decision, the authorization and the
//! business evidence. Calling:
//!
//! * asks Telephony for NORMALIZED route availability ([`TelephonyAvailability`]) —
//!   never provider names, carriers or trunks;
//! * sends durable, idempotent, versioned commands (originate / terminate / hold /
//!   resume / bridge) through [`TelephonyPort`] — never a telephone number: a public
//!   destination is an opaque ContactReference plus a single-use Contact Resolution
//!   Grant that only Telephony can exchange;
//! * applies Telephony's normalized, authenticated events to its own state machine
//!   ([`events`]); Telephony never sets Calling state;
//! * accounts telecom spend durably before any paid leg ([`spend`]).
//!
//! No SIP type, provider SDK, provider id, carrier status or telephone number exists in
//! Calling. Without a configured Telephony every public route is unavailable (a KNOWN
//! empty set) and application RTC is unaffected.

pub mod events;
pub mod http;
pub mod media;
pub mod spend;

use async_trait::async_trait;
use cf_service_auth::{
    client::InternalCallError, CorrelationId, DelegatedPlatformContext, OutboundServiceRequest,
    ServiceId,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    internal_auth::ServiceAuthRuntime,
    routing::{
        availability::{AvailabilityQuery, RouteAvailabilitySource},
        engine::{CandidateRoute, Evidence, RouteCapability},
    },
};

/// Why a Telephony request did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelephonyError {
    /// Telephony answered and refused (its stable error code). Nothing was executed for
    /// THIS request (a replay of an earlier accepted command is answered as accepted).
    Refused { status: u16, code: String },
    /// No answer (transport, timeout, configuration, 5xx, malformed response). The
    /// command may or may not have been persisted by Telephony: it is retried with the
    /// SAME command id / idempotency key, which Telephony deduplicates.
    Unavailable,
}

/// A leg control action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegControl {
    Terminate,
    Hold,
    Resume,
    /// Part F: answer an internal enterprise SIP dialog (IVR / voicemail need media).
    Answer,
    /// Part F: play one BUILT-IN prompt on an internal enterprise SIP leg.
    Prompt,
}

impl LegControl {
    pub fn path(&self) -> &'static str {
        match self {
            LegControl::Terminate => "terminate",
            LegControl::Hold => "hold",
            LegControl::Resume => "resume",
            LegControl::Answer => "answer",
            LegControl::Prompt => "prompt",
        }
    }
}

/// Calling's port to Telephony. Business logic depends on this trait only.
#[async_trait]
pub trait TelephonyPort: Send + Sync {
    /// Normalized candidate routes for `country` / `capability`; `Unknown` when
    /// Telephony could not be asked.
    async fn availability(
        &self,
        country: &str,
        capability: RouteCapability,
        correlation_id: &CorrelationId,
    ) -> Evidence<Vec<CandidateRoute>>;

    /// `POST /internal/v1/legs/originate` (cfmeet.telephony.command OriginateCommand v1).
    async fn originate(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError>;

    /// `POST /internal/v1/legs/{telephonyLegId}/{terminate|hold|resume|answer|prompt}`.
    async fn control(
        &self,
        telephony_leg_id: Uuid,
        action: LegControl,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError>;

    /// `POST /internal/v1/bridges`.
    async fn create_bridge(
        &self,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError>;

    /// `DELETE /internal/v1/bridges/{bridgeId}`.
    async fn end_bridge(
        &self,
        bridge_id: Uuid,
        correlation_id: &CorrelationId,
    ) -> Result<(), TelephonyError>;
}

/// No Telephony configured: no public route exists, every command fails safely.
pub struct UnconfiguredTelephony;

#[async_trait]
impl TelephonyPort for UnconfiguredTelephony {
    async fn availability(
        &self,
        _: &str,
        _: RouteCapability,
        _: &CorrelationId,
    ) -> Evidence<Vec<CandidateRoute>> {
        Evidence::Known(Vec::new())
    }

    async fn originate(
        &self,
        _: &DelegatedPlatformContext,
        _: &CorrelationId,
        _: &Value,
    ) -> Result<Value, TelephonyError> {
        Err(TelephonyError::Unavailable)
    }

    async fn control(
        &self,
        _: Uuid,
        _: LegControl,
        _: &CorrelationId,
        _: &Value,
    ) -> Result<Value, TelephonyError> {
        Err(TelephonyError::Unavailable)
    }

    async fn create_bridge(&self, _: &CorrelationId, _: &Value) -> Result<Value, TelephonyError> {
        Err(TelephonyError::Unavailable)
    }

    async fn end_bridge(&self, _: Uuid, _: &CorrelationId) -> Result<(), TelephonyError> {
        Err(TelephonyError::Unavailable)
    }
}

/// The authenticated HTTP adapter (service token, audience `cfmeet.telephony`; bounded
/// timeouts; no redirects; never a customer credential).
#[derive(Clone)]
pub struct HttpTelephony {
    service_auth: ServiceAuthRuntime,
}

impl std::fmt::Debug for HttpTelephony {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTelephony").finish_non_exhaustive()
    }
}

impl HttpTelephony {
    pub fn new(service_auth: ServiceAuthRuntime) -> Self {
        Self { service_auth }
    }
}

fn telephony_error(e: InternalCallError) -> TelephonyError {
    match e {
        InternalCallError::Status { status, code, .. } if (400..500).contains(&status) => {
            TelephonyError::Refused {
                status,
                code: code.unwrap_or_else(|| "REFUSED".to_string()),
            }
        }
        other => {
            // Never log URLs, bodies or tokens: the error type carries none.
            tracing::warn!(error = %other, "telephony unavailable");
            TelephonyError::Unavailable
        }
    }
}

#[async_trait]
impl TelephonyPort for HttpTelephony {
    async fn availability(
        &self,
        country: &str,
        capability: RouteCapability,
        correlation_id: &CorrelationId,
    ) -> Evidence<Vec<CandidateRoute>> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::telephony(), correlation_id);
        let path = format!(
            "/internal/v1/routes/availability?country={country}&capability={}",
            capability.as_str()
        );
        let Ok(response) = self
            .service_auth
            .client()
            .exchange_json(&outbound, axum::http::Method::GET, &path, None::<&Value>)
            .await
        else {
            return Evidence::Unknown;
        };
        if response.status() != 200 {
            return Evidence::Unknown;
        }
        let Ok(body) = serde_json::from_slice::<Value>(response.body()) else {
            return Evidence::Unknown;
        };
        match serde_json::from_value::<Vec<CandidateRoute>>(body["routes"].clone()) {
            Ok(routes) => Evidence::Known(routes),
            Err(_) => Evidence::Unknown,
        }
    }

    async fn originate(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::telephony(), correlation_id)
            .with_delegated_context(delegated.clone());
        let response = self
            .service_auth
            .client()
            .post_json(&outbound, "/internal/v1/legs/originate", command)
            .await
            .map_err(telephony_error)?;
        response.json().map_err(|_| TelephonyError::Unavailable)
    }

    async fn control(
        &self,
        telephony_leg_id: Uuid,
        action: LegControl,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::telephony(), correlation_id);
        let response = self
            .service_auth
            .client()
            .post_json(
                &outbound,
                &format!("/internal/v1/legs/{telephony_leg_id}/{}", action.path()),
                command,
            )
            .await
            .map_err(telephony_error)?;
        response.json().map_err(|_| TelephonyError::Unavailable)
    }

    async fn create_bridge(
        &self,
        correlation_id: &CorrelationId,
        command: &Value,
    ) -> Result<Value, TelephonyError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::telephony(), correlation_id);
        let response = self
            .service_auth
            .client()
            .post_json(&outbound, "/internal/v1/bridges", command)
            .await
            .map_err(telephony_error)?;
        response.json().map_err(|_| TelephonyError::Unavailable)
    }

    async fn end_bridge(
        &self,
        bridge_id: Uuid,
        correlation_id: &CorrelationId,
    ) -> Result<(), TelephonyError> {
        let outbound = OutboundServiceRequest::continuing(ServiceId::telephony(), correlation_id);
        self.service_auth
            .client()
            .exchange_json(
                &outbound,
                axum::http::Method::DELETE,
                &format!("/internal/v1/bridges/{bridge_id}"),
                None::<&Value>,
            )
            .await
            .map_err(telephony_error)
            .and_then(|r| match r.status() {
                200..=299 | 404 => Ok(()),
                status if (400..500).contains(&status) => Err(TelephonyError::Refused {
                    status,
                    code: "REFUSED".into(),
                }),
                _ => Err(TelephonyError::Unavailable),
            })
    }
}

/// The Routing Engine's availability source backed by Telephony.
pub struct TelephonyAvailability {
    port: std::sync::Arc<dyn TelephonyPort>,
}

impl TelephonyAvailability {
    pub fn new(port: std::sync::Arc<dyn TelephonyPort>) -> Self {
        Self { port }
    }
}

#[async_trait]
impl RouteAvailabilitySource for TelephonyAvailability {
    async fn availability(&self, query: &AvailabilityQuery) -> Evidence<Vec<CandidateRoute>> {
        let Some(country) = query.country.as_deref() else {
            return Evidence::Unknown;
        };
        let capability = query
            .capabilities
            .first()
            .copied()
            .unwrap_or(RouteCapability::VoiceOutbound);
        self.port
            .availability(country, capability, &CorrelationId::generate())
            .await
    }
}

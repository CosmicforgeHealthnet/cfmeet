//! Calling's side of Privacy Relay (Part D §26–§30).
//!
//! Privacy Relay (`services/privacy-relay`) is the authority for Communication Contexts,
//! Privacy Relay Sessions and communication authorization. Calling:
//!
//! * is the authenticated PLATFORM INGRESS of the public `/v1/privacy-relay/*` API
//!   ([`http`]): it authenticates the customer credential, checks the public scope,
//!   validates referenced principals, derives the signed delegated platform context and
//!   forwards over authenticated internal HTTP — no Privacy Relay business logic, no
//!   customer credential forwarding, no Privacy Relay data;
//! * asks Privacy Relay for CURRENT authority before every protected call creation and
//!   every transfer introduction on a protected call ([`PrivacyRelayAuthority`]), and
//!   FAILS CLOSED when it cannot get an answer — never falling back to direct calling;
//! * receives restrictive active-call enforcement commands ([`enforcement`]).
//!
//! Calling never imports Privacy Relay source and never touches its database: it stores
//! only the opaque references and decision evidence it was given.

pub mod enforcement;
pub mod http;

use std::fmt;

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::InternalCallError, CorrelationId, DelegatedPlatformContext, OutboundServiceRequest,
    ServiceId,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::internal_auth::ServiceAuthRuntime;

/// Authorization policy recorded on a protected call.
pub const PRIVACY_RELAY_POLICY: &str = "PRIVACY_RELAY_V1";
/// Privacy Relay's communication authorization route.
pub const AUTHORIZE_PATH: &str = "/internal/v1/communication/authorize";

/// Why a protected-call authorization is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationPurpose {
    CallCreate,
    CallTransfer,
}

impl AuthorizationPurpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthorizationPurpose::CallCreate => "CALL_CREATE",
            AuthorizationPurpose::CallTransfer => "CALL_TRANSFER",
        }
    }
}

/// One protected-call authorization question (CF Meet ids only).
#[derive(Debug, Clone)]
pub struct CallAuthorization {
    pub relay_session_id: Uuid,
    pub source_principal_id: Uuid,
    pub target_principal_ids: Vec<Uuid>,
    pub purpose: AuthorizationPurpose,
}

/// Privacy Relay's decision (safe ids, versions and validity only).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyDecision {
    pub decision_id: Uuid,
    pub decision: String,
    pub reason: String,
    pub context_id: Uuid,
    pub context_version: i64,
    pub relay_session_id: Uuid,
    pub relay_session_version: i64,
    pub strategy: String,
    pub policy_version: String,
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
    pub decided_at: DateTime<Utc>,
}

const STRATEGIES: &[&str] = &[
    "APP_ONLY",
    "APP_PREFERRED",
    "PUBLIC_RELAY_ALLOWED",
    "CALLBACK_RELAY_ALLOWED",
];

impl PrivacyDecision {
    /// AUTHORIZED, internally consistent, and for the session that was asked about.
    pub fn is_authorized_for(&self, relay_session_id: Uuid) -> bool {
        self.decision == "AUTHORIZED"
            && self.reason == "AUTHORIZED"
            && self.valid_until.is_some()
            && self.relay_session_id == relay_session_id
    }

    /// Bounded, well-formed values (anything else is treated as a malformed answer).
    fn well_formed(&self) -> bool {
        let token = |s: &str| {
            !s.is_empty()
                && s.len() <= 32
                && s.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        };
        token(&self.decision)
            && token(&self.reason)
            && STRATEGIES.contains(&self.strategy.as_str())
            && token(&self.policy_version)
            && self.context_version >= 1
            && self.relay_session_version >= 1
    }
}

/// Failure to obtain an answer. `NotFound`: the session does not exist in the caller's
/// scope. Everything else (timeout, transport, authentication, configuration, malformed
/// response, server error) is `Unavailable` and fails CLOSED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyRelayError {
    NotFound,
    Unavailable,
}

/// A forwarded ingress request (internal path, JSON body).
#[derive(Debug, Clone)]
pub struct ProxyRequest {
    pub delegated: DelegatedPlatformContext,
    pub correlation_id: CorrelationId,
    pub method: Method,
    pub path: String,
    pub body: Option<Value>,
}

/// Privacy Relay's answer to a forwarded request (any status).
#[derive(Debug, Clone)]
pub struct ProxyResponse {
    pub status: u16,
    pub body: Value,
    pub replayed: bool,
}

/// Calling's port to Privacy Relay. Business logic depends on this trait only.
#[async_trait]
pub trait PrivacyRelayAuthority: Send + Sync {
    /// Current authority for a protected call creation / transfer introduction.
    async fn authorize_call(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        request: &CallAuthorization,
    ) -> Result<PrivacyDecision, PrivacyRelayError>;

    /// Forwards a public ingress request.
    async fn proxy(&self, request: ProxyRequest) -> Result<ProxyResponse, PrivacyRelayError>;
}

/// No Privacy Relay configured: protected calls and the ingress fail closed.
pub struct UnconfiguredPrivacyRelay;

#[async_trait]
impl PrivacyRelayAuthority for UnconfiguredPrivacyRelay {
    async fn authorize_call(
        &self,
        _: &DelegatedPlatformContext,
        _: &CorrelationId,
        _: &CallAuthorization,
    ) -> Result<PrivacyDecision, PrivacyRelayError> {
        Err(PrivacyRelayError::Unavailable)
    }

    async fn proxy(&self, _: ProxyRequest) -> Result<ProxyResponse, PrivacyRelayError> {
        Err(PrivacyRelayError::Unavailable)
    }
}

/// The authenticated HTTP adapter: service token (audience `cfmeet.privacy-relay`), the
/// signed delegated context, the operation's correlation id, bounded timeouts, no
/// redirects — and never the customer credential.
#[derive(Clone)]
pub struct HttpPrivacyRelay {
    service_auth: ServiceAuthRuntime,
}

impl fmt::Debug for HttpPrivacyRelay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpPrivacyRelay").finish_non_exhaustive()
    }
}

impl HttpPrivacyRelay {
    pub fn new(service_auth: ServiceAuthRuntime) -> Self {
        Self { service_auth }
    }

    fn outbound(
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
    ) -> OutboundServiceRequest {
        OutboundServiceRequest::continuing(ServiceId::privacy_relay(), correlation_id)
            .with_delegated_context(delegated.clone())
    }
}

fn unavailable(error: &InternalCallError) -> PrivacyRelayError {
    // Never log URLs, bodies or tokens: the error type carries none.
    tracing::warn!(%error, "privacy relay unavailable; failing closed");
    PrivacyRelayError::Unavailable
}

#[async_trait]
impl PrivacyRelayAuthority for HttpPrivacyRelay {
    async fn authorize_call(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
        request: &CallAuthorization,
    ) -> Result<PrivacyDecision, PrivacyRelayError> {
        let body = json!({
            "relaySessionId": request.relay_session_id,
            "source": { "principalId": request.source_principal_id },
            "targets": request.target_principal_ids.iter().map(|t| json!({ "principalId": t })).collect::<Vec<_>>(),
            "channel": "VOICE",
            "route": "APPLICATION",
            "purpose": request.purpose.as_str(),
        });
        let response = self
            .service_auth
            .client()
            .post_json(
                &Self::outbound(delegated, correlation_id),
                AUTHORIZE_PATH,
                &body,
            )
            .await
            .map_err(|e| match e {
                InternalCallError::Status { status: 404, .. } => PrivacyRelayError::NotFound,
                other => unavailable(&other),
            })?;
        let decision: PrivacyDecision = response.json().map_err(|e| unavailable(&e))?;
        if !decision.well_formed() || decision.relay_session_id != request.relay_session_id {
            tracing::warn!("privacy relay returned a malformed decision; failing closed");
            return Err(PrivacyRelayError::Unavailable);
        }
        Ok(decision)
    }

    async fn proxy(&self, request: ProxyRequest) -> Result<ProxyResponse, PrivacyRelayError> {
        let response = self
            .service_auth
            .client()
            .exchange_json(
                &Self::outbound(&request.delegated, &request.correlation_id),
                request.method.clone(),
                &request.path,
                request.body.as_ref(),
            )
            .await
            .map_err(|e| unavailable(&e))?;
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).map_err(|_| PrivacyRelayError::Unavailable)?
        };
        Ok(ProxyResponse {
            status: response.status(),
            replayed: response.header("idempotent-replayed") == Some("true"),
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision() -> PrivacyDecision {
        PrivacyDecision {
            decision_id: Uuid::new_v4(),
            decision: "AUTHORIZED".into(),
            reason: "AUTHORIZED".into(),
            context_id: Uuid::new_v4(),
            context_version: 3,
            relay_session_id: Uuid::nil(),
            relay_session_version: 1,
            strategy: "APP_ONLY".into(),
            policy_version: "CONTEXT_POLICY_V1".into(),
            valid_until: Some(Utc::now()),
            decided_at: Utc::now(),
        }
    }

    #[test]
    fn only_a_consistent_authorized_decision_for_the_asked_session_authorizes() {
        assert!(decision().well_formed());
        assert!(decision().is_authorized_for(Uuid::nil()));
        assert!(!decision().is_authorized_for(Uuid::new_v4()));
        let mut d = decision();
        d.reason = "CONTEXT_REVOKED".into();
        assert!(!d.is_authorized_for(Uuid::nil()));
        let mut d = decision();
        d.valid_until = None;
        assert!(!d.is_authorized_for(Uuid::nil()));
        let mut d = decision();
        d.strategy = "+2348012345678".into();
        assert!(!d.well_formed());
    }
}

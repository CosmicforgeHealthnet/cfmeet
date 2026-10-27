//! Human sessions of the Developer Console (Part VI §50).
//!
//! The Developer Platform never verifies a CF Meet session itself and never holds the
//! session signing secret: the Meeting Service (the human-session authority) verifies it
//! over authenticated internal HTTP (`POST /internal/v1/user-sessions/verify`,
//! permission `user-session.verify`). Unreachable means refused (fail closed).

use std::sync::Arc;

use cf_service_auth::{
    client::InternalServiceClient, CorrelationId, OutboundServiceRequest, ServiceId,
};
use serde_json::{json, Value};
use uuid::Uuid;

/// A verified human developer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanUser {
    pub user_id: Uuid,
    /// Present only when the session authority reports the address verified.
    pub verified_email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    Verified(HumanUser),
    Rejected,
}

#[derive(Debug, thiserror::Error)]
#[error("the session authority is unavailable: {0}")]
pub struct SessionAuthorityUnavailable(pub String);

#[async_trait::async_trait]
pub trait HumanSessions: Send + Sync {
    async fn verify(
        &self,
        session_token: &str,
        correlation: &CorrelationId,
    ) -> Result<SessionOutcome, SessionAuthorityUnavailable>;
}

/// The Meeting Service session bridge over authenticated internal HTTP.
pub struct MeetingServiceSessions {
    client: Arc<InternalServiceClient>,
}

impl MeetingServiceSessions {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl HumanSessions for MeetingServiceSessions {
    async fn verify(
        &self,
        session_token: &str,
        correlation: &CorrelationId,
    ) -> Result<SessionOutcome, SessionAuthorityUnavailable> {
        let request = OutboundServiceRequest::continuing(ServiceId::meeting_service(), correlation);
        let response = self
            .client
            .exchange_json(
                &request,
                axum::http::Method::POST,
                "/internal/v1/user-sessions/verify",
                Some(&json!({ "sessionToken": session_token })),
            )
            .await
            .map_err(|e| SessionAuthorityUnavailable(e.to_string()))?;
        if response.status() != 200 {
            return Err(SessionAuthorityUnavailable(format!(
                "HTTP {}",
                response.status()
            )));
        }
        let body: Value = response
            .json()
            .map_err(|_| SessionAuthorityUnavailable("malformed response".into()))?;
        match body.get("outcome").and_then(Value::as_str) {
            Some("VERIFIED") => {
                let user_id = body
                    .get("userId")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or_else(|| SessionAuthorityUnavailable("malformed response".into()))?;
                Ok(SessionOutcome::Verified(HumanUser {
                    user_id,
                    verified_email: body
                        .get("verifiedEmail")
                        .and_then(Value::as_str)
                        .map(str::to_ascii_lowercase),
                }))
            }
            Some("REJECTED") => Ok(SessionOutcome::Rejected),
            _ => Err(SessionAuthorityUnavailable("unknown outcome".into())),
        }
    }
}

/// No session authority configured: every session is refused as unavailable.
pub struct UnconfiguredSessions;

#[async_trait::async_trait]
impl HumanSessions for UnconfiguredSessions {
    async fn verify(
        &self,
        _session_token: &str,
        _correlation: &CorrelationId,
    ) -> Result<SessionOutcome, SessionAuthorityUnavailable> {
        Err(SessionAuthorityUnavailable(
            "no session authority is configured".into(),
        ))
    }
}

//! HEALTHCARE_APPOINTMENT access — `AppointmentAuthorizationAdapter` (A–D closure).
//!
//! Contract: `authorize(actor, meeting_space, conference, external_context) ->
//! Authorized | Denied`. The meeting service never decides clinical authorization
//! itself and never embeds HealthNet business logic: it asks the authoritative
//! appointment source and fails closed on any error, timeout, malformed answer or
//! appointment outside its validity window. `external_context` is an opaque value from
//! the client (e.g. an appointment token) that is only forwarded to the authority for
//! verification — it is never proof on its own.
//!
//! The authoritative HealthNet clinical/appointment source does not exist in this
//! monorepo. `HttpAppointmentAuthorizationAdapter` implements the integration contract
//! below; without `HEALTHNET_APPOINTMENT_AUTHZ_URL` the service uses
//! `UnconfiguredAppointmentAuthorization`, which denies every request.
//!
//! ```text
//! POST {HEALTHNET_APPOINTMENT_AUTHZ_URL}
//! Authorization: Bearer {HEALTHNET_APPOINTMENT_AUTHZ_TOKEN}
//! { "meetingSpaceId", "conferenceId", "appointmentRef", "externalContext",
//!   "actor": { "userId", "verifiedEmail" } }
//! → 200 { "authorized": bool, "role"?: "HOST"|"CO_HOST"|…|"PARTICIPANT",
//!         "validFrom"?: RFC3339, "validUntil"?: RFC3339 }
//! ```

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domain::Role,
    models::meeting_spaces,
    services::{actor::ActorContext, meeting_space::access_config},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppointmentDecision {
    /// Active appointment verified by the authority. `role` is capped below OWNER.
    Authorized {
        role: Option<Role>,
    },
    Denied {
        reason: String,
    },
}

#[async_trait]
pub trait AppointmentAuthorizationAdapter: Send + Sync {
    async fn authorize(
        &self,
        actor: &ActorContext,
        meeting_space: &meeting_spaces::Model,
        conference_id: Option<Uuid>,
        external_context: Option<&str>,
    ) -> AppointmentDecision;
}

/// Default when no appointment authority is configured: deny everything.
pub struct UnconfiguredAppointmentAuthorization;

#[async_trait]
impl AppointmentAuthorizationAdapter for UnconfiguredAppointmentAuthorization {
    async fn authorize(
        &self,
        _actor: &ActorContext,
        _space: &meeting_spaces::Model,
        _conference: Option<Uuid>,
        _external_context: Option<&str>,
    ) -> AppointmentDecision {
        AppointmentDecision::Denied {
            reason: "appointment authority not configured".into(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeActor {
    user_id: Option<Uuid>,
    verified_email: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeRequest<'a> {
    meeting_space_id: Uuid,
    conference_id: Option<Uuid>,
    appointment_ref: Option<String>,
    external_context: Option<&'a str>,
    actor: AuthorizeActor,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeResponse {
    authorized: bool,
    role: Option<Role>,
    valid_from: Option<DateTime<Utc>>,
    valid_until: Option<DateTime<Utc>>,
}

/// HTTP adapter to the external HealthNet appointment authority (fail closed).
pub struct HttpAppointmentAuthorizationAdapter {
    client: reqwest::Client,
    url: String,
    service_token: Option<String>,
}

impl HttpAppointmentAuthorizationAdapter {
    pub fn new(url: String, service_token: Option<String>, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            url,
            service_token,
        }
    }

    pub fn from_env() -> Option<Self> {
        let url = std::env::var("HEALTHNET_APPOINTMENT_AUTHZ_URL")
            .ok()
            .filter(|u| !u.is_empty())?;
        let token = std::env::var("HEALTHNET_APPOINTMENT_AUTHZ_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        let timeout_ms = std::env::var("HEALTHNET_APPOINTMENT_AUTHZ_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000);
        Some(Self::new(url, token, Duration::from_millis(timeout_ms)))
    }
}

fn denied(reason: impl Into<String>) -> AppointmentDecision {
    AppointmentDecision::Denied {
        reason: reason.into(),
    }
}

#[async_trait]
impl AppointmentAuthorizationAdapter for HttpAppointmentAuthorizationAdapter {
    async fn authorize(
        &self,
        actor: &ActorContext,
        space: &meeting_spaces::Model,
        conference_id: Option<Uuid>,
        external_context: Option<&str>,
    ) -> AppointmentDecision {
        let registered = actor.registered();
        let body = AuthorizeRequest {
            meeting_space_id: space.id,
            conference_id,
            appointment_ref: access_config(space).appointment_ref,
            external_context,
            actor: AuthorizeActor {
                user_id: registered.map(|r| r.user_id),
                verified_email: registered.and_then(|r| r.verified_email.clone()),
            },
        };
        let mut request = self.client.post(&self.url).json(&body);
        if let Some(token) = &self.service_token {
            request = request.bearer_auth(token);
        }
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("appointment authority unreachable: {e}");
                return denied("appointment authority unavailable");
            }
        };
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), "appointment authority refused");
            return denied("appointment authority refused the request");
        }
        let decision: AuthorizeResponse = match response.json().await {
            Ok(d) => d,
            Err(_) => return denied("malformed appointment authority response"),
        };
        if !decision.authorized {
            return denied("no active appointment");
        }
        let now = Utc::now();
        if decision.valid_from.map(|f| now < f).unwrap_or(false)
            || decision.valid_until.map(|u| now >= u).unwrap_or(false)
        {
            return denied("appointment is not currently active");
        }
        AppointmentDecision::Authorized {
            // The appointment authority can never mint ownership.
            role: decision
                .role
                .map(|r| if r == Role::Owner { Role::Host } else { r }),
        }
    }
}

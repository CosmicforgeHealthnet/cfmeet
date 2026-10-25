//! Public number lifecycle events to the Developer Platform (Part VI §53).
//!
//! Numbering's transactional outbox ([`super::outbox`]) already records every lifecycle
//! change in the SAME transaction as the state change. This transport makes the Developer
//! Platform the outbox's consumer: the committed envelope of a customer-visible lifecycle
//! event is published, unchanged, to the Developer Platform's
//! `POST /internal/v1/platform/events` (`developer-event.publish`, producer
//! `cfmeet.numbering`, de-duplicated there on (producer, `event_id`)); every other event is
//! acknowledged without leaving Numbering.
//!
//! What is published ([`is_public`]) — only events that
//!
//! * describe a customer-visible lifecycle step of a number held by a customer: a
//!   reservation, an assignment, a suspension / restoration, a release or a port
//!   ([`PUBLIC_EVENT_TYPES`]); and
//! * carry the owning customer scope (tenant, application AND application environment):
//!   the envelope's ownership. Platform inventory events (resource lifecycle,
//!   quarantine — a quarantined number belongs to no customer any more) have no scope
//!   and are never published; no scope is ever invented.
//!
//! The payloads are the registered `cfmeet.numbering.event` payloads: CF Meet ids,
//! revisions and the common code vocabulary, never a telephone number, provider
//! identifier / reference, credential or secret (a database CHECK refuses them). KYC,
//! eligibility, provider-command, provider-event, risk, usage, routing and relay facts
//! are internal and never published.
//!
//! `NUMBERING_PUBLIC_EVENTS_DESTINATION` ([`PublicEventsDestination`]):
//!
//! * `developer-platform` (default) — Numbering's events never reached customers before
//!   (Calling's ingress does not accept them), and the Developer Platform ingests in
//!   shadow mode until the webhook cutover, so publishing changes nothing a customer sees.
//!   Without `CF_DEVELOPER_PLATFORM_BASE_URL` the publisher is not started and events stay
//!   PENDING in the outbox, exactly as before this change.
//! * `none` — the publisher is not started (events stay PENDING).
//!
//! Failures keep the outbox's rules: a 4xx refusal other than `RATE_LIMITED` dead-letters
//! the event, anything else is retried with bounded backoff.

use std::sync::Arc;

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use serde_json::Value;

use super::outbox::{Delivery, EventTransport};

/// The platform-event ingress path of the Developer Platform.
pub const PLATFORM_EVENTS_PATH: &str = "/internal/v1/platform/events";

/// Customer-visible number lifecycle events (the registered internal event types).
pub const PUBLIC_EVENT_TYPES: &[&str] = &[
    "number.reservation.changed",
    "number.assignment.changed",
    "number.suspended",
    "number.restored",
    "number.release.changed",
    "number.port.changed",
];

/// `NUMBERING_PUBLIC_EVENTS_DESTINATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublicEventsDestination {
    /// No publication: events stay PENDING in the outbox.
    None,
    /// The Developer Platform (default; shadow ingestion until the webhook cutover).
    #[default]
    DeveloperPlatform,
}

impl PublicEventsDestination {
    pub const VARIABLE: &'static str = "NUMBERING_PUBLIC_EVENTS_DESTINATION";

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "none" => Some(Self::None),
            "developer-platform" => Some(Self::DeveloperPlatform),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::DeveloperPlatform => "developer-platform",
        }
    }
}

/// Whether a committed envelope is a public lifecycle event of a customer scope.
pub fn is_public(envelope: &Value) -> bool {
    let typed = envelope
        .get("event_type")
        .and_then(Value::as_str)
        .is_some_and(|t| PUBLIC_EVENT_TYPES.contains(&t));
    let scoped = ["tenant_id", "application_id", "application_environment_id"]
        .iter()
        .all(|k| envelope["ownership"].get(*k).is_some_and(Value::is_string));
    typed && scoped
}

/// The Developer Platform's service-authenticated event ingestion.
pub struct DeveloperPlatformEvents {
    client: Arc<InternalServiceClient>,
}

impl DeveloperPlatformEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl EventTransport for DeveloperPlatformEvents {
    async fn deliver(&self, envelope: &Value) -> Delivery {
        if !is_public(envelope) {
            // Internal facts and unscoped inventory events never leave Numbering.
            crate::metrics::inc(
                "numbering_public_events_total",
                &[("outcome", "not_public")],
            );
            return Delivery::Delivered;
        }
        // The hop continues the committed correlation id.
        let correlation = envelope["correlation_id"]
            .as_str()
            .and_then(|c| cf_service_auth::CorrelationId::parse(c).ok())
            .unwrap_or_else(cf_service_auth::CorrelationId::generate);
        let request =
            OutboundServiceRequest::continuing(ServiceId::developer_platform(), &correlation);
        let outcome = match self
            .client
            .post_json(&request, PLATFORM_EVENTS_PATH, envelope)
            .await
        {
            Ok(_) => Delivery::Delivered,
            Err(InternalCallError::Status {
                status: 400..=499,
                code,
                ..
            }) if code.as_deref() != Some("RATE_LIMITED") => {
                Delivery::Rejected(format!("REFUSED {}", code.unwrap_or_default()))
            }
            Err(other) => Delivery::Retryable(other.to_string()),
        };
        crate::metrics::inc(
            "numbering_public_events_total",
            &[(
                "outcome",
                match outcome {
                    Delivery::Delivered => "published",
                    Delivery::Rejected(_) => "rejected",
                    Delivery::Retryable(_) => "retry",
                },
            )],
        );
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_default_is_the_developer_platform_and_values_parse() {
        assert_eq!(
            PublicEventsDestination::default(),
            PublicEventsDestination::DeveloperPlatform
        );
        for d in [
            PublicEventsDestination::None,
            PublicEventsDestination::DeveloperPlatform,
        ] {
            assert_eq!(PublicEventsDestination::parse(d.as_str()), Some(d));
        }
        // Calling's ingress does not accept Numbering events: not a destination.
        assert_eq!(PublicEventsDestination::parse("calling"), None);
        assert_eq!(PublicEventsDestination::parse("both"), None);
    }

    #[test]
    fn only_scoped_lifecycle_events_are_public() {
        let scope = json!({
            "tenant_id": "11111111-1111-4111-8111-111111111111",
            "application_id": "22222222-2222-4222-8222-222222222222",
            "application_environment_id": "33333333-3333-4333-8333-333333333333",
        });
        for t in PUBLIC_EVENT_TYPES {
            assert!(is_public(&json!({ "event_type": t, "ownership": scope })));
            assert!(!is_public(&json!({ "event_type": t })), "{t} unscoped");
        }
        for t in [
            "number.kyc-case.changed",
            "number.kyc-evidence.received",
            "number.provider-command.changed",
            "number.provider-event.received",
            "number.eligibility.evaluated",
            "number.risk.decided",
            "number.usage.recorded",
            "number.quarantine.changed",
            "number.resource.lifecycle-changed",
        ] {
            assert!(
                !is_public(&json!({ "event_type": t, "ownership": scope })),
                "{t}"
            );
        }
        let partial = json!({ "event_type": "number.suspended", "ownership": {
            "tenant_id": "11111111-1111-4111-8111-111111111111",
            "application_id": "22222222-2222-4222-8222-222222222222" } });
        assert!(
            !is_public(&partial),
            "no environment scope: never published"
        );
    }
}

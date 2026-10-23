//! Event transport to the platform's customer webhook delivery: committed SMS events
//! (platform envelope) are published to `POST /internal/v1/platform/events` of
//!
//! * Calling (`platform-event.publish`), which normalizes them into public `sms.*`
//!   webhook events and delivers them signed (`cf-webhooks`) — today's path; and / or
//! * the Developer Platform (`developer-event.publish`, Part VI §53), the central
//!   receiver every domain service publishes to directly. It serves the SAME path and
//!   the SAME envelope, de-duplicates on (producer, `event_id`) and ingests in shadow
//!   mode until the webhook cutover.
//!
//! [`PublicEventsDestination`] (`SMS_GATEWAY_PUBLIC_EVENTS_DESTINATION`) chooses; the
//! default `calling` keeps today's behaviour. `both` publishes every event to both
//! receivers (shadow comparison); the outbox records each receiver's outcome durably so
//! a retry only re-sends to the receiver that has not accepted it yet (see
//! [`super::outbox`]). No other service's database is touched.

use std::sync::Arc;

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use serde_json::Value;

use super::outbox::{Delivery, EventTransport};

/// The platform-event ingress path (identical on Calling and the Developer Platform).
pub const PLATFORM_EVENTS_PATH: &str = "/internal/v1/platform/events";

/// One receiver of public events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventDestination {
    /// Calling's platform-event ingress (today's customer webhook delivery).
    Calling,
    /// The Developer Platform's central event ingestion (Part VI §53).
    DeveloperPlatform,
}

impl EventDestination {
    pub fn as_str(self) -> &'static str {
        match self {
            EventDestination::Calling => "calling",
            EventDestination::DeveloperPlatform => "developer-platform",
        }
    }

    /// The service the receiver runs as.
    pub fn service(self) -> ServiceId {
        match self {
            EventDestination::Calling => ServiceId::calling(),
            EventDestination::DeveloperPlatform => ServiceId::developer_platform(),
        }
    }
}

/// `SMS_GATEWAY_PUBLIC_EVENTS_DESTINATION`: where committed SMS events are published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublicEventsDestination {
    /// Calling only (default: today's behaviour, unchanged).
    #[default]
    Calling,
    /// The Developer Platform only.
    DeveloperPlatform,
    /// Both (dual publication for shadow comparison before the cutover).
    Both,
}

impl PublicEventsDestination {
    pub const VARIABLE: &'static str = "SMS_GATEWAY_PUBLIC_EVENTS_DESTINATION";

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "calling" => Some(Self::Calling),
            "developer-platform" => Some(Self::DeveloperPlatform),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calling => "calling",
            Self::DeveloperPlatform => "developer-platform",
            Self::Both => "both",
        }
    }

    /// The receivers, in publication order.
    pub fn destinations(self) -> &'static [EventDestination] {
        match self {
            Self::Calling => &[EventDestination::Calling],
            Self::DeveloperPlatform => &[EventDestination::DeveloperPlatform],
            Self::Both => &[
                EventDestination::Calling,
                EventDestination::DeveloperPlatform,
            ],
        }
    }
}

/// The service-authenticated platform-event ingress of one receiver.
pub struct PlatformEventTransport {
    client: Arc<InternalServiceClient>,
    target: ServiceId,
}

impl PlatformEventTransport {
    /// Calling's ingress (`platform-event.publish`).
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self::to(client, EventDestination::Calling)
    }

    /// The ingress of `destination` (`developer-event.publish` for the Developer Platform).
    pub fn to(client: Arc<InternalServiceClient>, destination: EventDestination) -> Self {
        Self {
            client,
            target: destination.service(),
        }
    }
}

#[async_trait::async_trait]
impl EventTransport for PlatformEventTransport {
    async fn deliver(&self, envelope: &Value) -> Delivery {
        let correlation = envelope["correlation_id"]
            .as_str()
            .and_then(|c| cf_service_auth::CorrelationId::parse(c).ok())
            .unwrap_or_else(cf_service_auth::CorrelationId::generate);
        let request = OutboundServiceRequest::continuing(self.target.clone(), &correlation);
        match self
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_parse_and_default_to_calling() {
        assert_eq!(
            PublicEventsDestination::default(),
            PublicEventsDestination::Calling
        );
        for d in [
            PublicEventsDestination::Calling,
            PublicEventsDestination::DeveloperPlatform,
            PublicEventsDestination::Both,
        ] {
            assert_eq!(PublicEventsDestination::parse(d.as_str()), Some(d));
        }
        assert_eq!(PublicEventsDestination::parse("local"), None);
        assert_eq!(PublicEventsDestination::parse("none"), None);
        assert_eq!(
            PublicEventsDestination::Both.destinations(),
            &[
                EventDestination::Calling,
                EventDestination::DeveloperPlatform
            ]
        );
    }
}

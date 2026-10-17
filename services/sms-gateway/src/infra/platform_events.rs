//! Event transport to the platform's customer webhook delivery: committed SMS events
//! (platform envelope) are published to Calling's `POST /internal/v1/platform/events`
//! (`platform-event.publish`), which normalizes them into public `sms.*` webhook events
//! and delivers them signed (`cf-webhooks`). No other service's database is touched.

use std::sync::Arc;

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use serde_json::Value;

use super::outbox::{Delivery, EventTransport};

pub struct PlatformEventTransport {
    client: Arc<InternalServiceClient>,
}

impl PlatformEventTransport {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl EventTransport for PlatformEventTransport {
    async fn deliver(&self, envelope: &Value) -> Delivery {
        let correlation = envelope["correlation_id"]
            .as_str()
            .and_then(|c| cf_service_auth::CorrelationId::parse(c).ok())
            .unwrap_or_else(cf_service_auth::CorrelationId::generate);
        let request = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation);
        match self
            .client
            .post_json(&request, "/internal/v1/platform/events", envelope)
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

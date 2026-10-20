//! Realtime fan-out from COMMITTED state (§66): the outbox publisher hands every
//! committed event to [`RealtimeTransport`], which publishes a pointer to the broker
//! (never content) and creates the event's push jobs. Nothing is published before the
//! database commit; a broker failure leaves the event PENDING for a bounded retry (and
//! connected clients recover from the database regardless).

use serde_json::Value;
use uuid::Uuid;

use super::broker::BrokerMessage;
use crate::{
    events::outbox::{Delivery, EventTransport, OutboxEvent},
    state::AppState,
};

pub struct RealtimeTransport {
    state: AppState,
}

impl RealtimeTransport {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

fn uuid_at(payload: &Value, key: &str) -> Option<Uuid> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// The broker message for a committed event (`None`: nothing to fan out).
pub fn broker_message(event: &OutboxEvent) -> Option<BrokerMessage> {
    if let (Some(c), Some(seq)) = (event.conversation_id, event.conversation_event_sequence) {
        return Some(BrokerMessage::Durable {
            conversation_id: c,
            event_sequence: seq,
            event_type: event.event_type.clone(),
            subject_principal_id: event
                .event_type
                .starts_with("messaging.member.")
                .then(|| uuid_at(&event.payload, "principal_id"))
                .flatten(),
        });
    }
    match event.event_type.as_str() {
        "messaging.device.revoked" => uuid_at(&event.payload, "device_id")
            .map(|device_id| BrokerMessage::DeviceRevoked { device_id }),
        "messaging.client-session.revoked" => uuid_at(&event.payload, "session_id")
            .map(|session_id| BrokerMessage::SessionRevoked { session_id }),
        "messaging.mls.welcome-available" => Some(BrokerMessage::Device {
            device_id: uuid_at(&event.payload, "device_id")?,
            event_type: event.event_type.clone(),
            reference_id: uuid_at(&event.payload, "welcome_id")?,
            conversation_id: uuid_at(&event.payload, "conversation_id"),
        }),
        _ => None,
    }
}

#[async_trait::async_trait]
impl EventTransport for RealtimeTransport {
    async fn deliver(&self, _envelope: &Value) -> Delivery {
        Delivery::Delivered
    }

    async fn deliver_event(&self, event: &OutboxEvent, _envelope: &Value) -> Delivery {
        // Push jobs first (idempotent per event and device): a retry after a broker
        // failure creates nothing twice.
        if event.event_type == "messaging.message.created"
            && event.payload.get("message_kind").and_then(Value::as_str) == Some("APPLICATION")
        {
            if let Err(e) = super::push::fanout_jobs(&self.state, event).await {
                return Delivery::Retryable(format!("push fan-out: {e}"));
            }
        }
        match broker_message(event) {
            None => Delivery::Delivered,
            Some(m) => match self.state.realtime.broker.publish(&m).await {
                Ok(()) => Delivery::Delivered,
                Err(e) => {
                    crate::metrics::inc("messaging_realtime_broker_publish_failures_total", &[]);
                    Delivery::Retryable(e.to_string())
                }
            },
        }
    }
}

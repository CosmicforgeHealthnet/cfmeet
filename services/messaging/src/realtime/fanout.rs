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
            recipients: Vec::new(),
        });
    }
    match event.event_type.as_str() {
        "messaging.device.revoked" => uuid_at(&event.payload, "device_id")
            .map(|device_id| BrokerMessage::DeviceRevoked { device_id }),
        "messaging.client-session.revoked" => uuid_at(&event.payload, "session_id")
            .map(|session_id| BrokerMessage::SessionRevoked { session_id }),
        "messaging.read-cursor.synced" => Some(BrokerMessage::ReadCursor {
            conversation_id: uuid_at(&event.payload, "conversation_id")?,
            principal_id: uuid_at(&event.payload, "principal_id")?,
            read_sequence: event.payload.get("read_sequence")?.as_i64()?,
        }),
        "messaging.mls.welcome-available" => Some(BrokerMessage::Device {
            device_id: uuid_at(&event.payload, "device_id")?,
            event_type: event.event_type.clone(),
            reference_id: uuid_at(&event.payload, "welcome_id")?,
            conversation_id: uuid_at(&event.payload, "conversation_id"),
        }),
        _ => None,
    }
}

/// At most this many member principals travel with one event for inbox routing; a
/// larger conversation is delivered to subscribed connections only.
pub const MAX_INBOX_RECIPIENTS: usize = 1024;

/// The inbox recipients of a durable conversation event: the conversation's ACTIVE
/// member principals now (after the commit), plus the event's subject (a removed member
/// learns about its own removal). Empty above [`MAX_INBOX_RECIPIENTS`] or on a database
/// error: inbox signals are a best-effort hint, and clients reconcile by REST.
async fn inbox_recipients(
    state: &AppState,
    conversation_id: Uuid,
    subject: Option<Uuid>,
) -> Vec<Uuid> {
    #[derive(sea_orm::FromQueryResult)]
    struct Row {
        principal_id: Uuid,
    }
    let rows: Vec<Row> = match crate::db::all(
        &state.db,
        "SELECT principal_id FROM messaging_conversation_members \
          WHERE conversation_id = $1 AND state = 'ACTIVE' LIMIT $2",
        vec![
            conversation_id.into(),
            ((MAX_INBOX_RECIPIENTS + 1) as i64).into(),
        ],
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %crate::db::summarize_db_error(&e), "inbox recipients unavailable");
            crate::metrics::inc("messaging_realtime_inbox_recipient_failures_total", &[]);
            return Vec::new();
        }
    };
    with_subject(rows.into_iter().map(|r| r.principal_id).collect(), subject)
}

/// Bounds the member list and adds the event's subject once.
pub fn with_subject(mut members: Vec<Uuid>, subject: Option<Uuid>) -> Vec<Uuid> {
    if members.len() > MAX_INBOX_RECIPIENTS {
        return Vec::new();
    }
    if let Some(s) = subject {
        if !members.contains(&s) {
            members.push(s);
        }
    }
    members
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
        let message = match broker_message(event) {
            Some(BrokerMessage::Durable {
                conversation_id,
                event_sequence,
                event_type,
                subject_principal_id,
                ..
            }) => Some(BrokerMessage::Durable {
                recipients: inbox_recipients(&self.state, conversation_id, subject_principal_id)
                    .await,
                conversation_id,
                event_sequence,
                event_type,
                subject_principal_id,
            }),
            other => other,
        };
        match message {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_recipients_are_bounded_and_include_the_subject_once() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert_eq!(with_subject(vec![a], Some(b)), vec![a, b]);
        assert_eq!(with_subject(vec![a, b], Some(b)), vec![a, b]);
        assert_eq!(with_subject(vec![a], None), vec![a]);
        let many = (0..=MAX_INBOX_RECIPIENTS as u128)
            .map(Uuid::from_u128)
            .collect();
        assert!(with_subject(many, Some(a)).is_empty());
    }
}

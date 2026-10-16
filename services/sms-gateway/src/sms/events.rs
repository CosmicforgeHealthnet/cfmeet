//! Message events, recorded in the transactional outbox with the state they describe and
//! published (platform envelope) to the platform's customer webhook delivery.
//!
//! Event types `sms-gateway.message.<requested|submitted|delivered|failed|unknown|blocked|
//! received>` become the public webhook events `sms.<…>`. The payload carries references,
//! the status, bounded codes and the MASKED remote party only — never content, a number,
//! a provider identifier or a grant (the database refuses such keys).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr};
use serde_json::json;
use uuid::Uuid;

pub use crate::infra::outbox::EventScope;
use crate::{
    domain::RequestContext,
    infra::outbox::{enqueue, NewEvent},
};

use super::TransportRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageEvent {
    Requested,
    Submitted,
    Delivered,
    Failed,
    Unknown,
    Blocked,
    Received,
}

impl MessageEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            MessageEvent::Requested => "sms-gateway.message.requested",
            MessageEvent::Submitted => "sms-gateway.message.submitted",
            MessageEvent::Delivered => "sms-gateway.message.delivered",
            MessageEvent::Failed => "sms-gateway.message.failed",
            MessageEvent::Unknown => "sms-gateway.message.unknown",
            MessageEvent::Blocked => "sms-gateway.message.blocked",
            MessageEvent::Received => "sms-gateway.message.received",
        }
    }
}

/// Records `event` for `row` (its CURRENT state) in the caller's transaction.
pub async fn record<C: ConnectionTrait>(
    db: &C,
    row: &TransportRow,
    event: MessageEvent,
    ctx: &RequestContext,
    now: DateTime<Utc>,
) -> Result<Uuid, DbErr> {
    let payload = json!({
        "message_id": row.id,
        "direction": row.direction,
        "status": row.status,
        "number_resource_id": row.number_resource_id,
        "thread_id": row.thread_id,
        "segment_count": row.segment_count,
        "encoding": row.content_encoding,
        "remote_masked": row.remote_masked,
        "principal_id": row.principal_id,
        "target_kind": row.target_kind,
        "target_ref": row.target_ref,
        "failure_code": row.failure_code,
        "blocked_reason": row.blocked_reason,
        "estimated_cost_minor": row.estimated_cost_minor,
        "confirmed_cost_minor": row.confirmed_cost_minor,
        "currency": row.cost_currency,
    });
    enqueue(
        db,
        NewEvent {
            event_type: event.event_type(),
            aggregate_type: "SMS_MESSAGE",
            aggregate_id: row.id,
            scope: Some(row.scope()),
            payload,
        },
        ctx,
        now,
    )
    .await
}

//! Durable publication of Privacy Relay domain events (Part G §67, ADR 0028).
//!
//! The authoritative transaction writes the platform envelope to `privacy_relay_outbox`
//! (see [`crate::audit`]). This publisher claims PENDING rows with `FOR UPDATE SKIP
//! LOCKED` + a lease (multi-instance safe; a crashed worker's claim expires), hands each
//! envelope to an [`EventPublisher`] (transport-neutral; the HTTP adapter delivers it to
//! Calling's authenticated platform-event ingress, which normalizes it for customer
//! webhooks), retries with bounded backoff and dead-letters after
//! [`OUTBOX_MAX_ATTEMPTS`]. Without a configured publisher rows simply stay PENDING
//! (authoritative, never lost).

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::Duration;
use serde_json::Value;
use uuid::Uuid;

use crate::{db, error::RelayResult, service::RelayService};

/// Calling's platform-event ingress.
pub const PLATFORM_EVENTS_PATH: &str = "/internal/v1/platform/events";
/// Publication attempts before an event is dead-lettered.
pub const OUTBOX_MAX_ATTEMPTS: i32 = 10;
const LEASE_SECONDS: i64 = 60;

/// The neutral publication boundary. `Err` is a bounded failure code.
#[async_trait]
pub trait EventPublisher: Send + Sync {
    fn available(&self) -> bool {
        true
    }

    async fn publish(&self, correlation_id: &str, envelope: &Value) -> Result<(), &'static str>;
}

/// No publisher configured: events stay PENDING.
pub struct NoEventPublisher;

#[async_trait]
impl EventPublisher for NoEventPublisher {
    fn available(&self) -> bool {
        false
    }

    async fn publish(&self, _: &str, _: &Value) -> Result<(), &'static str> {
        Err("PUBLISHER_NOT_CONFIGURED")
    }
}

/// Delivers envelopes to Calling (service token, the event's correlation id).
pub struct HttpEventPublisher {
    client: Arc<InternalServiceClient>,
}

impl HttpEventPublisher {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl EventPublisher for HttpEventPublisher {
    async fn publish(&self, correlation_id: &str, envelope: &Value) -> Result<(), &'static str> {
        let correlation = cf_service_auth::CorrelationId::parse(correlation_id)
            .unwrap_or_else(|_| cf_service_auth::CorrelationId::generate());
        let request = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation);
        self.client
            .post_json(&request, PLATFORM_EVENTS_PATH, envelope)
            .await
            .map(|_| ())
            .map_err(|e| match e {
                InternalCallError::UnknownTarget(_) => "PUBLISHER_NOT_CONFIGURED",
                InternalCallError::Timeout { .. } => "TIMEOUT",
                InternalCallError::Status { status, .. } if status < 500 => "REJECTED",
                _ => "UNAVAILABLE",
            })
    }
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct Claimed {
    id: Uuid,
    envelope: Value,
    attempt_count: i32,
}

fn backoff(attempt: i32) -> Duration {
    Duration::seconds(2_i64.saturating_pow(attempt.clamp(0, 9) as u32).min(300))
}

impl RelayService {
    /// One publication pass. Returns the number of events published.
    pub async fn publish_outbox(&self, limit: u64) -> RelayResult<usize> {
        if !self.events.available() {
            return Ok(0);
        }
        let now = self.now();
        let claimed: Vec<Claimed> = db::all(
            &self.db,
            "UPDATE privacy_relay_outbox o SET lease_until = $1, attempt_count = o.attempt_count + 1, \
               updated_at = $2 \
             WHERE o.id IN (SELECT id FROM privacy_relay_outbox \
                             WHERE status = 'PENDING' AND next_attempt_at <= $2 \
                               AND (lease_until IS NULL OR lease_until <= $2) \
                             ORDER BY next_attempt_at, created_at LIMIT $3 FOR UPDATE SKIP LOCKED) \
             RETURNING o.id, o.envelope, o.attempt_count",
            vec![
                (now + Duration::seconds(LEASE_SECONDS)).into(),
                now.into(),
                (limit as i64).into(),
            ],
        )
        .await?;
        let mut published = 0;
        for row in claimed {
            let correlation = row
                .envelope
                .get("correlation_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let outcome = self.events.publish(&correlation, &row.envelope).await;
            let now = self.now();
            match outcome {
                Ok(()) => {
                    db::exec(
                        &self.db,
                        "UPDATE privacy_relay_outbox SET status = 'PUBLISHED', published_at = $2, \
                           lease_until = NULL, last_error_code = NULL, updated_at = $2 \
                         WHERE id = $1 AND status = 'PENDING' AND attempt_count = $3",
                        vec![row.id.into(), now.into(), row.attempt_count.into()],
                    )
                    .await?;
                    published += 1;
                }
                Err(code) if row.attempt_count >= OUTBOX_MAX_ATTEMPTS => {
                    db::exec(
                        &self.db,
                        "UPDATE privacy_relay_outbox SET status = 'DEAD_LETTER', lease_until = NULL, \
                           last_error_code = $2, updated_at = $3 \
                         WHERE id = $1 AND status = 'PENDING' AND attempt_count = $4",
                        vec![row.id.into(), code.into(), now.into(), row.attempt_count.into()],
                    )
                    .await?;
                    tracing::error!(outbox_id = %row.id, code, "privacy relay event dead-lettered");
                }
                Err(code) => {
                    db::exec(
                        &self.db,
                        "UPDATE privacy_relay_outbox SET next_attempt_at = $2, lease_until = NULL, \
                           last_error_code = $3, updated_at = $4 \
                         WHERE id = $1 AND status = 'PENDING' AND attempt_count = $5",
                        vec![
                            row.id.into(),
                            (now + backoff(row.attempt_count)).into(),
                            code.into(),
                            now.into(),
                            row.attempt_count.into(),
                        ],
                    )
                    .await?;
                }
            }
        }
        Ok(published)
    }

    /// Operator replay of a dead-lettered event.
    pub async fn replay_dead_lettered_event(&self, event_id: &str) -> RelayResult<bool> {
        let now = self.now();
        Ok(db::exec(
            &self.db,
            "UPDATE privacy_relay_outbox SET status = 'PENDING', attempt_count = 0, next_attempt_at = $2, \
               lease_until = NULL, last_error_code = NULL, updated_at = $2 \
             WHERE event_id = $1 AND status = 'DEAD_LETTER'",
            vec![event_id.into(), now.into()],
        )
        .await?
            > 0)
    }
}

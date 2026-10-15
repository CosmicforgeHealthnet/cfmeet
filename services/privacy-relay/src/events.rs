//! Durable publication of Privacy Relay domain events (Part G §67, ADR 0028).
//!
//! The authoritative transaction writes the platform envelope to `privacy_relay_outbox`
//! (see [`crate::audit`]). This publisher claims PENDING rows with `FOR UPDATE SKIP
//! LOCKED` + a lease (multi-instance safe; a crashed worker's claim expires), hands each
//! envelope to an [`EventPublisher`] (transport-neutral; the HTTP adapter delivers it to
//! Calling's authenticated platform-event ingress, which normalizes it for customer
//! webhooks) and classifies every failure (§81):
//!
//! * PERMANENT ([`is_permanent`]: Calling refused the envelope as malformed or the call
//!   as forbidden, or the stored envelope itself is unusable) — dead-lettered
//!   immediately; retrying cannot succeed;
//! * TRANSIENT (timeouts, unavailability, throttling, authentication races) — retried
//!   with capped exponential backoff and dead-lettered after [`OUTBOX_MAX_ATTEMPTS`].
//!
//! Every dead letter raises (deduplicated) an `OUTBOX_DEAD_LETTER` incident. The
//! publication hop continues the correlation id the envelope was committed with (the
//! one of the inbound request that caused the event); an envelope without a valid one
//! is a permanent failure, never a freshly minted id. Without a configured publisher
//! rows simply stay PENDING (authoritative, never lost).

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::Duration;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    db,
    error::RelayResult,
    incidents::{self, IncidentCategory},
    metrics,
    service::RelayService,
};

/// Calling's platform-event ingress.
pub const PLATFORM_EVENTS_PATH: &str = "/internal/v1/platform/events";
/// Publication attempts before an event is dead-lettered.
pub const OUTBOX_MAX_ATTEMPTS: i32 = 10;
const LEASE_SECONDS: i64 = 60;

/// Failure codes that no retry can fix (dead-lettered on the first occurrence).
pub const PERMANENT_PUBLISH_FAILURES: &[&str] = &["MALFORMED", "FORBIDDEN", "MALFORMED_ENVELOPE"];

/// Whether a bounded publication failure code is permanent.
pub fn is_permanent(code: &str) -> bool {
    PERMANENT_PUBLISH_FAILURES.contains(&code)
}

/// The bounded code of an HTTP publication failure (`MALFORMED` / `FORBIDDEN` are
/// permanent; everything else is retried).
pub fn classify_http_failure(error: &InternalCallError) -> &'static str {
    match error {
        InternalCallError::UnknownTarget(_) => "PUBLISHER_NOT_CONFIGURED",
        InternalCallError::Timeout { .. } => "TIMEOUT",
        InternalCallError::Status { status, .. } => classify_status(*status),
        _ => "UNAVAILABLE",
    }
}

/// HTTP status -> bounded failure code.
pub fn classify_status(status: u16) -> &'static str {
    match status {
        400 | 413 | 415 | 422 => "MALFORMED",
        403 => "FORBIDDEN",
        401 => "UNAUTHENTICATED",
        408 => "TIMEOUT",
        429 => "THROTTLED",
        s if s >= 500 => "UNAVAILABLE",
        _ => "REJECTED",
    }
}

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
        // The hop continues the committed correlation id; never a fresh one.
        let correlation = cf_service_auth::CorrelationId::parse(correlation_id)
            .map_err(|_| "MALFORMED_ENVELOPE")?;
        let request = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation);
        self.client
            .post_json(&request, PLATFORM_EVENTS_PATH, envelope)
            .await
            .map(|_| ())
            .map_err(|e| classify_http_failure(&e))
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
                .filter(|c| cf_service_auth::CorrelationId::parse(c).is_ok())
                .map(str::to_string);
            let outcome = match &correlation {
                Some(correlation) => self.events.publish(correlation, &row.envelope).await,
                // A stored envelope without its correlation id cannot be published
                // faithfully: permanent.
                None => Err("MALFORMED_ENVELOPE"),
            };
            let now = self.now();
            match outcome {
                Ok(()) => {
                    let updated = db::exec(
                        &self.db,
                        "UPDATE privacy_relay_outbox SET status = 'PUBLISHED', published_at = $2, \
                           lease_until = NULL, last_error_code = NULL, updated_at = $2 \
                         WHERE id = $1 AND status = 'PENDING' AND attempt_count = $3",
                        vec![row.id.into(), now.into(), row.attempt_count.into()],
                    )
                    .await?;
                    if updated == 1 {
                        published += 1;
                        metrics::inc("privacy_relay_outbox_published_total", &[]);
                    }
                }
                Err(code) if is_permanent(code) || row.attempt_count >= OUTBOX_MAX_ATTEMPTS => {
                    let kind = if is_permanent(code) {
                        "PERMANENT"
                    } else {
                        "EXHAUSTED"
                    };
                    let updated = db::exec(
                        &self.db,
                        "UPDATE privacy_relay_outbox SET status = 'DEAD_LETTER', lease_until = NULL, \
                           last_error_code = $2, updated_at = $3 \
                         WHERE id = $1 AND status = 'PENDING' AND attempt_count = $4",
                        vec![row.id.into(), code.into(), now.into(), row.attempt_count.into()],
                    )
                    .await?;
                    if updated == 1 {
                        metrics::inc(
                            "privacy_relay_outbox_dead_lettered_total",
                            &[("reason", code), ("kind", kind)],
                        );
                        incidents::raise_best_effort(
                            &self.db,
                            IncidentCategory::OutboxDeadLetter,
                            &incidents::dedupe_key(IncidentCategory::OutboxDeadLetter, &[code]),
                            now,
                        )
                        .await;
                        tracing::error!(outbox_id = %row.id, code, kind, "privacy relay event dead-lettered");
                    }
                }
                Err(code) => {
                    metrics::inc("privacy_relay_outbox_retries_total", &[("reason", code)]);
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

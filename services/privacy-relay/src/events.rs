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
//!
//! Part VI §53 — receivers. [`PublicEventsDestination`]
//! (`PRIVACY_RELAY_PUBLIC_EVENTS_DESTINATION`) chooses where envelopes go: `calling`
//! (default, today's behaviour), `developer-platform` (the central event ingestion, the
//! SAME path and envelope, `developer-event.publish`) or `both` (dual publication for
//! shadow comparison). Each receiver's outcome (`calling_outcome` /
//! `developer_platform_outcome`: `ACCEPTED` or `REJECTED`) is recorded on the row with the
//! attempt, so a retry re-sends only to a receiver that has not settled; the row is
//! PUBLISHED once every receiver accepted it and DEAD_LETTER once every receiver settled
//! with a permanent refusal (or the attempts are exhausted). The classification above
//! applies per receiver; a receiver without a configured publisher is not attempted and
//! the row waits for it without spending an attempt.

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

    pub fn service(self) -> ServiceId {
        match self {
            EventDestination::Calling => ServiceId::calling(),
            EventDestination::DeveloperPlatform => ServiceId::developer_platform(),
        }
    }

    /// The recorded failure code of this receiver (Calling's unchanged).
    fn code(self, code: &'static str) -> String {
        match self {
            EventDestination::Calling => code.to_string(),
            EventDestination::DeveloperPlatform => format!("DEVELOPER_PLATFORM_{code}"),
        }
    }
}

/// `PRIVACY_RELAY_PUBLIC_EVENTS_DESTINATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublicEventsDestination {
    /// Calling only (default: today's behaviour, unchanged).
    #[default]
    Calling,
    /// The Developer Platform only.
    DeveloperPlatform,
    /// Both (shadow comparison before the cutover).
    Both,
}

impl PublicEventsDestination {
    pub const VARIABLE: &'static str = "PRIVACY_RELAY_PUBLIC_EVENTS_DESTINATION";

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

/// A receiver accepted the envelope (never re-sent there).
pub const OUTCOME_ACCEPTED: &str = "ACCEPTED";
/// A receiver refused the envelope for good (never re-sent there).
pub const OUTCOME_REJECTED: &str = "REJECTED";

/// Delivers envelopes to one receiver (service token, the event's correlation id).
pub struct HttpEventPublisher {
    client: Arc<InternalServiceClient>,
    target: ServiceId,
}

impl HttpEventPublisher {
    /// Calling's ingress (`platform-event.publish`).
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self::to(client, EventDestination::Calling)
    }

    /// The ingress of `destination` (`developer-event.publish` on the Developer Platform).
    pub fn to(client: Arc<InternalServiceClient>, destination: EventDestination) -> Self {
        Self {
            client,
            target: destination.service(),
        }
    }
}

#[async_trait]
impl EventPublisher for HttpEventPublisher {
    async fn publish(&self, correlation_id: &str, envelope: &Value) -> Result<(), &'static str> {
        // The hop continues the committed correlation id; never a fresh one.
        let correlation = cf_service_auth::CorrelationId::parse(correlation_id)
            .map_err(|_| "MALFORMED_ENVELOPE")?;
        let request = OutboundServiceRequest::continuing(self.target.clone(), &correlation);
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
    calling_outcome: Option<String>,
    developer_platform_outcome: Option<String>,
}

fn backoff(attempt: i32) -> Duration {
    Duration::seconds(2_i64.saturating_pow(attempt.clamp(0, 9) as u32).min(300))
}

impl RelayService {
    /// The publisher of one receiver.
    fn event_publisher(&self, destination: EventDestination) -> &Arc<dyn EventPublisher> {
        match destination {
            EventDestination::Calling => &self.events,
            EventDestination::DeveloperPlatform => &self.developer_events,
        }
    }

    /// One publication pass. Returns the number of events published.
    pub async fn publish_outbox(&self, limit: u64) -> RelayResult<usize> {
        let destinations = self.public_events.destinations();
        if !destinations
            .iter()
            .any(|d| self.event_publisher(*d).available())
        {
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
             RETURNING o.id, o.envelope, o.attempt_count, o.calling_outcome, o.developer_platform_outcome",
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
            let mut calling = row.calling_outcome.clone();
            let mut developer_platform = row.developer_platform_outcome.clone();
            // (recorded code, bounded reason) of this pass's first refusal / transient failure.
            let mut refusal: Option<(String, &'static str)> = None;
            let mut transient: Option<(String, &'static str)> = None;
            let mut waiting_for_configuration = false;
            for destination in destinations {
                let slot = match destination {
                    EventDestination::Calling => &mut calling,
                    EventDestination::DeveloperPlatform => &mut developer_platform,
                };
                if slot.is_some() {
                    continue;
                }
                let publisher = self.event_publisher(*destination);
                if !publisher.available() {
                    waiting_for_configuration = true;
                    continue;
                }
                let outcome = match &correlation {
                    Some(correlation) => publisher.publish(correlation, &row.envelope).await,
                    // A stored envelope without its correlation id cannot be published
                    // faithfully: permanent.
                    None => Err("MALFORMED_ENVELOPE"),
                };
                match outcome {
                    Ok(()) => *slot = Some(OUTCOME_ACCEPTED.to_string()),
                    Err(code) if is_permanent(code) => {
                        *slot = Some(OUTCOME_REJECTED.to_string());
                        refusal.get_or_insert((destination.code(code), code));
                    }
                    Err(code) => {
                        transient.get_or_insert((destination.code(code), code));
                    }
                }
            }
            let settled = |d: &EventDestination| match d {
                EventDestination::Calling => calling.as_deref(),
                EventDestination::DeveloperPlatform => developer_platform.as_deref(),
            };
            let all_settled = destinations.iter().all(|d| settled(d).is_some());
            let any_refused = destinations
                .iter()
                .any(|d| settled(d) == Some(OUTCOME_REJECTED));
            debug_assert!(all_settled || transient.is_some() || waiting_for_configuration);
            let now = self.now();
            if all_settled && !any_refused {
                let updated = db::exec(
                    &self.db,
                    "UPDATE privacy_relay_outbox SET status = 'PUBLISHED', published_at = $2, \
                       lease_until = NULL, last_error_code = NULL, updated_at = $2, \
                       calling_outcome = $4, developer_platform_outcome = $5 \
                     WHERE id = $1 AND status = 'PENDING' AND attempt_count = $3",
                    vec![
                        row.id.into(),
                        now.into(),
                        row.attempt_count.into(),
                        calling.into(),
                        developer_platform.into(),
                    ],
                )
                .await?;
                if updated == 1 {
                    published += 1;
                    metrics::inc("privacy_relay_outbox_published_total", &[]);
                }
            } else if all_settled
                || (transient.is_some() && row.attempt_count >= OUTBOX_MAX_ATTEMPTS)
            {
                let (code, reason, kind) = match (&refusal, &transient, all_settled) {
                    (Some((code, reason)), _, true) => (code.clone(), *reason, "PERMANENT"),
                    // Refused by a receiver in an earlier pass.
                    (None, _, true) => ("REJECTED".to_string(), "REJECTED", "PERMANENT"),
                    (_, Some((code, reason)), false) => (code.clone(), *reason, "EXHAUSTED"),
                    (_, None, false) => ("UNAVAILABLE".to_string(), "UNAVAILABLE", "EXHAUSTED"),
                };
                let updated = db::exec(
                    &self.db,
                    "UPDATE privacy_relay_outbox SET status = 'DEAD_LETTER', lease_until = NULL, \
                       last_error_code = $2, updated_at = $3, \
                       calling_outcome = $5, developer_platform_outcome = $6 \
                     WHERE id = $1 AND status = 'PENDING' AND attempt_count = $4",
                    vec![
                        row.id.into(),
                        code.clone().into(),
                        now.into(),
                        row.attempt_count.into(),
                        calling.into(),
                        developer_platform.into(),
                    ],
                )
                .await?;
                if updated == 1 {
                    metrics::inc(
                        "privacy_relay_outbox_dead_lettered_total",
                        &[("reason", reason), ("kind", kind)],
                    );
                    incidents::raise_best_effort(
                        &self.db,
                        IncidentCategory::OutboxDeadLetter,
                        &incidents::dedupe_key(
                            IncidentCategory::OutboxDeadLetter,
                            &[code.as_str()],
                        ),
                        now,
                    )
                    .await;
                    tracing::error!(outbox_id = %row.id, code = %code, kind, "privacy relay event dead-lettered");
                }
            } else if let Some((code, reason)) = transient {
                metrics::inc("privacy_relay_outbox_retries_total", &[("reason", reason)]);
                db::exec(
                    &self.db,
                    "UPDATE privacy_relay_outbox SET next_attempt_at = $2, lease_until = NULL, \
                       last_error_code = $3, updated_at = $4, \
                       calling_outcome = $6, developer_platform_outcome = $7 \
                     WHERE id = $1 AND status = 'PENDING' AND attempt_count = $5",
                    vec![
                        row.id.into(),
                        (now + backoff(row.attempt_count)).into(),
                        code.into(),
                        now.into(),
                        row.attempt_count.into(),
                        calling.into(),
                        developer_platform.into(),
                    ],
                )
                .await?;
            } else {
                // Only receivers without a configured publisher remain (any refusal of
                // this pass is recorded): the row waits for configuration, and the claim
                // does not count as an attempt.
                db::exec(
                    &self.db,
                    "UPDATE privacy_relay_outbox SET next_attempt_at = $2, lease_until = NULL, \
                       attempt_count = attempt_count - 1, last_error_code = 'PUBLISHER_NOT_CONFIGURED', \
                       updated_at = $3, calling_outcome = $5, developer_platform_outcome = $6 \
                     WHERE id = $1 AND status = 'PENDING' AND attempt_count = $4",
                    vec![
                        row.id.into(),
                        (now + Duration::seconds(LEASE_SECONDS)).into(),
                        now.into(),
                        row.attempt_count.into(),
                        calling.into(),
                        developer_platform.into(),
                    ],
                )
                .await?;
            }
        }
        Ok(published)
    }

    /// Operator replay of a dead-lettered event (re-sent to every receiver that has not
    /// accepted it; a receiver's earlier refusal is forgotten).
    pub async fn replay_dead_lettered_event(&self, event_id: &str) -> RelayResult<bool> {
        let now = self.now();
        Ok(db::exec(
            &self.db,
            "UPDATE privacy_relay_outbox SET status = 'PENDING', attempt_count = 0, next_attempt_at = $2, \
               lease_until = NULL, last_error_code = NULL, updated_at = $2, \
               calling_outcome = NULLIF(calling_outcome, 'REJECTED'), \
               developer_platform_outcome = NULLIF(developer_platform_outcome, 'REJECTED') \
             WHERE event_id = $1 AND status = 'DEAD_LETTER'",
            vec![event_id.into(), now.into()],
        )
        .await?
            > 0)
    }
}

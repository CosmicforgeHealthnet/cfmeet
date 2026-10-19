//! Part V I §§116–118 — customer webhooks via the existing platform webhook system.
//!
//! Messaging owns no webhook subscriptions, secrets, signatures or deliveries. The outbox
//! trigger of migration 12 queues the public-worthy events in `messaging_webhook_relay`
//! (same transaction as the event); this worker relays each one, as its unchanged
//! platform envelope, to Calling's `POST /internal/v1/platform/events`
//! (`platform-event.publish`, granted to Messaging only for its own events). Calling then
//! projects it through an allow-list (ids, sequences, codes), signs it with the platform
//! webhook signature and delivers / retries / replays it like every other platform event.
//!
//! Privacy: outbox payloads hold ids, states and versions only (enforced by the
//! database), so no message content of any security profile is ever relayed; the relay
//! adds only the conversation id an event belongs to. Unconfigured
//! (`CF_CALLING_BASE_URL` unset): rows stay PENDING (`NOT_CONFIGURED`), nothing is lost.

use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::Duration;
use sea_orm::TransactionTrait;
use serde_json::{json, Value};

use crate::{db, error::Result, events::outbox::OutboxEvent, state::AppState};

/// Relay attempts before an event is dead-lettered (visible, never silently dropped).
pub const MAX_ATTEMPTS: i32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayFailure {
    /// Retry with backoff (timeout, 5xx, 429, unreachable).
    Unavailable,
    /// The ingress refused the envelope for good (contract violation): dead letter.
    Rejected,
    /// No Calling target configured: stays PENDING.
    NotConfigured,
}

impl RelayFailure {
    fn code(self) -> &'static str {
        match self {
            RelayFailure::Unavailable => "UNAVAILABLE",
            RelayFailure::Rejected => "REJECTED",
            RelayFailure::NotConfigured => "NOT_CONFIGURED",
        }
    }
}

/// Where relayed envelopes go (Calling's platform-event ingress).
#[async_trait]
pub trait PlatformEventSink: Send + Sync {
    async fn publish(&self, envelope: &Value) -> std::result::Result<(), RelayFailure>;
}

/// The service-authenticated Calling ingress.
pub struct CallingPlatformEvents {
    client: Arc<InternalServiceClient>,
}

impl CallingPlatformEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl PlatformEventSink for CallingPlatformEvents {
    async fn publish(&self, envelope: &Value) -> std::result::Result<(), RelayFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::calling());
        match self
            .client
            .exchange_json(
                &request,
                Method::POST,
                "/internal/v1/platform/events",
                Some(envelope),
            )
            .await
        {
            Ok(r) if (200..300).contains(&r.status()) => Ok(()),
            Ok(r) if r.status() >= 500 || r.status() == 429 || r.status() == 401 => {
                Err(RelayFailure::Unavailable)
            }
            Ok(_) => Err(RelayFailure::Rejected),
            Err(InternalCallError::UnknownTarget(_)) => Err(RelayFailure::NotConfigured),
            Err(_) => Err(RelayFailure::Unavailable),
        }
    }
}

/// No Calling target: every event stays PENDING.
pub struct UnconfiguredSink;

#[async_trait]
impl PlatformEventSink for UnconfiguredSink {
    async fn publish(&self, _: &Value) -> std::result::Result<(), RelayFailure> {
        Err(RelayFailure::NotConfigured)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayReport {
    pub relayed: usize,
    pub retried: usize,
    pub dead_lettered: usize,
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 10)).min(900))
}

/// The envelope relayed for `event`: the platform envelope, plus the conversation id an
/// event of a conversation belongs to (ids only).
pub fn envelope(event: &OutboxEvent, runtime_environment: &str) -> Value {
    let mut envelope = event.envelope(runtime_environment);
    if let (Some(c), Some(p)) = (event.conversation_id, envelope["payload"].as_object_mut()) {
        p.entry("conversation_id").or_insert(json!(c));
    }
    envelope
}

/// One pass over due relay rows (bounded). Multi-instance safe: each row is row-locked
/// (`FOR UPDATE SKIP LOCKED`) for its attempt and the outcome is written in the same
/// transaction, so a crash mid-attempt leaves it PENDING (at-least-once; Calling
/// de-duplicates on the event id).
pub async fn relay_due(
    state: &AppState,
    sink: &dyn PlatformEventSink,
    limit: usize,
) -> Result<RelayReport> {
    let mut report = RelayReport::default();
    for _ in 0..limit {
        let txn = state.db.begin().await?;
        let now = state.now();
        let event: Option<OutboxEvent> = db::one(
            &txn,
            "SELECT o.id, o.sequence, o.event_type, o.schema_version, o.aggregate_type, o.aggregate_id, \
                    o.tenant_id, o.application_id, o.environment_id, o.correlation_id, o.causation_id, \
                    o.payload, o.occurred_at, r.attempts, o.conversation_id, o.conversation_event_sequence \
               FROM messaging_webhook_relay r JOIN messaging_outbox_events o ON o.id = r.event_id \
              WHERE r.status = 'PENDING' AND r.next_attempt_at <= $1 \
              ORDER BY r.next_attempt_at, o.sequence LIMIT 1 FOR UPDATE OF r SKIP LOCKED",
            vec![now.into()],
        )
        .await?;
        let Some(event) = event else {
            txn.rollback().await?;
            break;
        };
        let attempts = event.attempts + 1;
        let outcome = sink
            .publish(&envelope(&event, &state.runtime_environment().to_string()))
            .await;
        match outcome {
            Ok(()) => {
                db::exec(
                    &txn,
                    "UPDATE messaging_webhook_relay SET status = 'RELAYED', attempts = $2, relayed_at = $3, \
                       last_error = NULL WHERE event_id = $1",
                    vec![event.id.into(), attempts.into(), now.into()],
                )
                .await?;
                report.relayed += 1;
                crate::metrics::inc("messaging_webhook_relay_total", &[("outcome", "relayed")]);
            }
            Err(f) => {
                let dead = f == RelayFailure::Rejected
                    || (f == RelayFailure::Unavailable && attempts >= MAX_ATTEMPTS);
                // Not configured is not an attempt: the row waits for configuration.
                let counted = if f == RelayFailure::NotConfigured {
                    event.attempts
                } else {
                    attempts
                };
                db::exec(
                    &txn,
                    "UPDATE messaging_webhook_relay SET status = $2, attempts = $3, last_error = $4, \
                       next_attempt_at = $5 WHERE event_id = $1",
                    vec![
                        event.id.into(),
                        if dead { "DEAD_LETTER" } else { "PENDING" }.into(),
                        counted.into(),
                        f.code().into(),
                        (now + backoff(counted)).into(),
                    ],
                )
                .await?;
                if dead {
                    report.dead_lettered += 1;
                    crate::metrics::inc(
                        "messaging_webhook_relay_total",
                        &[("outcome", "dead_letter")],
                    );
                } else {
                    report.retried += 1;
                    crate::metrics::inc("messaging_webhook_relay_total", &[("outcome", "retried")]);
                }
            }
        }
        txn.commit().await?;
        if outcome.is_err() {
            // Back off the pass: the sink is down or unconfigured.
            break;
        }
    }
    Ok(report)
}

/// The background relay loop.
pub async fn run(state: AppState, sink: Arc<dyn PlatformEventSink>, interval: StdDuration) {
    loop {
        match relay_due(&state, sink.as_ref(), 200).await {
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "webhook relay pass failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

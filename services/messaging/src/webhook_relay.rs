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
//!
//! Part VI §53 — receivers. [`PublicEventsDestination`] (`MESSAGING_PUBLIC_EVENTS_DESTINATION`)
//! chooses where the SAME envelope goes: `calling` (default, today's behaviour),
//! `developer-platform` (the central event ingestion, same path, `developer-event.publish`;
//! it de-duplicates on (producer, `event_id`)) or `both` (dual publication for shadow
//! comparison). Each receiver's outcome (`calling_outcome` / `developer_platform_outcome`:
//! `ACCEPTED` or `REJECTED`) is written with the attempt, so a retry re-sends only to a
//! receiver that has not settled: the row is RELAYED once every receiver accepted it and
//! DEAD_LETTER once every receiver settled with a refusal (or the attempts are exhausted).
//! Failure codes of the Developer Platform are prefixed `DEVELOPER_PLATFORM_`.

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

/// The Developer Platform's service-authenticated central event ingestion (Part VI §53):
/// same path, same envelope, `developer-event.publish`. The hop continues the event's
/// committed correlation id.
pub struct DeveloperPlatformEvents {
    client: Arc<InternalServiceClient>,
}

impl DeveloperPlatformEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl PlatformEventSink for DeveloperPlatformEvents {
    async fn publish(&self, envelope: &Value) -> std::result::Result<(), RelayFailure> {
        let request = match envelope["correlation_id"]
            .as_str()
            .and_then(|c| cf_service_auth::CorrelationId::parse(c).ok())
        {
            Some(c) => OutboundServiceRequest::continuing(ServiceId::developer_platform(), &c),
            None => OutboundServiceRequest::new_operation(ServiceId::developer_platform()),
        };
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

/// One receiver of relayed events.
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

    /// The recorded failure code of this receiver (Calling's unchanged).
    fn code(self, failure: RelayFailure) -> &'static str {
        match (self, failure) {
            (EventDestination::Calling, f) => f.code(),
            (EventDestination::DeveloperPlatform, RelayFailure::Unavailable) => {
                "DEVELOPER_PLATFORM_UNAVAILABLE"
            }
            (EventDestination::DeveloperPlatform, RelayFailure::Rejected) => {
                "DEVELOPER_PLATFORM_REJECTED"
            }
            (EventDestination::DeveloperPlatform, RelayFailure::NotConfigured) => {
                "DEVELOPER_PLATFORM_NOT_CONFIGURED"
            }
        }
    }
}

/// `MESSAGING_PUBLIC_EVENTS_DESTINATION`.
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
    pub const VARIABLE: &'static str = "MESSAGING_PUBLIC_EVENTS_DESTINATION";

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

    /// The receivers, in relay order.
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

/// The receivers' recorded outcomes of one relay row.
#[derive(Debug, Default, sea_orm::FromQueryResult)]
struct Outcomes {
    calling_outcome: Option<String>,
    developer_platform_outcome: Option<String>,
}

/// A receiver accepted the envelope (never re-sent there).
pub const OUTCOME_ACCEPTED: &str = "ACCEPTED";
/// A receiver refused the envelope for good (never re-sent there).
pub const OUTCOME_REJECTED: &str = "REJECTED";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayReport {
    pub relayed: usize,
    pub retried: usize,
    pub dead_lettered: usize,
    /// Rows deferred while the WEBHOOK_RELAY kill switch is engaged.
    pub paused: usize,
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

/// One pass over due relay rows to Calling only (today's single receiver).
pub async fn relay_due(
    state: &AppState,
    sink: &dyn PlatformEventSink,
    limit: usize,
) -> Result<RelayReport> {
    relay_due_to(state, &[(EventDestination::Calling, sink)], limit).await
}

/// One pass over due relay rows (bounded), to every listed receiver. Multi-instance safe:
/// each row is row-locked (`FOR UPDATE SKIP LOCKED`) for its attempt and the outcomes are
/// written in the same transaction, so a crash mid-attempt leaves it PENDING
/// (at-least-once; both receivers de-duplicate on the event id). A receiver that already
/// settled the row (accepted / refused) is never sent it again.
pub async fn relay_due_to(
    state: &AppState,
    sinks: &[(EventDestination, &dyn PlatformEventSink)],
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
        // Kill switch: relayed events wait PENDING (not an attempt), nothing is lost.
        if crate::operations::engaged(
            &txn,
            crate::operations::KillSwitch::WebhookRelay,
            Some(event.environment_id),
        )
        .await?
        {
            db::exec(
                &txn,
                "UPDATE messaging_webhook_relay SET next_attempt_at = $2, last_error = 'KILL_SWITCH_ENGAGED' WHERE event_id = $1",
                vec![event.id.into(), (now + Duration::seconds(60)).into()],
            )
            .await?;
            txn.commit().await?;
            report.paused += 1;
            continue;
        }
        let settled: Option<Outcomes> = db::one(
            &txn,
            "SELECT calling_outcome, developer_platform_outcome FROM messaging_webhook_relay WHERE event_id = $1",
            vec![event.id.into()],
        )
        .await?;
        let Outcomes {
            calling_outcome: mut calling,
            developer_platform_outcome: mut developer_platform,
        } = settled.unwrap_or_default();
        let attempts = event.attempts + 1;
        let body = envelope(&event, &state.runtime_environment().to_string());
        // The first transient / unconfigured failure and the first refusal of this pass.
        let mut failure: Option<(RelayFailure, &'static str)> = None;
        let mut refusal: Option<&'static str> = None;
        for (destination, sink) in sinks {
            let slot = match destination {
                EventDestination::Calling => &mut calling,
                EventDestination::DeveloperPlatform => &mut developer_platform,
            };
            if slot.is_some() {
                continue;
            }
            match sink.publish(&body).await {
                Ok(()) => *slot = Some(OUTCOME_ACCEPTED.to_string()),
                Err(RelayFailure::Rejected) => {
                    *slot = Some(OUTCOME_REJECTED.to_string());
                    refusal.get_or_insert(destination.code(RelayFailure::Rejected));
                }
                Err(f) => {
                    // A transient failure outranks "not configured" (it counts).
                    let code = destination.code(f);
                    match failure {
                        Some((RelayFailure::Unavailable, _)) => {}
                        _ if f == RelayFailure::Unavailable || failure.is_none() => {
                            failure = Some((f, code))
                        }
                        _ => {}
                    }
                }
            }
        }
        let outcome_of = |d: &EventDestination| match d {
            EventDestination::Calling => calling.as_deref(),
            EventDestination::DeveloperPlatform => developer_platform.as_deref(),
        };
        let all_settled = sinks.iter().all(|(d, _)| outcome_of(d).is_some());
        let any_refused = sinks
            .iter()
            .any(|(d, _)| outcome_of(d) == Some(OUTCOME_REJECTED));
        // (status, attempts, last error, next attempt) of the row.
        let (status, counted, code, next): (&str, i32, Option<&str>, _) =
            if all_settled && !any_refused {
                ("RELAYED", attempts, None, now)
            } else if all_settled {
                (
                    "DEAD_LETTER",
                    attempts,
                    Some(refusal.unwrap_or(RelayFailure::Rejected.code())),
                    now,
                )
            } else {
                match failure {
                    Some((RelayFailure::Unavailable, code)) => (
                        if attempts >= MAX_ATTEMPTS {
                            "DEAD_LETTER"
                        } else {
                            "PENDING"
                        },
                        attempts,
                        Some(code),
                        now + backoff(attempts),
                    ),
                    // Not configured is not an attempt: the row waits for configuration.
                    Some((_, code)) => (
                        "PENDING",
                        event.attempts,
                        Some(code),
                        now + backoff(event.attempts),
                    ),
                    None => (
                        "PENDING",
                        event.attempts,
                        Some(RelayFailure::NotConfigured.code()),
                        now + backoff(event.attempts),
                    ),
                }
            };
        db::exec(
            &txn,
            "UPDATE messaging_webhook_relay SET status = $2, attempts = $3, last_error = $4, \
               next_attempt_at = CASE WHEN $2 = 'PENDING' THEN $5 ELSE next_attempt_at END, \
               relayed_at = CASE WHEN $2 = 'RELAYED' THEN $6 ELSE relayed_at END, \
               calling_outcome = $7, developer_platform_outcome = $8 WHERE event_id = $1",
            vec![
                event.id.into(),
                status.into(),
                counted.into(),
                code.map(str::to_string).into(),
                next.into(),
                now.into(),
                calling.into(),
                developer_platform.into(),
            ],
        )
        .await?;
        let label = match status {
            "RELAYED" => {
                report.relayed += 1;
                "relayed"
            }
            "DEAD_LETTER" => {
                report.dead_lettered += 1;
                "dead_letter"
            }
            _ => {
                report.retried += 1;
                "retried"
            }
        };
        crate::metrics::inc("messaging_webhook_relay_total", &[("outcome", label)]);
        txn.commit().await?;
        if status == "DEAD_LETTER" && any_refused {
            crate::operations::raise(
                &state.db,
                crate::operations::IncidentCategory::WebhookRelayDeadLetter,
                &event.event_id(),
                now,
            )
            .await;
        }
        if failure.is_some() || refusal.is_some() {
            // Back off the pass: a receiver is down, refusing or unconfigured.
            break;
        }
    }
    Ok(report)
}

/// The background relay loop.
pub async fn run(
    state: AppState,
    sinks: Vec<(EventDestination, Arc<dyn PlatformEventSink>)>,
    interval: StdDuration,
) {
    let sinks: Vec<(EventDestination, &dyn PlatformEventSink)> =
        sinks.iter().map(|(d, s)| (*d, s.as_ref())).collect();
    loop {
        match relay_due_to(&state, &sinks, 200).await {
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "webhook relay pass failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

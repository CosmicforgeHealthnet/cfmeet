//! Transactional outbox.
//!
//! [`enqueue`] writes an event with the SAME connection / transaction as the state change
//! it describes, so either both commit or neither does. [`OutboxPublisher`] then delivers
//! committed events through an [`EventTransport`]:
//!
//! * events are claimed with a lease and a fresh claim token (`FOR UPDATE SKIP LOCKED`),
//!   so concurrent publishers never deliver the same claim twice; a publisher that dies
//!   loses its lease and another instance takes over after `lease_expires_at`;
//! * events of one aggregate are delivered in commit order: an event is not claimed while
//!   an earlier event of its aggregate is still pending;
//! * every completion is guarded by the claim token (a publisher whose lease expired
//!   cannot overwrite a newer claim);
//! * failures back off exponentially (bounded) and exhausted or permanently refused
//!   events become DEAD_LETTER — visible, never silently dropped.
//!
//! Delivery is at-least-once: a crash between delivery and completion re-delivers the
//! same `event_id`, which consumers de-duplicate. An uncertain delivery outcome is
//! therefore safe to retry for events (unlike provider operations, see [`super::jobs`]).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use super::platform_events::EventDestination;
use super::{bounded_error, RetryPolicy};
use crate::domain::{RequestContext, SharedClock};

/// Owning customer scope of an event subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

/// An event to record with a state change.
#[derive(Debug, Clone)]
pub struct NewEvent {
    /// `sms-gateway.<aggregate>.<change>` (contract `cfmeet.sms-gateway.event`).
    pub event_type: &'static str,
    pub aggregate_type: &'static str,
    pub aggregate_id: Uuid,
    pub scope: Option<EventScope>,
    /// snake_case keys; never content, a number or a secret (enforced by the database).
    pub payload: Value,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// The recorded error of a receiver: Calling's as-is (today's codes), the Developer
/// Platform's prefixed so the two are told apart.
fn destination_error(destination: EventDestination, reason: &str) -> String {
    match destination {
        EventDestination::Calling => reason.to_string(),
        other => format!("{}: {reason}", other.as_str()),
    }
}

/// Records `event` in the caller's transaction. Returns the event id.
pub async fn enqueue<C: ConnectionTrait>(
    db: &C,
    event: NewEvent,
    ctx: &RequestContext,
    now: DateTime<Utc>,
) -> Result<Uuid, DbErr> {
    let id = Uuid::new_v4();
    db.execute(stmt(
        "INSERT INTO sms_outbox_events (id, event_type, aggregate_type, aggregate_id, tenant_id, \
           application_id, environment_id, correlation_id, causation_id, payload, occurred_at, next_attempt_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
        vec![
            id.into(),
            event.event_type.into(),
            event.aggregate_type.into(),
            event.aggregate_id.into(),
            event.scope.map(|s| s.tenant_id).into(),
            event.scope.map(|s| s.application_id).into(),
            event.scope.map(|s| s.environment_id).into(),
            ctx.correlation_id.as_str().into(),
            ctx.cause().as_str().into(),
            event.payload.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(id)
}

/// A claimed, committed event.
#[derive(Debug, Clone, FromQueryResult)]
pub struct OutboxEvent {
    pub id: Uuid,
    pub sequence: i64,
    pub event_type: String,
    pub schema_version: i16,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
    pub attempts: i32,
    /// Durable outcome at Calling (`ACCEPTED` / `REJECTED`; `None`: not settled yet).
    pub calling_outcome: Option<String>,
    /// Durable outcome at the Developer Platform (same vocabulary).
    pub developer_platform_outcome: Option<String>,
}

/// How one delivery completes: status, last error, next attempt, published at, metric
/// label.
type Completion = (
    &'static str,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    &'static str,
);

/// A receiver accepted the event (it is never re-sent there).
pub const OUTCOME_ACCEPTED: &str = "ACCEPTED";
/// A receiver refused the event for good (never re-sent there; the event dead-letters
/// once every receiver has settled).
pub const OUTCOME_REJECTED: &str = "REJECTED";

impl OutboxEvent {
    pub fn event_id(&self) -> String {
        format!("evt_{}", self.id.simple())
    }

    /// The settled outcome of `destination`, if any.
    pub fn outcome(&self, destination: EventDestination) -> Option<&str> {
        match destination {
            EventDestination::Calling => self.calling_outcome.as_deref(),
            EventDestination::DeveloperPlatform => self.developer_platform_outcome.as_deref(),
        }
    }

    /// The `cfmeet.platform.event-envelope` of this event. The causation id is carried
    /// only when the cause was itself an event (the envelope's definition).
    pub fn envelope(&self, runtime_environment: &str) -> Value {
        let mut envelope = json!({
            "event_id": self.event_id(),
            "event_type": self.event_type,
            "schema_version": self.schema_version,
            "occurred_at": self.occurred_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            "producer_service": cf_service_auth::services::SMS_GATEWAY,
            "runtime_environment": runtime_environment,
            "correlation_id": self.correlation_id,
            "payload": self.payload,
        });
        if let Some(cause) = self
            .causation_id
            .as_deref()
            .filter(|c| c.starts_with("evt_"))
        {
            envelope["causation_id"] = json!(cause);
        }
        if let (Some(t), Some(a), Some(e)) =
            (self.tenant_id, self.application_id, self.environment_id)
        {
            envelope["ownership"] = json!({
                "tenant_id": t,
                "application_id": a,
                "application_environment_id": e,
            });
        }
        envelope
    }
}

/// Outcome of one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// Transient (timeout, 5xx, unreachable, unknown): retried with backoff.
    Retryable(String),
    /// The receiver refused the event for good (contract violation): dead-lettered.
    Rejected(String),
}

/// Where committed events go. Wired per deployment; until a consumer exists the
/// publisher is not started and events stay PENDING (readiness reports the backlog).
#[async_trait::async_trait]
pub trait EventTransport: Send + Sync {
    async fn deliver(&self, envelope: &Value) -> Delivery;
}

#[derive(Debug, Clone, Copy)]
pub struct PublisherSettings {
    pub lease: chrono::Duration,
    pub max_attempts: i32,
    pub retry: RetryPolicy,
    pub batch: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublishReport {
    pub claimed: usize,
    pub published: usize,
    pub retried: usize,
    pub dead_lettered: usize,
    /// Completions refused because the claim was lost (lease expired meanwhile).
    pub lost_claims: usize,
}

/// Publishes committed events to one or more receivers ([`EventDestination`]).
///
/// With several receivers (`both`, shadow comparison) every receiver's outcome is
/// recorded on the row together with the attempt, so a retry re-sends only to the
/// receivers that have not settled; the event is PUBLISHED once every receiver accepted
/// it and DEAD_LETTER once every receiver settled with at least one permanent refusal
/// (or when the attempts are exhausted). The retry / dead-letter rules of one receiver
/// are unchanged (4xx except `RATE_LIMITED` refuses for good; anything else retries).
pub struct OutboxPublisher {
    db: DatabaseConnection,
    destinations: Vec<(EventDestination, Arc<dyn EventTransport>)>,
    clock: SharedClock,
    settings: PublisherSettings,
    runtime_environment: String,
}

impl OutboxPublisher {
    /// A publisher to Calling only (today's single receiver).
    pub fn new(
        db: DatabaseConnection,
        transport: Arc<dyn EventTransport>,
        clock: SharedClock,
        settings: PublisherSettings,
        runtime_environment: impl Into<String>,
    ) -> Self {
        Self::with_destinations(
            db,
            vec![(EventDestination::Calling, transport)],
            clock,
            settings,
            runtime_environment,
        )
    }

    /// A publisher to every listed receiver (each at most once, in order).
    pub fn with_destinations(
        db: DatabaseConnection,
        mut destinations: Vec<(EventDestination, Arc<dyn EventTransport>)>,
        clock: SharedClock,
        settings: PublisherSettings,
        runtime_environment: impl Into<String>,
    ) -> Self {
        let mut seen = Vec::new();
        destinations.retain(|(d, _)| {
            let fresh = !seen.contains(d);
            seen.push(*d);
            fresh
        });
        Self {
            db,
            destinations,
            clock,
            settings,
            runtime_environment: runtime_environment.into(),
        }
    }

    /// Claims due events (bounded batch) under a fresh claim token.
    pub async fn claim(&self, token: Uuid) -> Result<Vec<OutboxEvent>, DbErr> {
        let now = self.clock.now();
        let mut events = OutboxEvent::find_by_statement(stmt(
            "UPDATE sms_outbox_events o SET lease_token = $1, lease_expires_at = $2 \
             WHERE o.id IN ( \
               SELECT e.id FROM sms_outbox_events e \
                WHERE e.status = 'PENDING' AND e.next_attempt_at <= $3 \
                  AND (e.lease_expires_at IS NULL OR e.lease_expires_at <= $3) \
                  AND NOT EXISTS (SELECT 1 FROM sms_outbox_events p \
                                   WHERE p.aggregate_type = e.aggregate_type AND p.aggregate_id = e.aggregate_id \
                                     AND p.status = 'PENDING' AND p.sequence < e.sequence) \
                ORDER BY e.sequence LIMIT $4 FOR UPDATE SKIP LOCKED) \
             RETURNING o.id, o.sequence, o.event_type, o.schema_version, o.aggregate_type, o.aggregate_id, \
                       o.tenant_id, o.application_id, o.environment_id, o.correlation_id, o.causation_id, \
                       o.payload, o.occurred_at, o.attempts, o.calling_outcome, o.developer_platform_outcome",
            vec![
                token.into(),
                (now + self.settings.lease).into(),
                now.into(),
                (self.settings.batch as i64).into(),
            ],
        ))
        .all(&self.db)
        .await?;
        events.sort_by_key(|e| e.sequence);
        Ok(events)
    }

    /// Delivers one claimed event to every receiver that has not settled it yet and
    /// records the outcomes under `token`.
    pub async fn deliver_claimed(
        &self,
        event: &OutboxEvent,
        token: Uuid,
        report: &mut PublishReport,
    ) -> Result<(), DbErr> {
        let envelope = event.envelope(&self.runtime_environment);
        let mut calling = event.calling_outcome.clone();
        let mut developer_platform = event.developer_platform_outcome.clone();
        let mut retry: Option<String> = None;
        let mut refusal: Option<String> = None;
        for (destination, transport) in &self.destinations {
            let slot = match destination {
                EventDestination::Calling => &mut calling,
                EventDestination::DeveloperPlatform => &mut developer_platform,
            };
            if slot.is_some() {
                continue;
            }
            let outcome = transport.deliver(&envelope).await;
            let label = match &outcome {
                Delivery::Delivered => "accepted",
                Delivery::Rejected(_) => "rejected",
                Delivery::Retryable(_) => "retry",
            };
            crate::metrics::inc(
                "sms_outbox_destination_deliveries_total",
                &[("destination", destination.as_str()), ("outcome", label)],
            );
            match outcome {
                Delivery::Delivered => *slot = Some(OUTCOME_ACCEPTED.into()),
                Delivery::Rejected(reason) => {
                    *slot = Some(OUTCOME_REJECTED.into());
                    refusal.get_or_insert(destination_error(*destination, &reason));
                }
                Delivery::Retryable(reason) => {
                    retry.get_or_insert(destination_error(*destination, &reason));
                }
            }
        }
        let settled = |d: &EventDestination| match d {
            EventDestination::Calling => calling.as_deref(),
            EventDestination::DeveloperPlatform => developer_platform.as_deref(),
        };
        let all_settled = self.destinations.iter().all(|(d, _)| settled(d).is_some());
        let any_refused = self
            .destinations
            .iter()
            .any(|(d, _)| settled(d) == Some(OUTCOME_REJECTED));
        let now = self.clock.now();
        let attempts = event.attempts + 1;
        // (status, last error, next attempt, published at, metric label)
        let (status, error, next, published, label): Completion = if all_settled && !any_refused {
            ("PUBLISHED", None, None, Some(now), "published")
        } else if all_settled {
            // A refusal recorded by an earlier pass is named by its receiver.
            let refusal = refusal.or_else(|| {
                self.destinations
                    .iter()
                    .find(|(d, _)| settled(d) == Some(OUTCOME_REJECTED))
                    .map(|(d, _)| format!("REFUSED by {}", d.as_str()))
            });
            ("DEAD_LETTER", refusal, None, None, "dead_letter")
        } else if attempts >= self.settings.max_attempts {
            ("DEAD_LETTER", retry, None, None, "dead_letter")
        } else {
            (
                "PENDING",
                retry,
                Some(now + self.settings.retry.delay(attempts)),
                None,
                "retry",
            )
        };
        let updated = self
            .db
            .execute(stmt(
                "UPDATE sms_outbox_events SET status = $3, attempts = $4, \
                   last_error = CASE WHEN $3 = 'PUBLISHED' THEN NULL ELSE COALESCE($5, last_error) END, \
                   next_attempt_at = COALESCE($6, next_attempt_at), published_at = $7, \
                   calling_outcome = $8, developer_platform_outcome = $9, \
                   lease_token = NULL, lease_expires_at = NULL \
                 WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                vec![
                    event.id.into(),
                    token.into(),
                    status.into(),
                    attempts.into(),
                    error.as_deref().map(bounded_error).into(),
                    next.into(),
                    published.into(),
                    calling.into(),
                    developer_platform.into(),
                ],
            ))
            .await?
            .rows_affected();
        if updated == 1 {
            match label {
                "published" => report.published += 1,
                "retry" => report.retried += 1,
                _ => report.dead_lettered += 1,
            }
            crate::metrics::inc("sms_outbox_deliveries_total", &[("outcome", label)]);
            if label == "dead_letter" {
                tracing::error!(event_id = %event.event_id(), event_type = %event.event_type, attempts,
                    "outbox event dead-lettered");
            }
        } else {
            report.lost_claims += 1;
            tracing::warn!(event_id = %event.event_id(), "outbox claim lost before completion");
        }
        Ok(())
    }

    /// One publication pass: claim due events and deliver them in sequence.
    pub async fn publish_due(&self) -> Result<PublishReport, DbErr> {
        let token = Uuid::new_v4();
        let events = self.claim(token).await?;
        let mut report = PublishReport {
            claimed: events.len(),
            ..Default::default()
        };
        for event in &events {
            self.deliver_claimed(event, token, &mut report).await?;
        }
        Ok(report)
    }

    /// Runs publication passes every `interval` until the process stops.
    pub async fn run(self: Arc<Self>, interval: std::time::Duration) {
        loop {
            if let Err(e) = self.publish_due().await {
                tracing::warn!(summary = %crate::db::summarize_db_error(&e), "outbox publication pass failed");
            }
            tokio::time::sleep(interval).await;
        }
    }
}

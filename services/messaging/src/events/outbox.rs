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
//! same `event_id`, which consumers de-duplicate. No consumer (realtime gateway, webhooks)
//! exists yet (Workstreams I / later): until one is wired the publisher is not started and
//! events stay PENDING, reported by readiness and metrics.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

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
    /// `messaging.<aggregate>.<change>` (contract `cfmeet.messaging.event`).
    pub event_type: &'static str,
    pub aggregate_type: &'static str,
    pub aggregate_id: Uuid,
    pub scope: EventScope,
    /// snake_case keys; ids, states and versions only — never key material, ciphertext,
    /// content or a credential (enforced by the database).
    pub payload: Value,
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
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
        "INSERT INTO messaging_outbox_events (id, event_type, aggregate_type, aggregate_id, tenant_id, \
           application_id, environment_id, correlation_id, causation_id, payload, occurred_at, next_attempt_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
        vec![
            id.into(),
            event.event_type.into(),
            event.aggregate_type.into(),
            event.aggregate_id.into(),
            event.scope.tenant_id.into(),
            event.scope.application_id.into(),
            event.scope.environment_id.into(),
            ctx.correlation_id.as_str().into(),
            ctx.cause().as_str().into(),
            event.payload.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(id)
}

/// Records `event` as the next durable event of `conversation_id` (its replay cursor),
/// in the caller's transaction. The per-conversation event sequence is allocated from
/// the conversation's counter row, so it is gap-free per committed transaction and
/// totally ordered within the conversation. Returns `(event id, event sequence)`.
pub async fn enqueue_conversation<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    event: NewEvent,
    ctx: &RequestContext,
    now: DateTime<Utc>,
) -> Result<(Uuid, i64), DbErr> {
    let row = db
        .query_one(stmt(
            "UPDATE messaging_conversation_sequences SET next_event_sequence = next_event_sequence + 1 \
             WHERE conversation_id = $1 RETURNING next_event_sequence - 1",
            vec![conversation_id.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("conversation sequence".into()))?;
    let cursor: i64 = row.try_get_by_index(0)?;
    let id = Uuid::new_v4();
    db.execute(stmt(
        "INSERT INTO messaging_outbox_events (id, event_type, aggregate_type, aggregate_id, tenant_id, \
           application_id, environment_id, correlation_id, causation_id, payload, occurred_at, next_attempt_at, \
           conversation_id, conversation_event_sequence) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11, $12, $13)",
        vec![
            id.into(),
            event.event_type.into(),
            event.aggregate_type.into(),
            event.aggregate_id.into(),
            event.scope.tenant_id.into(),
            event.scope.application_id.into(),
            event.scope.environment_id.into(),
            ctx.correlation_id.as_str().into(),
            ctx.cause().as_str().into(),
            event.payload.into(),
            now.into(),
            conversation_id.into(),
            cursor.into(),
        ],
    ))
    .await?;
    Ok((id, cursor))
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
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
    pub attempts: i32,
    pub conversation_id: Option<Uuid>,
    pub conversation_event_sequence: Option<i64>,
}

impl OutboxEvent {
    pub fn event_id(&self) -> String {
        format!("evt_{}", self.id.simple())
    }

    /// The `cfmeet.platform.event-envelope` of this event. The causation id is carried
    /// only when the cause was itself an event (the envelope's definition).
    pub fn envelope(&self, runtime_environment: &str) -> Value {
        let mut envelope = json!({
            "event_id": self.event_id(),
            "event_type": self.event_type,
            "schema_version": self.schema_version,
            "occurred_at": self.occurred_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            "producer_service": cf_service_auth::services::MESSAGING,
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
        envelope["ownership"] = json!({
            "tenant_id": self.tenant_id,
            "application_id": self.application_id,
            "application_environment_id": self.environment_id,
        });
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

    /// Delivers one claimed event. Transports that route by conversation cursor (the
    /// realtime fanout) read it from `event`; the platform envelope is unchanged.
    async fn deliver_event(&self, _event: &OutboxEvent, envelope: &Value) -> Delivery {
        self.deliver(envelope).await
    }
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

pub struct OutboxPublisher {
    db: DatabaseConnection,
    transport: Arc<dyn EventTransport>,
    clock: SharedClock,
    settings: PublisherSettings,
    runtime_environment: String,
}

impl OutboxPublisher {
    pub fn new(
        db: DatabaseConnection,
        transport: Arc<dyn EventTransport>,
        clock: SharedClock,
        settings: PublisherSettings,
        runtime_environment: impl Into<String>,
    ) -> Self {
        Self {
            db,
            transport,
            clock,
            settings,
            runtime_environment: runtime_environment.into(),
        }
    }

    /// Claims due events (bounded batch) under a fresh claim token.
    pub async fn claim(&self, token: Uuid) -> Result<Vec<OutboxEvent>, DbErr> {
        let now = self.clock.now();
        let mut events = OutboxEvent::find_by_statement(stmt(
            "UPDATE messaging_outbox_events o SET lease_token = $1, lease_expires_at = $2 \
             WHERE o.id IN ( \
               SELECT e.id FROM messaging_outbox_events e \
                WHERE e.status = 'PENDING' AND e.next_attempt_at <= $3 \
                  AND (e.lease_expires_at IS NULL OR e.lease_expires_at <= $3) \
                  AND NOT EXISTS (SELECT 1 FROM messaging_outbox_events p \
                                   WHERE p.aggregate_type = e.aggregate_type AND p.aggregate_id = e.aggregate_id \
                                     AND p.status = 'PENDING' AND p.sequence < e.sequence) \
                ORDER BY e.sequence LIMIT $4 FOR UPDATE SKIP LOCKED) \
             RETURNING o.id, o.sequence, o.event_type, o.schema_version, o.aggregate_type, o.aggregate_id, \
                       o.tenant_id, o.application_id, o.environment_id, o.correlation_id, o.causation_id, \
                       o.payload, o.occurred_at, o.attempts, o.conversation_id, o.conversation_event_sequence",
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

    /// Delivers one claimed event and records the outcome under `token`.
    pub async fn deliver_claimed(
        &self,
        event: &OutboxEvent,
        token: Uuid,
        report: &mut PublishReport,
    ) -> Result<(), DbErr> {
        let outcome = self
            .transport
            .deliver_event(event, &event.envelope(&self.runtime_environment))
            .await;
        let now = self.clock.now();
        let attempts = event.attempts + 1;
        let (sql, values, label): (&str, Vec<sea_orm::Value>, &'static str) = match &outcome {
            Delivery::Delivered => (
                "UPDATE messaging_outbox_events SET status = 'PUBLISHED', published_at = $3, attempts = $4, \
                   lease_token = NULL, lease_expires_at = NULL, last_error = NULL \
                 WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                vec![event.id.into(), token.into(), now.into(), attempts.into()],
                "published",
            ),
            Delivery::Rejected(reason) => (
                "UPDATE messaging_outbox_events SET status = 'DEAD_LETTER', attempts = $3, last_error = $4, \
                   lease_token = NULL, lease_expires_at = NULL \
                 WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                vec![event.id.into(), token.into(), attempts.into(), bounded_error(reason).into()],
                "dead_letter",
            ),
            Delivery::Retryable(reason) if attempts >= self.settings.max_attempts => (
                "UPDATE messaging_outbox_events SET status = 'DEAD_LETTER', attempts = $3, last_error = $4, \
                   lease_token = NULL, lease_expires_at = NULL \
                 WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                vec![event.id.into(), token.into(), attempts.into(), bounded_error(reason).into()],
                "dead_letter",
            ),
            Delivery::Retryable(reason) => (
                "UPDATE messaging_outbox_events SET attempts = $3, last_error = $4, next_attempt_at = $5, \
                   lease_token = NULL, lease_expires_at = NULL \
                 WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                vec![
                    event.id.into(),
                    token.into(),
                    attempts.into(),
                    bounded_error(reason).into(),
                    (now + self.settings.retry.delay(attempts)).into(),
                ],
                "retry",
            ),
        };
        let updated = self.db.execute(stmt(sql, values)).await?.rows_affected();
        if updated == 1 {
            match label {
                "published" => report.published += 1,
                "retry" => report.retried += 1,
                _ => report.dead_lettered += 1,
            }
            crate::metrics::inc("messaging_outbox_deliveries_total", &[("outcome", label)]);
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

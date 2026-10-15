//! Durable processing (Part G §67 / §68): the outbox publisher and the webhook delivery
//! worker. Both claim with `FOR UPDATE SKIP LOCKED` + a lease (multi-worker safe; a
//! crashed worker's claim expires and the row is reclaimed), retry with bounded backoff
//! and dead-letter; every completion is guarded by the claim (status + attempt), so a
//! stale worker can never overwrite a newer outcome.

use async_trait::async_trait;
use chrono::Duration;
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::Value;
use uuid::Uuid;

use super::{catalog, stmt, DbResultExt, WebhookResult, WebhookService};

/// Publication attempts of one outbox event before it is dead-lettered.
pub const OUTBOX_MAX_ATTEMPTS: i32 = 10;
const OUTBOX_LEASE_SECONDS: i64 = 60;

pub const PRODUCER_CALLING: &str = "cfmeet.calling";
pub const PRODUCER_PRIVACY_RELAY: &str = "cfmeet.privacy-relay";

/// The neutral event publisher boundary: the event schema (the platform envelope) is
/// broker-independent; a deployment may add a broker transport without changing it.
#[async_trait]
pub trait EventTransport: Send + Sync {
    /// Publishes ONE envelope. `Err` is a bounded failure code (the row is retried).
    async fn publish(&self, producer: &str, envelope: &Value) -> Result<(), &'static str>;
}

/// The built-in transport: normalizes the envelope into the public catalog and fans it
/// out to customer webhook subscriptions (Calling's own database).
pub struct LocalWebhookTransport {
    webhooks: WebhookService,
}

impl LocalWebhookTransport {
    pub fn new(webhooks: WebhookService) -> Self {
        Self { webhooks }
    }
}

/// (tenant, application, environment) from an envelope's ownership block.
pub(crate) fn ownership(envelope: &Value) -> Option<(Uuid, Uuid, Uuid)> {
    let o = envelope.get("ownership")?;
    let id = |k: &str| o.get(k)?.as_str()?.parse::<Uuid>().ok();
    Some((
        id("tenant_id")?,
        id("application_id")?,
        id("application_environment_id")?,
    ))
}

#[async_trait]
impl EventTransport for LocalWebhookTransport {
    async fn publish(&self, producer: &str, envelope: &Value) -> Result<(), &'static str> {
        let projected = match producer {
            PRODUCER_CALLING => catalog::project_calling(envelope),
            PRODUCER_PRIVACY_RELAY => catalog::project_privacy_relay(envelope),
            _ => return Err("UNKNOWN_PRODUCER"),
        };
        // Internal-only facts are acknowledged and never become public events.
        let Some(event) = projected else {
            return Ok(());
        };
        let source = envelope
            .get("event_id")
            .and_then(Value::as_str)
            .ok_or("MALFORMED_ENVELOPE")?;
        let scope = ownership(envelope).ok_or("MALFORMED_ENVELOPE")?;
        let occurred_at = envelope
            .get("occurred_at")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .ok_or("MALFORMED_ENVELOPE")?;
        let correlation = envelope.get("correlation_id").and_then(Value::as_str);
        self.webhooks
            .ingest_public_event(
                self.webhooks.db(),
                producer,
                source,
                scope,
                occurred_at,
                correlation,
                &event,
            )
            .await
            .map(|_| ())
            .map_err(|_| "WEBHOOK_STORE_UNAVAILABLE")
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublishReport {
    pub published: usize,
    pub retried: usize,
    pub dead_lettered: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryReport {
    pub succeeded: usize,
    pub retried: usize,
    pub failed: usize,
    pub cancelled: usize,
}

#[derive(Debug, FromQueryResult)]
struct ClaimedOutbox {
    id: Uuid,
    envelope: Value,
    attempt_count: i32,
}

/// A delivery claimed by this worker (its lease is held until the outcome is written).
#[derive(Debug, Clone, FromQueryResult)]
pub struct ClaimedDelivery {
    pub id: Uuid,
    pub webhook_event_id: Uuid,
    pub subscription_id: Uuid,
    pub attempt_count: i32,
}

#[derive(Debug, FromQueryResult)]
struct Target {
    endpoint_url: String,
    status: String,
    secret_sealed: String,
    body: Value,
    event_id: String,
}

fn outbox_backoff(attempt: i32) -> Duration {
    Duration::seconds(2_i64.saturating_pow(attempt.clamp(0, 9) as u32).min(300))
}

impl WebhookService {
    /// One publisher pass over Calling's transactional outbox.
    pub async fn publish_outbox_once(
        &self,
        transport: &dyn EventTransport,
        limit: u64,
    ) -> WebhookResult<PublishReport> {
        let now = self.now();
        let claimed = ClaimedOutbox::find_by_statement(stmt(
            "UPDATE calling_outbox_events o SET lease_until = $1, attempt_count = o.attempt_count + 1, \
               updated_at = $2 \
             WHERE o.id IN (SELECT id FROM calling_outbox_events \
                             WHERE status = 'PENDING' AND next_attempt_at <= $2 \
                               AND (lease_until IS NULL OR lease_until <= $2) \
                             ORDER BY next_attempt_at, created_at LIMIT $3 FOR UPDATE SKIP LOCKED) \
             RETURNING o.id, o.envelope, o.attempt_count",
            vec![
                (now + Duration::seconds(OUTBOX_LEASE_SECONDS)).into(),
                now.into(),
                (limit as i64).into(),
            ],
        ))
        .all(self.db())
        .await
        .db()?;
        let mut report = PublishReport::default();
        for row in claimed {
            let outcome = transport
                .publish(super::worker::PRODUCER_CALLING, &row.envelope)
                .await;
            let now = self.now();
            match outcome {
                Ok(()) => {
                    self.db()
                        .execute(stmt(
                            "UPDATE calling_outbox_events SET status = 'PUBLISHED', published_at = $2, \
                               lease_until = NULL, last_error_code = NULL, updated_at = $2 \
                             WHERE id = $1 AND status = 'PENDING' AND attempt_count = $3",
                            vec![row.id.into(), now.into(), row.attempt_count.into()],
                        ))
                        .await
                        .db()?;
                    report.published += 1;
                }
                Err(code) if row.attempt_count >= OUTBOX_MAX_ATTEMPTS => {
                    self.db()
                        .execute(stmt(
                            "UPDATE calling_outbox_events SET status = 'DEAD_LETTER', lease_until = NULL, \
                               last_error_code = $2, updated_at = $3 \
                             WHERE id = $1 AND status = 'PENDING' AND attempt_count = $4",
                            vec![row.id.into(), code.into(), now.into(), row.attempt_count.into()],
                        ))
                        .await
                        .db()?;
                    tracing::error!(outbox_id = %row.id, code, "outbox event dead-lettered");
                    report.dead_lettered += 1;
                }
                Err(code) => {
                    self.db()
                        .execute(stmt(
                            "UPDATE calling_outbox_events SET next_attempt_at = $2, lease_until = NULL, \
                               last_error_code = $3, updated_at = $4 \
                             WHERE id = $1 AND status = 'PENDING' AND attempt_count = $5",
                            vec![
                                row.id.into(),
                                (now + outbox_backoff(row.attempt_count)).into(),
                                code.into(),
                                now.into(),
                                row.attempt_count.into(),
                            ],
                        ))
                        .await
                        .db()?;
                    report.retried += 1;
                }
            }
        }
        if report.published + report.retried + report.dead_lettered > 0 {
            crate::metrics::add(
                "calling_outbox_published_total",
                &[],
                report.published as u64,
            );
            crate::metrics::add(
                "calling_outbox_dead_lettered_total",
                &[],
                report.dead_lettered as u64,
            );
        }
        Ok(report)
    }

    /// Operator replay of a dead-lettered outbox event (it is published again; consumers
    /// dedupe on the event id).
    pub async fn replay_dead_lettered_outbox(&self, event_id: &str) -> WebhookResult<bool> {
        let now = self.now();
        Ok(self
            .db()
            .execute(stmt(
                "UPDATE calling_outbox_events SET status = 'PENDING', attempt_count = 0, \
                   next_attempt_at = $2, lease_until = NULL, last_error_code = NULL, updated_at = $2 \
                 WHERE event_id = $1 AND status = 'DEAD_LETTER'",
                vec![event_id.into(), now.into()],
            ))
            .await
            .db()?
            .rows_affected()
            > 0)
    }

    fn delivery_lease(&self) -> Duration {
        Duration::seconds(self.settings.request_timeout.as_secs() as i64 + 30)
    }

    /// Claims due deliveries (and deliveries whose worker lease expired).
    pub async fn claim_due(&self, limit: u64) -> WebhookResult<Vec<ClaimedDelivery>> {
        let now = self.now();
        ClaimedDelivery::find_by_statement(stmt(
            "UPDATE webhook_deliveries d SET status = 'DELIVERING', lease_until = $1, \
               attempt_count = d.attempt_count + 1, updated_at = $2 \
             WHERE d.id IN (SELECT id FROM webhook_deliveries \
                             WHERE ((status IN ('PENDING', 'RETRY_SCHEDULED') AND next_attempt_at <= $2) \
                                    OR (status = 'DELIVERING' AND lease_until <= $2)) \
                               AND attempt_count < 64 \
                             ORDER BY next_attempt_at LIMIT $3 FOR UPDATE SKIP LOCKED) \
             RETURNING d.id, d.webhook_event_id, d.subscription_id, d.attempt_count",
            vec![
                (now + self.delivery_lease()).into(),
                now.into(),
                (limit as i64).into(),
            ],
        ))
        .all(self.db())
        .await
        .db()
    }

    /// One delivery pass: claim, sign, POST, record.
    pub async fn deliver_due_once(&self, limit: u64) -> WebhookResult<DeliveryReport> {
        let claimed = self.claim_due(limit).await?;
        let mut report = DeliveryReport::default();
        for c in claimed {
            match self.attempt(&c).await? {
                "SUCCEEDED" => report.succeeded += 1,
                "RETRY_SCHEDULED" => report.retried += 1,
                "CANCELLED" => report.cancelled += 1,
                _ => report.failed += 1,
            }
        }
        Ok(report)
    }

    async fn attempt(&self, c: &ClaimedDelivery) -> WebhookResult<&'static str> {
        let started = self.now();
        let Some(target) = Target::find_by_statement(stmt(
            "SELECT s.endpoint_url, s.status, s.secret_sealed, e.body, e.event_id \
               FROM webhook_subscriptions s, webhook_events e WHERE s.id = $1 AND e.id = $2",
            vec![c.subscription_id.into(), c.webhook_event_id.into()],
        ))
        .one(self.db())
        .await
        .db()?
        else {
            return self
                .finish(c, "FAILED", None, Some("MISSING_TARGET"), started)
                .await;
        };
        if target.status != "ACTIVE" {
            return self
                .finish(c, "CANCELLED", None, Some("SUBSCRIPTION_INACTIVE"), started)
                .await;
        }
        let secret = match self
            .settings
            .secret_box
            .as_deref()
            .map(|b| b.open(&target.secret_sealed, &c.subscription_id.to_string()))
        {
            Some(Ok(s)) => s,
            outcome => {
                // Configuration problem: refused, counted and retried — never sent
                // unsigned. A seal under a key id this deployment does not hold (rotated
                // away too early, or tampered) is told apart from a missing key ring.
                let category = match outcome {
                    Some(Err(cf_webhooks::secret_box::SecretBoxError::UnknownKey)) => {
                        "SIGNING_KEY_UNKNOWN"
                    }
                    _ => "SIGNING_UNAVAILABLE",
                };
                crate::metrics::inc(
                    "calling_webhook_signing_refusals_total",
                    &[("reason", category)],
                );
                tracing::warn!(
                    delivery_id = %c.id,
                    category,
                    "webhook delivery refused: the signing secret cannot be opened"
                );
                return self.finish(c, "RETRY", None, Some(category), started).await;
            }
        };
        // The SSRF policy is enforced again at delivery time (and at connection time by
        // the client's guarded resolver).
        let Ok(url) =
            cf_webhooks::endpoint::validate_url(&target.endpoint_url, self.settings.policy)
        else {
            return self
                .finish(c, "FAILED", None, Some("ENDPOINT_BLOCKED"), started)
                .await;
        };
        let body =
            serde_json::to_vec(&target.body).map_err(|_| crate::error::CallingError::Internal)?;
        let timestamp = self.now().timestamp();
        let delivery_id = c.id.to_string();
        let signature =
            cf_webhooks::signing::sign(secret.as_bytes(), timestamp, &delivery_id, &body);
        use cf_webhooks::signing::*;
        let sent = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header(HEADER_DELIVERY_ID, &delivery_id)
            .header(HEADER_EVENT_ID, &target.event_id)
            .header(HEADER_TIMESTAMP, timestamp.to_string())
            .header(HEADER_SIGNATURE, signature)
            .body(body)
            .send()
            .await;
        let (outcome, http) = match &sent {
            Ok(r) => (
                cf_webhooks::retry::classify_status(r.status().as_u16()),
                Some(r.status().as_u16() as i32),
            ),
            Err(e) => (cf_webhooks::retry::classify_error(e), None),
        };
        match outcome {
            cf_webhooks::retry::Outcome::Delivered => {
                self.finish(c, "SUCCEEDED", http, None, started).await
            }
            cf_webhooks::retry::Outcome::Retry(category) => {
                self.finish(c, "RETRY", http, Some(category), started).await
            }
            cf_webhooks::retry::Outcome::Permanent(category) => {
                self.finish(c, "FAILED", http, Some(category), started)
                    .await
            }
        }
    }

    /// Records the attempt and moves the delivery on — only if this worker still holds
    /// the claim (status DELIVERING with this attempt number).
    async fn finish(
        &self,
        c: &ClaimedDelivery,
        outcome: &'static str,
        http: Option<i32>,
        category: Option<&'static str>,
        started: chrono::DateTime<chrono::Utc>,
    ) -> WebhookResult<&'static str> {
        let now = self.now();
        let exhausted = outcome == "RETRY" && c.attempt_count as u32 >= self.settings.max_attempts;
        let status = match outcome {
            "SUCCEEDED" => "SUCCEEDED",
            "CANCELLED" => "CANCELLED",
            "RETRY" if !exhausted => "RETRY_SCHEDULED",
            _ => "FAILED",
        };
        let next = now
            + Duration::from_std(cf_webhooks::retry::backoff(c.attempt_count as u32 + 1))
                .unwrap_or_else(|_| Duration::seconds(30));
        let txn = sea_orm::TransactionTrait::begin(self.db()).await.db()?;
        let changed = txn
            .execute(stmt(
                "UPDATE webhook_deliveries SET status = $2, lease_until = NULL, \
                   last_http_status = COALESCE($3, last_http_status), failure_category = $4, \
                   next_attempt_at = CASE WHEN $2 = 'RETRY_SCHEDULED' THEN $5 ELSE next_attempt_at END, \
                   completed_at = CASE WHEN $2 IN ('SUCCEEDED', 'FAILED', 'CANCELLED') THEN $6 ELSE NULL END, \
                   updated_at = $6 \
                 WHERE id = $1 AND status = 'DELIVERING' AND attempt_count = $7",
                vec![
                    c.id.into(),
                    status.into(),
                    http.into(),
                    category.into(),
                    next.into(),
                    now.into(),
                    c.attempt_count.into(),
                ],
            ))
            .await
            .db()?
            .rows_affected();
        if changed == 0 {
            // The lease expired and another worker owns the delivery now.
            txn.rollback().await.db()?;
            return Ok("STALE");
        }
        if outcome != "CANCELLED" {
            txn.execute(stmt(
                "INSERT INTO webhook_delivery_attempts (delivery_id, attempt_number, outcome, \
                   http_status, failure_category, started_at, finished_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
                vec![
                    c.id.into(),
                    c.attempt_count.into(),
                    (if status == "RETRY_SCHEDULED" {
                        "RETRY"
                    } else if status == "SUCCEEDED" {
                        "SUCCEEDED"
                    } else {
                        "FAILED"
                    })
                    .into(),
                    http.into(),
                    category.into(),
                    started.into(),
                    now.into(),
                ],
            ))
            .await
            .db()?;
        }
        txn.commit().await.db()?;
        crate::metrics::inc("calling_webhook_deliveries_total", &[("result", status)]);
        if status == "FAILED" {
            tracing::warn!(delivery_id = %c.id, category = category.unwrap_or(""), "webhook delivery dead-lettered");
        }
        Ok(status)
    }
}

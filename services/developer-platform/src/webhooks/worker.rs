//! The delivery worker (Calling's Part G §68 semantics, carried over): deliveries are
//! claimed with `FOR UPDATE SKIP LOCKED` + a lease (multi-worker safe; a crashed worker's
//! claim expires and the row is reclaimed), signed with `cf_webhooks::signing`, POSTed
//! through the SSRF-safe `cf_webhooks::endpoint` client, classified by
//! `cf_webhooks::retry` (2xx success; 408 / 429 / 5xx / network retried with jittered
//! backoff; other answers permanent) and dead-lettered (FAILED) after the bounded number
//! of attempts. Every completion is guarded by the claim (status + attempt number), so a
//! stale worker can never overwrite a newer outcome. Each attempt is recorded (status
//! class and error class only — never a response body) and moves the endpoint's health;
//! an endpoint failing `auto_disable_after` consecutive attempts is disabled
//! (`CONSECUTIVE_FAILURES`). In shadow mode due deliveries are recorded SHADOW and nothing
//! is sent.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use super::{DeliveryMode, WebhookService};
use crate::control::stmt;

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryReport {
    pub succeeded: usize,
    pub retried: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub shadowed: usize,
    /// Claims another worker took over (lease expired) before this one finished.
    pub stale: usize,
    /// Endpoints disabled automatically during the pass.
    pub disabled: usize,
}

/// A delivery claimed by this worker (its lease is held until the outcome is written).
#[derive(Debug, Clone, FromQueryResult)]
pub struct ClaimedDelivery {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event_id: String,
    pub attempt_count: i32,
}

#[derive(Debug, FromQueryResult)]
struct Target {
    url: String,
    status: String,
    /// `None` only for a migrated subscription awaiting a secret rotation (never ACTIVE).
    secret_sealed: Option<String>,
    payload: Value,
}

/// Section 53 cutover gate (a claim condition on a delivery of `devplat_webhook_deliveries`
/// rows aliased by the table name): without any migration every delivery is claimable;
/// while the migration from Calling is PREPARING only deliveries to NATIVE subscriptions
/// are (Calling still sends to the migrated ones); once CUT_OVER (the takeover verified
/// that Calling stopped sending) every delivery is; once ROLLED_BACK none is (Calling is
/// the authority again). Held mirrors of Calling's open deliveries are never claimed.
pub const MIGRATION_GATE: &str = "(NOT EXISTS (SELECT 1 FROM devplat_webhook_migration_state m WHERE m.id = 1) \
    OR EXISTS (SELECT 1 FROM devplat_webhook_migration_state m WHERE m.id = 1 AND m.phase = 'CUT_OVER') \
    OR (EXISTS (SELECT 1 FROM devplat_webhook_migration_state m WHERE m.id = 1 AND m.phase = 'PREPARING') \
        AND EXISTS (SELECT 1 FROM devplat_webhook_subscriptions gs \
                     WHERE gs.id = devplat_webhook_deliveries.subscription_id AND gs.origin = 'NATIVE')))";

/// How an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Succeeded,
    Retry,
    Failed,
    Cancelled,
}

impl WebhookService {
    fn lease(&self) -> Duration {
        Duration::seconds(self.settings.request_timeout.as_secs() as i64 + 30)
    }

    /// Claims due deliveries (and deliveries whose worker lease expired).
    pub async fn claim_due(&self, limit: u64) -> Result<Vec<ClaimedDelivery>, DbErr> {
        let now = self.now();
        ClaimedDelivery::find_by_statement(stmt(
            &format!("UPDATE devplat_webhook_deliveries d SET status = 'DELIVERING', lease_until = $1, \
               attempt_count = d.attempt_count + 1, updated_at = $2 \
             WHERE d.id IN (SELECT id FROM devplat_webhook_deliveries \
                             WHERE ((status IN ('PENDING', 'RETRY_SCHEDULED') AND next_attempt_at <= $2) \
                                    OR (status = 'DELIVERING' AND lease_until <= $2)) \
                               AND attempt_count < $4 AND NOT migration_hold \
                               AND {MIGRATION_GATE} \
                             ORDER BY next_attempt_at LIMIT $3 FOR UPDATE SKIP LOCKED) \
             RETURNING d.id, d.subscription_id, d.event_id, d.attempt_count"),
            vec![
                (now + self.lease()).into(),
                now.into(),
                (limit as i64).into(),
                (self.settings.max_attempts as i32).into(),
            ],
        ))
        .all(self.db())
        .await
    }

    /// A claim whose worker died at the attempt cap is dead-lettered instead of being
    /// reclaimed forever (a poison delivery that crashes its worker cannot loop).
    async fn dead_letter_expired_at_cap(&self, limit: u64) -> Result<usize, DbErr> {
        let now = self.now();
        Ok(self
            .db()
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries SET status = 'FAILED', lease_until = NULL, \
                   failure_category = 'LEASE_EXPIRED_AT_CAP', completed_at = $1, updated_at = $1 \
                 WHERE id IN (SELECT id FROM devplat_webhook_deliveries \
                               WHERE status = 'DELIVERING' AND lease_until <= $1 AND attempt_count >= $2 \
                                 AND NOT migration_hold \
                               ORDER BY next_attempt_at LIMIT $3 FOR UPDATE SKIP LOCKED)",
                vec![
                    now.into(),
                    (self.settings.max_attempts as i32).into(),
                    (limit as i64).into(),
                ],
            ))
            .await?
            .rows_affected() as usize)
    }

    /// Shadow mode: due deliveries are recorded SHADOW; nothing is sent.
    async fn shadow_due(&self, limit: u64) -> Result<usize, DbErr> {
        let now = self.now();
        Ok(self
            .db()
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries SET status = 'SHADOW', lease_until = NULL, \
                   completed_at = $1, updated_at = $1 \
                 WHERE id IN (SELECT id FROM devplat_webhook_deliveries \
                               WHERE (status IN ('PENDING', 'RETRY_SCHEDULED') \
                                  OR (status = 'DELIVERING' AND lease_until <= $1)) \
                                 AND NOT migration_hold \
                               ORDER BY next_attempt_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
                vec![now.into(), (limit as i64).into()],
            ))
            .await?
            .rows_affected() as usize)
    }

    /// One delivery pass: claim, sign, POST, record (or record SHADOW in shadow mode).
    pub async fn deliver_due_once(&self, limit: u64) -> Result<DeliveryReport, DbErr> {
        let mut report = DeliveryReport::default();
        if self.settings.mode == DeliveryMode::Shadow {
            report.shadowed = self.shadow_due(limit).await?;
            return Ok(report);
        }
        report.failed += self.dead_letter_expired_at_cap(limit).await?;
        for c in self.claim_due(limit).await? {
            let (status, disabled) = self.attempt(&c).await?;
            report.disabled += disabled as usize;
            match status {
                "SUCCEEDED" => report.succeeded += 1,
                "RETRY_SCHEDULED" => report.retried += 1,
                "CANCELLED" => report.cancelled += 1,
                "STALE" => report.stale += 1,
                _ => report.failed += 1,
            }
        }
        Ok(report)
    }

    async fn attempt(&self, c: &ClaimedDelivery) -> Result<(&'static str, bool), DbErr> {
        let started = self.now();
        let Some(target) = Target::find_by_statement(stmt(
            "SELECT s.url, s.status, s.secret_sealed, e.payload \
               FROM devplat_webhook_subscriptions s JOIN devplat_webhook_events e ON e.event_id = $2 \
              WHERE s.id = $1",
            vec![c.subscription_id.into(), c.event_id.clone().into()],
        ))
        .one(self.db())
        .await?
        else {
            return self
                .finish(c, Outcome::Failed, None, Some("MISSING_TARGET"), started, false)
                .await;
        };
        if target.status != "ACTIVE" {
            return self
                .finish(
                    c,
                    Outcome::Cancelled,
                    None,
                    Some("SUBSCRIPTION_INACTIVE"),
                    started,
                    false,
                )
                .await;
        }
        let opened = self
            .settings
            .key_ring
            .as_deref()
            .zip(target.secret_sealed.as_deref())
            .map(|(ring, sealed)| ring.open(sealed, &c.subscription_id.to_string()));
        let signing_key = match opened {
            Some(Ok(s)) => s,
            outcome => {
                // A configuration problem: refused and retried, never sent unsigned. A seal
                // under a key id this deployment does not hold is told apart from a
                // missing key ring. Not an endpoint failure (health is unchanged).
                let category = match outcome {
                    Some(Err(cf_webhooks::secret_box::SecretBoxError::UnknownKey)) => {
                        "SIGNING_KEY_UNKNOWN"
                    }
                    _ => "SIGNING_UNAVAILABLE",
                };
                tracing::warn!(delivery_id = %c.id, category, "webhook delivery refused: it cannot be signed");
                return self
                    .finish(c, Outcome::Retry, None, Some(category), started, false)
                    .await;
            }
        };
        // The SSRF policy is enforced again at delivery time (and at connection time by
        // the client's guarded resolver).
        let Ok(url) = cf_webhooks::endpoint::validate_url(&target.url, self.settings.policy) else {
            return self
                .finish(
                    c,
                    Outcome::Failed,
                    None,
                    Some("ENDPOINT_BLOCKED"),
                    started,
                    true,
                )
                .await;
        };
        let body = serde_json::to_vec(&target.payload).unwrap_or_default();
        let timestamp = self.now().timestamp();
        let delivery_id = c.id.to_string();
        let signature =
            cf_webhooks::signing::sign(signing_key.as_bytes(), timestamp, &delivery_id, &body);
        drop(signing_key);
        use cf_webhooks::signing::{
            HEADER_DELIVERY_ID, HEADER_EVENT_ID, HEADER_SIGNATURE, HEADER_TIMESTAMP,
        };
        let sent = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header(HEADER_DELIVERY_ID, &delivery_id)
            .header(HEADER_EVENT_ID, &c.event_id)
            .header(HEADER_TIMESTAMP, timestamp.to_string())
            .header(HEADER_SIGNATURE, signature)
            .body(body)
            .send()
            .await;
        let (classified, http) = match &sent {
            Ok(r) => (
                cf_webhooks::retry::classify_status(r.status().as_u16()),
                Some(r.status().as_u16() as i32),
            ),
            Err(e) => (cf_webhooks::retry::classify_error(e), None),
        };
        // A receiver that is rate limiting (429) or unavailable (503) may ask for a later
        // retry: honoured when longer than the backoff (never shorter).
        let retry_after = match &sent {
            Ok(r) if matches!(r.status().as_u16(), 429 | 503) => cf_webhooks::retry::retry_after(
                r.headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
                self.now().timestamp(),
            ),
            _ => None,
        };
        // The response body is never read or stored.
        drop(sent);
        match classified {
            cf_webhooks::retry::Outcome::Delivered => {
                self.finish(c, Outcome::Succeeded, http, None, started, true)
                    .await
            }
            cf_webhooks::retry::Outcome::Retry(category) => {
                self.finish_after(
                    c,
                    Outcome::Retry,
                    http,
                    Some(category),
                    started,
                    true,
                    retry_after,
                )
                .await
            }
            cf_webhooks::retry::Outcome::Permanent(category) => {
                self.finish(c, Outcome::Failed, http, Some(category), started, true)
                    .await
            }
        }
    }

    /// Records the attempt and moves the delivery (and the endpoint health) on — only if
    /// this worker still holds the claim (status DELIVERING with this attempt number).
    /// Returns the new status and whether the endpoint was disabled automatically.
    async fn finish(
        &self,
        c: &ClaimedDelivery,
        outcome: Outcome,
        http: Option<i32>,
        category: Option<&'static str>,
        started: DateTime<Utc>,
        endpoint_health: bool,
    ) -> Result<(&'static str, bool), DbErr> {
        self.finish_after(c, outcome, http, category, started, endpoint_health, None)
            .await
    }

    /// [`Self::finish`] with the receiver's `Retry-After` (a retry is scheduled no earlier).
    #[allow(clippy::too_many_arguments)]
    async fn finish_after(
        &self,
        c: &ClaimedDelivery,
        outcome: Outcome,
        http: Option<i32>,
        category: Option<&'static str>,
        started: DateTime<Utc>,
        endpoint_health: bool,
        retry_after: Option<std::time::Duration>,
    ) -> Result<(&'static str, bool), DbErr> {
        let now = self.now();
        let exhausted =
            outcome == Outcome::Retry && c.attempt_count as u32 >= self.settings.max_attempts;
        let status = match outcome {
            Outcome::Succeeded => "SUCCEEDED",
            Outcome::Cancelled => "CANCELLED",
            Outcome::Retry if !exhausted => "RETRY_SCHEDULED",
            _ => "FAILED",
        };
        let next = now
            + Duration::from_std(cf_webhooks::retry::next_delay(
                c.attempt_count as u32 + 1,
                retry_after,
            ))
            .unwrap_or_else(|_| Duration::seconds(30));
        let txn = self.db().begin().await?;
        // Lock order: subscription, then delivery (as every subscription mutation does).
        let endpoint = if endpoint_health && outcome != Outcome::Cancelled {
            txn.query_one(stmt(
                "SELECT status, consecutive_failures FROM devplat_webhook_subscriptions \
                  WHERE id = $1 FOR UPDATE",
                vec![c.subscription_id.into()],
            ))
            .await?
            .map(|r| {
                Ok::<_, DbErr>((
                    r.try_get_by_index::<String>(0)?,
                    r.try_get_by_index::<i32>(1)?,
                ))
            })
            .transpose()?
            .filter(|(s, _)| s != "DELETED")
        } else {
            None
        };
        let changed = txn
            .execute(stmt(
                "UPDATE devplat_webhook_deliveries SET status = $2, lease_until = NULL, \
                   last_http_status = COALESCE($3, last_http_status), failure_category = $4, \
                   next_attempt_at = CASE WHEN $2 = 'RETRY_SCHEDULED' THEN $5 ELSE next_attempt_at END, \
                   completed_at = CASE WHEN $2 IN ('SUCCEEDED', 'FAILED', 'CANCELLED') THEN $6 END, \
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
            .await?
            .rows_affected();
        if changed == 0 {
            // The lease expired and another worker owns the delivery now.
            txn.rollback().await?;
            return Ok(("STALE", false));
        }
        if outcome != Outcome::Cancelled {
            txn.execute(stmt(
                "INSERT INTO devplat_webhook_attempts (delivery_id, attempt_number, started_at, \
                   finished_at, http_status, outcome, error_class) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
                vec![
                    c.id.into(),
                    c.attempt_count.into(),
                    started.into(),
                    now.max(started).into(),
                    http.into(),
                    match status {
                        "SUCCEEDED" => "SUCCEEDED",
                        "RETRY_SCHEDULED" => "RETRY",
                        _ => "FAILED",
                    }
                    .into(),
                    category.into(),
                ],
            ))
            .await?;
        }
        let mut disabled = false;
        if let Some((endpoint_status, failures)) = endpoint {
            if outcome == Outcome::Succeeded {
                txn.execute(stmt(
                    "UPDATE devplat_webhook_subscriptions SET consecutive_failures = 0, \
                       last_success_at = $2 WHERE id = $1",
                    vec![c.subscription_id.into(), now.into()],
                ))
                .await?;
            } else {
                let failures = failures.saturating_add(1);
                disabled =
                    endpoint_status == "ACTIVE" && failures >= self.settings.auto_disable_after;
                txn.execute(stmt(
                    "UPDATE devplat_webhook_subscriptions SET consecutive_failures = $2, \
                       last_failure_at = $3, \
                       status = CASE WHEN $4 THEN 'DISABLED' ELSE status END, \
                       disabled_reason = CASE WHEN $4 THEN 'CONSECUTIVE_FAILURES' ELSE disabled_reason END, \
                       version = CASE WHEN $4 THEN version + 1 ELSE version END, \
                       updated_at = CASE WHEN $4 THEN $3 ELSE updated_at END \
                     WHERE id = $1",
                    vec![
                        c.subscription_id.into(),
                        failures.into(),
                        now.into(),
                        disabled.into(),
                    ],
                ))
                .await?;
                if disabled {
                    tracing::warn!(webhook_id = %c.subscription_id, threshold = self.settings.auto_disable_after,
                        "webhook endpoint disabled after consecutive failures");
                }
            }
        }
        txn.commit().await?;
        if status == "FAILED" {
            tracing::warn!(delivery_id = %c.id, category = category.unwrap_or(""), "webhook delivery dead-lettered");
        }
        Ok((status, disabled))
    }
}

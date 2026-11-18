//! Bounded retention purge of webhook, API usage and activity history (P9).
//!
//! These tables only grew. The worker now deletes rows older than each class's bound, in
//! bounded batches, once per [`RETENTION_INTERVAL`] across every worker instance
//! (`devplat_worker_schedules`, job `retention`). Each batch is ONE transaction that sets
//! `SET LOCAL devplat.retention_worker = 'on'` — the only way past the tables'
//! delete guards (gap-closure migration `m20261206_000001_retention_purge`) — claims its
//! rows with `FOR UPDATE SKIP LOCKED` (two workers never act on the same row), deletes
//! them and appends one `devplat_retention_audit` row (class, rows, policy): the durable
//! per-class counters (`retention_summary`, part of the operator ingestion health view).
//! A failed batch rolls back entirely, is logged and audited as FAILED, and is retried at
//! the next run.
//!
//! | Class | Rows | Variable (days; `0` = keep) | Default |
//! |---|---|---|---|
//! | `WEBHOOK_DELIVERY_HISTORY` | terminal deliveries (SUCCEEDED / FAILED / CANCELLED / SHADOW) completed before the bound, with their attempts; never a delivery a newer or open replay still references | `DEVELOPER_PLATFORM_RETENTION_WEBHOOK_DELIVERY_DAYS` | 30 |
//! | `WEBHOOK_EVENT_HISTORY` | events received before the bound with no delivery left | `DEVELOPER_PLATFORM_RETENTION_WEBHOOK_EVENT_DAYS` | 30 |
//! | `API_USAGE_BATCHES` | the batch-id idempotency ledger of API request accounting | `DEVELOPER_PLATFORM_RETENTION_API_USAGE_BATCH_DAYS` | 35 |
//! | `API_USAGE_COUNTERS` | daily request totals and API_OPERATION counters (the gateway re-seeds only today's window) | `DEVELOPER_PLATFORM_RETENTION_API_USAGE_DAYS` | 400 |
//! | `INGESTION_REJECTIONS` | refused usage facts and platform events | `DEVELOPER_PLATFORM_RETENTION_REJECTION_DAYS` | 90 |
//! | `ACTIVITY_EVENTS` | the console activity feed | `DEVELOPER_PLATFORM_RETENTION_ACTIVITY_DAYS` | 365 |
//!
//! `DEVELOPER_PLATFORM_RETENTION_ENABLED=false` turns the whole purge off. Not purged
//! (owner decision on billing evidence): the unified usage ledger's facts and rollups,
//! reconciliations, entitlement snapshots and the operator audit.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

/// Minimum time between two purge runs across all worker instances.
pub const RETENTION_INTERVAL: Duration = Duration::from_secs(3600);
/// Rows per class per run (each class: one transaction).
pub const RETENTION_BATCH: i64 = 1000;
/// Schedule name.
pub const JOB: &str = "retention";

/// The purge classes, in run order (deliveries before their events).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    WebhookDeliveryHistory,
    WebhookEventHistory,
    ApiUsageBatches,
    ApiUsageCounters,
    IngestionRejections,
    ActivityEvents,
}

impl Class {
    pub const ALL: [Class; 6] = [
        Class::WebhookDeliveryHistory,
        Class::WebhookEventHistory,
        Class::ApiUsageBatches,
        Class::ApiUsageCounters,
        Class::IngestionRejections,
        Class::ActivityEvents,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Class::WebhookDeliveryHistory => "WEBHOOK_DELIVERY_HISTORY",
            Class::WebhookEventHistory => "WEBHOOK_EVENT_HISTORY",
            Class::ApiUsageBatches => "API_USAGE_BATCHES",
            Class::ApiUsageCounters => "API_USAGE_COUNTERS",
            Class::IngestionRejections => "INGESTION_REJECTIONS",
            Class::ActivityEvents => "ACTIVITY_EVENTS",
        }
    }

    pub fn variable(&self) -> &'static str {
        match self {
            Class::WebhookDeliveryHistory => "DEVELOPER_PLATFORM_RETENTION_WEBHOOK_DELIVERY_DAYS",
            Class::WebhookEventHistory => "DEVELOPER_PLATFORM_RETENTION_WEBHOOK_EVENT_DAYS",
            Class::ApiUsageBatches => "DEVELOPER_PLATFORM_RETENTION_API_USAGE_BATCH_DAYS",
            Class::ApiUsageCounters => "DEVELOPER_PLATFORM_RETENTION_API_USAGE_DAYS",
            Class::IngestionRejections => "DEVELOPER_PLATFORM_RETENTION_REJECTION_DAYS",
            Class::ActivityEvents => "DEVELOPER_PLATFORM_RETENTION_ACTIVITY_DAYS",
        }
    }

    fn default_days(&self) -> u32 {
        match self {
            Class::WebhookDeliveryHistory | Class::WebhookEventHistory => 30,
            Class::ApiUsageBatches => 35,
            Class::ApiUsageCounters => 400,
            Class::IngestionRejections => 90,
            Class::ActivityEvents => 365,
        }
    }
}

/// Retention bounds (days; `None` = keep forever).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSettings {
    pub enabled: bool,
    bounds: Vec<(Class, Option<u32>)>,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            bounds: Class::ALL
                .iter()
                .map(|c| (*c, Some(c.default_days())))
                .collect(),
        }
    }
}

impl RetentionSettings {
    /// Off: nothing is purged.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let enabled = match get("DEVELOPER_PLATFORM_RETENTION_ENABLED")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("true") => true,
            Some("false") => false,
            Some(_) => {
                return Err("DEVELOPER_PLATFORM_RETENTION_ENABLED must be true or false".into())
            }
        };
        let mut bounds = Vec::new();
        for class in Class::ALL {
            let days = match get(class.variable()).map(|v| v.trim().to_string()) {
                None => Some(class.default_days()),
                Some(v) if v.is_empty() => Some(class.default_days()),
                Some(v) => match v.parse::<u32>() {
                    Ok(0) => None,
                    Ok(d) if d <= 3650 => Some(d),
                    _ => {
                        return Err(format!(
                            "{} must be 0 (keep) or 1-3650 days",
                            class.variable()
                        ))
                    }
                },
            };
            bounds.push((class, days));
        }
        Ok(Self { enabled, bounds })
    }

    pub fn days(&self, class: Class) -> Option<u32> {
        self.bounds
            .iter()
            .find(|(c, _)| *c == class)
            .and_then(|(_, d)| *d)
    }

    pub fn with_days(mut self, class: Class, days: Option<u32>) -> Self {
        for (c, d) in &mut self.bounds {
            if *c == class {
                *d = days;
            }
        }
        self
    }
}

/// Rows deleted per class in one run (classes without rows are absent).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub deleted: Vec<(Class, u64)>,
    pub failed: Vec<Class>,
}

impl RetentionReport {
    pub fn deleted(&self, class: Class) -> u64 {
        self.deleted
            .iter()
            .find(|(c, _)| *c == class)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

async fn exec<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<u64, DbErr> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}

/// The class's delete statements (`$1` = cutoff, `$2` = batch).
async fn purge_class<C: ConnectionTrait>(
    txn: &C,
    class: Class,
    cutoff: DateTime<Utc>,
    batch: i64,
) -> Result<u64, DbErr> {
    let values = || vec![cutoff.into(), batch.into()];
    match class {
        Class::WebhookDeliveryHistory => {
            // Terminal, completed before the cutoff, and not referenced by a replay that
            // stays (open, or completed after the cutoff).
            let ids: Vec<Uuid> = txn
                .query_all(stmt(
                    "SELECT d.id FROM devplat_webhook_deliveries d \
                      WHERE d.status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'SHADOW') \
                        AND d.completed_at < $1 \
                        AND NOT EXISTS (SELECT 1 FROM devplat_webhook_deliveries r \
                                         WHERE r.replay_of = d.id \
                                           AND (r.completed_at IS NULL OR r.completed_at >= $1)) \
                      ORDER BY d.completed_at, d.id LIMIT $2 FOR UPDATE SKIP LOCKED",
                    values(),
                ))
                .await?
                .iter()
                .map(|r| r.try_get_by_index::<Uuid>(0))
                .collect::<Result<_, _>>()?;
            if ids.is_empty() {
                return Ok(0);
            }
            exec(
                txn,
                "DELETE FROM devplat_webhook_attempts WHERE delivery_id = ANY($1)",
                vec![ids.clone().into()],
            )
            .await?;
            // A replay in the same batch is deleted together with its original (one
            // statement: the self reference is checked at its end). A replay that is
            // not due keeps its original (the claim above).
            exec(
                txn,
                "DELETE FROM devplat_webhook_deliveries WHERE id = ANY($1) \
                   AND NOT EXISTS (SELECT 1 FROM devplat_webhook_deliveries r \
                                    WHERE r.replay_of = devplat_webhook_deliveries.id \
                                      AND NOT (r.id = ANY($1)))",
                vec![ids.into()],
            )
            .await
        }
        Class::WebhookEventHistory => {
            exec(
                txn,
                "DELETE FROM devplat_webhook_events WHERE ctid IN ( \
                   SELECT e.ctid FROM devplat_webhook_events e \
                    WHERE e.received_at < $1 \
                      AND NOT EXISTS (SELECT 1 FROM devplat_webhook_deliveries d \
                                       WHERE d.tenant_id = e.tenant_id AND d.application_id = e.application_id \
                                         AND d.environment_id = e.environment_id AND d.event_id = e.event_id) \
                    ORDER BY e.received_at LIMIT $2 FOR UPDATE OF e SKIP LOCKED)",
                values(),
            )
            .await
        }
        Class::ApiUsageBatches => {
            exec(
                txn,
                "DELETE FROM devplat_api_usage_batches WHERE batch_id IN ( \
                   SELECT batch_id FROM devplat_api_usage_batches WHERE recorded_at < $1 \
                    ORDER BY recorded_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
                values(),
            )
            .await
        }
        Class::ApiUsageCounters => {
            let day = cutoff.date_naive();
            let daily = exec(
                txn,
                "DELETE FROM devplat_api_usage_daily WHERE (environment_id, usage_day) IN ( \
                   SELECT environment_id, usage_day FROM devplat_api_usage_daily WHERE usage_day < $1 \
                    ORDER BY usage_day LIMIT $2 FOR UPDATE SKIP LOCKED)",
                vec![day.into(), batch.into()],
            )
            .await?;
            let operations = exec(
                txn,
                "DELETE FROM devplat_api_operation_usage \
                  WHERE (environment_id, usage_day, domain, operation_id, status_class) IN ( \
                   SELECT environment_id, usage_day, domain, operation_id, status_class \
                     FROM devplat_api_operation_usage WHERE usage_day < $1 \
                    ORDER BY usage_day LIMIT $2 FOR UPDATE SKIP LOCKED)",
                vec![day.into(), batch.into()],
            )
            .await?;
            Ok(daily + operations)
        }
        Class::IngestionRejections => {
            let usage = exec(
                txn,
                "DELETE FROM devplat_usage_rejections WHERE ctid IN ( \
                   SELECT ctid FROM devplat_usage_rejections WHERE received_at < $1 \
                    ORDER BY received_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
                values(),
            )
            .await?;
            let events = exec(
                txn,
                "DELETE FROM devplat_event_rejections WHERE ctid IN ( \
                   SELECT ctid FROM devplat_event_rejections WHERE received_at < $1 \
                    ORDER BY received_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
                values(),
            )
            .await?;
            Ok(usage + events)
        }
        Class::ActivityEvents => {
            exec(
                txn,
                "DELETE FROM devplat_activity_events WHERE ctid IN ( \
                   SELECT ctid FROM devplat_activity_events WHERE occurred_at < $1 \
                    ORDER BY occurred_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
                values(),
            )
            .await
        }
    }
}

async fn audit<C: ConnectionTrait>(
    db: &C,
    class: Class,
    days: u32,
    rows: u64,
    outcome: &str,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let policy = format!(
        "{}={days}",
        class.variable().trim_start_matches("DEVELOPER_PLATFORM_")
    );
    exec(
        db,
        "INSERT INTO devplat_retention_audit (id, data_class, deleted_rows, outcome, policy, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        vec![
            Uuid::new_v4().into(),
            class.as_str().into(),
            (rows as i64).into(),
            outcome.into(),
            policy.into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

/// One purge run at `now`: every class with a bound, at most `batch` rows each.
pub async fn purge_once(
    db: &DatabaseConnection,
    settings: &RetentionSettings,
    now: DateTime<Utc>,
    batch: i64,
) -> RetentionReport {
    let mut report = RetentionReport::default();
    if !settings.enabled {
        return report;
    }
    for class in Class::ALL {
        let Some(days) = settings.days(class) else {
            continue;
        };
        let cutoff = now - chrono::Duration::days(days.into());
        let outcome: Result<u64, DbErr> = async {
            let txn = db.begin().await?;
            txn.execute_unprepared("SET LOCAL devplat.retention_worker = 'on'")
                .await?;
            let n = purge_class(&txn, class, cutoff, batch).await?;
            if n == 0 {
                txn.rollback().await?;
                return Ok(0);
            }
            audit(&txn, class, days, n, "DELETED", now).await?;
            txn.commit().await?;
            Ok(n)
        }
        .await;
        match outcome {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(data_class = class.as_str(), deleted = n, "retention purge");
                report.deleted.push((class, n));
            }
            Err(e) => {
                tracing::warn!(data_class = class.as_str(), error = %e, "retention purge failed; nothing applied, retried next run");
                let _ = audit(db, class, days, 0, "FAILED", now).await;
                report.failed.push(class);
            }
        }
    }
    report
}

/// The purge job of the worker: runs at most once per [`RETENTION_INTERVAL`] across
/// every instance.
pub async fn retention_pass(
    db: &DatabaseConnection,
    settings: &RetentionSettings,
    now: DateTime<Utc>,
) -> Option<RetentionReport> {
    if !settings.enabled {
        return None;
    }
    match crate::worker::claim_schedule(db, JOB, RETENTION_INTERVAL, now).await {
        Ok(true) => {}
        Ok(false) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "retention schedule unavailable");
            return None;
        }
    }
    let report = purge_once(db, settings, now, RETENTION_BATCH).await;
    let outcome = if report.failed.is_empty() {
        "SUCCEEDED"
    } else {
        "FAILED"
    };
    let _ = crate::worker::complete_schedule(db, JOB, outcome, now).await;
    Some(report)
}

/// Per-class totals of the last 30 days (operator health view).
pub async fn retention_summary<C: ConnectionTrait>(
    db: &C,
    now: DateTime<Utc>,
) -> Result<Value, DbErr> {
    let rows = db
        .query_all(stmt(
            "SELECT data_class, outcome, COALESCE(SUM(deleted_rows), 0)::bigint AS rows, COUNT(*)::bigint AS runs, \
                    MAX(occurred_at) AS last_at \
               FROM devplat_retention_audit WHERE occurred_at >= $1 \
              GROUP BY data_class, outcome ORDER BY data_class, outcome",
            vec![(now - chrono::Duration::days(30)).into()],
        ))
        .await?;
    let mut out = Vec::new();
    for r in rows {
        out.push(json!({
            "dataClass": r.try_get::<String>("", "data_class")?,
            "outcome": r.try_get::<String>("", "outcome")?,
            "deletedRows": r.try_get::<i64>("", "rows")?,
            "batches": r.try_get::<i64>("", "runs")?,
            "lastAt": r.try_get::<DateTime<Utc>>("", "last_at")?,
        }));
    }
    Ok(json!({ "windowDays": 30, "classes": out }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_bound_and_validate() {
        let s = RetentionSettings::from_lookup(&|_| None).unwrap();
        assert!(s.enabled);
        assert_eq!(s.days(Class::WebhookDeliveryHistory), Some(30));
        assert_eq!(s.days(Class::ActivityEvents), Some(365));
        let s = RetentionSettings::from_lookup(&|k: &str| match k {
            "DEVELOPER_PLATFORM_RETENTION_ACTIVITY_DAYS" => Some("0".into()),
            "DEVELOPER_PLATFORM_RETENTION_API_USAGE_DAYS" => Some("730".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(s.days(Class::ActivityEvents), None);
        assert_eq!(s.days(Class::ApiUsageCounters), Some(730));
        assert!(RetentionSettings::from_lookup(&|k: &str| {
            (k == "DEVELOPER_PLATFORM_RETENTION_REJECTION_DAYS").then(|| "9999".into())
        })
        .is_err());
        let off = RetentionSettings::from_lookup(&|k: &str| {
            (k == "DEVELOPER_PLATFORM_RETENTION_ENABLED").then(|| "false".into())
        })
        .unwrap();
        assert!(!off.enabled);
    }
}

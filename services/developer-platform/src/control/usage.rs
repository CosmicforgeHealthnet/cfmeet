//! Durable API request accounting (Section 51 quotas).
//!
//! The API Gateway flushes accepted request counts in batches identified by a batch id;
//! a batch is applied at most once (a replay after an unknown outcome is acknowledged
//! without counting again). The daily total is what the gateway re-seeds its shared day
//! window from after a counter-store loss. The same batches carry the normalized
//! API_OPERATION counts (domain / contract operation id / status class) of Appendix D.

use chrono::{NaiveDate, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::http::error::{ApiError, ErrorCode};

/// Upper bound of entries per batch and of one entry's count.
pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_COUNT: u64 = 1_000_000_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageBatch {
    pub batch_id: Uuid,
    /// Daily accepted-request totals per environment.
    #[serde(default)]
    pub entries: Vec<UsageEntry>,
    /// Normalized API_OPERATION counts (domain / operation / status class).
    #[serde(default)]
    pub operations: Vec<OperationEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationEntry {
    pub environment_id: Uuid,
    pub day: String,
    pub domain: String,
    pub operation_id: String,
    pub status_class: String,
    pub count: u64,
}

fn valid_domain(d: &str) -> bool {
    (1..=40).contains(&d.len())
        && d.as_bytes()[0].is_ascii_lowercase()
        && d.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
}

fn valid_operation(o: &str) -> bool {
    (1..=120).contains(&o.len())
        && o.as_bytes()[0].is_ascii_lowercase()
        && o.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageEntry {
    pub environment_id: Uuid,
    pub day: String,
    pub count: u64,
}

pub fn parse_day(day: &str) -> Result<NaiveDate, ApiError> {
    if day.len() != 10 {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    NaiveDate::parse_from_str(day, "%Y-%m-%d").map_err(|_| ApiError(ErrorCode::ValidationError))
}

/// Applies `batch` exactly once (`RECORDED`, or `ALREADY_RECORDED` for a replay).
pub async fn record<C: TransactionTrait>(
    db: &C,
    recorded_by: &str,
    batch: UsageBatch,
) -> Result<Value, ApiError> {
    let total = batch.entries.len() + batch.operations.len();
    if total == 0 || total > MAX_ENTRIES {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let mut parsed = Vec::with_capacity(batch.entries.len());
    for e in &batch.entries {
        if e.count == 0 || e.count > MAX_COUNT {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        parsed.push((e.environment_id, parse_day(&e.day)?, e.count as i64));
    }
    let mut operations = Vec::with_capacity(batch.operations.len());
    for o in &batch.operations {
        if o.count == 0
            || o.count > MAX_COUNT
            || !valid_domain(&o.domain)
            || !valid_operation(&o.operation_id)
            || !matches!(o.status_class.as_str(), "2xx" | "3xx" | "4xx" | "5xx")
        {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        operations.push((o.environment_id, parse_day(&o.day)?, o, o.count as i64));
    }
    let now = Utc::now();
    let txn = db.begin().await?;
    let inserted = txn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO devplat_api_usage_batches (batch_id, recorded_by, entry_count, recorded_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (batch_id) DO NOTHING",
            vec![batch.batch_id.into(), recorded_by.into(), (total as i32).into(), now.into()],
        ))
        .await?
        .rows_affected();
    if inserted == 0 {
        txn.rollback().await?;
        return Ok(json!({ "batchId": batch.batch_id, "outcome": "ALREADY_RECORDED" }));
    }
    for (environment, day, count) in parsed {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO devplat_api_usage_daily (environment_id, usage_day, request_count, updated_at) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (environment_id, usage_day) DO UPDATE \
             SET request_count = devplat_api_usage_daily.request_count + EXCLUDED.request_count, \
                 updated_at = EXCLUDED.updated_at",
            vec![environment.into(), day.into(), count.into(), now.into()],
        ))
        .await?;
    }
    for (environment, day, o, count) in operations {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO devplat_api_operation_usage \
                 (environment_id, usage_day, domain, operation_id, status_class, request_count, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (environment_id, usage_day, domain, operation_id, status_class) DO UPDATE \
             SET request_count = devplat_api_operation_usage.request_count + EXCLUDED.request_count, \
                 updated_at = EXCLUDED.updated_at",
            vec![
                environment.into(),
                day.into(),
                o.domain.clone().into(),
                o.operation_id.clone().into(),
                o.status_class.clone().into(),
                count.into(),
                now.into(),
            ],
        ))
        .await?;
    }
    txn.commit().await?;
    Ok(json!({ "batchId": batch.batch_id, "outcome": "RECORDED" }))
}

/// Durable accepted-request total of `environment` on `day` (0 when none).
pub async fn daily<C: ConnectionTrait>(
    db: &C,
    environment: Uuid,
    day: &str,
) -> Result<Value, ApiError> {
    let date = parse_day(day)?;
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT request_count FROM devplat_api_usage_daily WHERE environment_id = $1 AND usage_day = $2",
            vec![environment.into(), date.into()],
        ))
        .await?;
    let count: i64 = match row {
        Some(r) => r.try_get("", "request_count")?,
        None => 0,
    };
    Ok(json!({ "environmentId": environment, "day": day, "count": count }))
}

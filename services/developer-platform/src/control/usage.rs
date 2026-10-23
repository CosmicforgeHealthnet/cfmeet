//! Durable API request accounting (Section 51 quotas).
//!
//! The API Gateway flushes accepted request counts in batches identified by a batch id;
//! a batch is applied at most once (a replay after an unknown outcome is acknowledged
//! without counting again). The daily total is what the gateway re-seeds its shared day
//! window from after a counter-store loss.

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
    pub entries: Vec<UsageEntry>,
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
    if batch.entries.is_empty() || batch.entries.len() > MAX_ENTRIES {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let mut parsed = Vec::with_capacity(batch.entries.len());
    for e in &batch.entries {
        if e.count == 0 || e.count > MAX_COUNT {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        parsed.push((e.environment_id, parse_day(&e.day)?, e.count as i64));
    }
    let now = Utc::now();
    let txn = db.begin().await?;
    let inserted = txn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO devplat_api_usage_batches (batch_id, recorded_by, entry_count, recorded_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (batch_id) DO NOTHING",
            vec![batch.batch_id.into(), recorded_by.into(), (parsed.len() as i32).into(), now.into()],
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

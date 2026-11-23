//! The admin bridge's daily dataset pull (ADR 0068 A7, brief §3):
//! `GET /internal/v1/operator/datasets/{datasetId}/days/{yyyy-mm-dd}`.
//!
//! Only `cfmeet.admin-bridge` holding `messaging-firstparty.administer`, with a named
//! operator (the bridge sends `X-CF-Operator: hq:dataset-sync`). The answer is
//! `{"datasetId", "date", "metrics": {"<name>": int|null}}`: counts only, computed for
//! that UTC day from data Messaging already keeps, platform-wide (every environment).
//!
//! | Dataset | Metric | Source |
//! |---|---|---|
//! | `cfmeet.messaging_daily` | `messages_sent` | accepted application messages of the day (`MESSAGE_ACCEPTED` usage facts, kept durably; E2EE and MANAGED alike, never content) |
//! | `cfmeet.operations_health` | `failed_messages` | committed events of the day that are dead letters now (the A2 failed-messages read) |
//!
//! A metric is `null`, never a guessed 0, when it cannot be computed: the day has not
//! ended yet, or its source did not exist for the whole day (the day began before the
//! migration that created the source was applied). A day in the future is 400; an unknown
//! dataset is 404. Read-only.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Map, Value};

use crate::{
    db,
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    operator_reads::require_bridge,
    state::AppState,
};

pub const MESSAGING_DAILY: &str = "cfmeet.messaging_daily";
pub const OPERATIONS_HEALTH: &str = "cfmeet.operations_health";

/// Routes nested under `/internal/v1`.
pub fn routes() -> Router<AppState> {
    Router::new().route("/operator/datasets/:datasetId/days/:day", get(day))
}

/// A metric Messaging computes, and the migration that created its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    MessagesSent,
    FailedMessages,
}

impl Metric {
    pub fn name(self) -> &'static str {
        match self {
            Metric::MessagesSent => "messages_sent",
            Metric::FailedMessages => "failed_messages",
        }
    }

    /// The `messaging_seaql_migrations` version that created the metric's source.
    fn source_migration(self) -> &'static str {
        match self {
            Metric::MessagesSent => "m20261113_000011_engagement_usage",
            Metric::FailedMessages => "m20261101_000001_messaging_foundation",
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Metric::MessagesSent => {
                "SELECT COALESCE(sum(quantity), 0)::bigint AS n FROM messaging_usage_records \
                  WHERE usage_type = 'MESSAGE_ACCEPTED' AND occurred_at >= $1 AND occurred_at < $2"
            }
            Metric::FailedMessages => {
                "SELECT count(*)::bigint AS n FROM messaging_outbox_events \
                  WHERE status = 'DEAD_LETTER' AND occurred_at >= $1 AND occurred_at < $2"
            }
        }
    }
}

/// The metrics of a dataset Messaging reports (`None`: not one of its datasets).
pub fn metrics_of(dataset: &str) -> Option<&'static [Metric]> {
    match dataset {
        MESSAGING_DAILY => Some(&[Metric::MessagesSent]),
        OPERATIONS_HEALTH => Some(&[Metric::FailedMessages]),
        _ => None,
    }
}

/// A strict `yyyy-mm-dd` day.
pub fn parse_day(raw: &str) -> Option<NaiveDate> {
    let d = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    (d.format("%Y-%m-%d").to_string() == raw).then_some(d)
}

#[derive(FromQueryResult)]
struct Count {
    n: i64,
}

#[derive(FromQueryResult)]
struct Applied {
    applied_at: i64,
}

/// One metric for `[start, end)`, or `None` when it cannot be computed.
async fn compute<C: ConnectionTrait>(
    db: &C,
    metric: Metric,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<i64>, ApiError> {
    if end > now {
        return Ok(None);
    }
    let applied: Option<Applied> = db::one(
        db,
        &format!(
            "SELECT applied_at FROM {} WHERE version = $1",
            messaging_migration::MIGRATION_TABLE
        ),
        vec![metric.source_migration().into()],
    )
    .await?;
    match applied.and_then(|a| Utc.timestamp_opt(a.applied_at, 0).single()) {
        Some(at) if at <= start => {}
        _ => return Ok(None),
    }
    let c: Option<Count> = db::one(db, metric.sql(), vec![start.into(), end.into()]).await?;
    Ok(c.map(|c| c.n))
}

/// `GET /operator/datasets/{datasetId}/days/{day}`.
async fn day(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path((dataset, raw_day)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let metrics = metrics_of(&dataset).ok_or(Error::code(ErrorCode::NotFound))?;
    let date = parse_day(&raw_day)
        .ok_or_else(|| Error::validation("the day must be a calendar date yyyy-mm-dd"))?;
    let start = date
        .and_hms_opt(0, 0, 0)
        .map(|d| d.and_utc())
        .ok_or_else(|| Error::validation("the day must be a calendar date yyyy-mm-dd"))?;
    let end = start + Duration::days(1);
    let now = state.now();
    if start > now {
        return Err(Error::validation("the day must not be in the future"));
    }
    let mut out = Map::new();
    for m in metrics {
        out.insert(
            m.name().into(),
            json!(compute(&state.db, *m, start, end, now).await?),
        );
    }
    Ok(Json(json!({
        "datasetId": dataset,
        "date": raw_day,
        "metrics": out,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_messagings_datasets_and_metrics_are_reported() {
        assert_eq!(
            metrics_of(MESSAGING_DAILY).unwrap()[0].name(),
            "messages_sent"
        );
        assert_eq!(
            metrics_of(OPERATIONS_HEALTH).unwrap()[0].name(),
            "failed_messages"
        );
        assert!(metrics_of("cfmeet.trust_safety_queue").is_none());
        assert!(metrics_of("").is_none());
    }

    #[test]
    fn days_are_strict_calendar_dates() {
        assert!(parse_day("2026-11-20").is_some());
        for bad in [
            "2026-11-31",
            "2026-1-20",
            "20261120",
            "2026-11-20T00:00:00Z",
            "",
        ] {
            assert!(parse_day(bad).is_none(), "{bad}");
        }
    }
}

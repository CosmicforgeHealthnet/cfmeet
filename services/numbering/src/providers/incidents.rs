//! Operational incidents (`numbering_incidents`): de-duplicated per key while OPEN, so a
//! recurring problem counts occurrences instead of flooding operators. Detail is bounded
//! and never carries a number, credential or customer data (database CHECK).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::numbers::stmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    ProviderStateDrift,
    UnknownProviderOutcome,
    CallbackAuthenticationFailed,
    UnmatchedProviderEvent,
    ProviderReleasedUnexpectedly,
    ReconciliationConflict,
    ProviderOutage,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::ProviderStateDrift,
        Category::UnknownProviderOutcome,
        Category::CallbackAuthenticationFailed,
        Category::UnmatchedProviderEvent,
        Category::ProviderReleasedUnexpectedly,
        Category::ReconciliationConflict,
        Category::ProviderOutage,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Category::ProviderStateDrift => "PROVIDER_STATE_DRIFT",
            Category::UnknownProviderOutcome => "UNKNOWN_PROVIDER_OUTCOME",
            Category::CallbackAuthenticationFailed => "CALLBACK_AUTHENTICATION_FAILED",
            Category::UnmatchedProviderEvent => "UNMATCHED_PROVIDER_EVENT",
            Category::ProviderReleasedUnexpectedly => "PROVIDER_RELEASED_UNEXPECTEDLY",
            Category::ReconciliationConflict => "RECONCILIATION_CONFLICT",
            Category::ProviderOutage => "PROVIDER_OUTAGE",
        }
    }

    fn severity(&self) -> &'static str {
        match self {
            Category::ProviderReleasedUnexpectedly | Category::ProviderStateDrift => "HIGH",
            Category::ReconciliationConflict | Category::UnknownProviderOutcome => "MEDIUM",
            Category::CallbackAuthenticationFailed | Category::ProviderOutage => "MEDIUM",
            Category::UnmatchedProviderEvent => "LOW",
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentRow {
    pub id: Uuid,
    pub dedupe_key: String,
    pub category: String,
    pub severity: String,
    pub status: String,
    pub provider_id: Option<String>,
    pub number_resource_id: Option<Uuid>,
    pub command_id: Option<Uuid>,
    pub occurrences: i32,
    pub detail: Value,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<String>,
}

/// What an incident is about.
#[derive(Debug, Clone, Default)]
pub struct Subject {
    pub provider_id: Option<String>,
    pub number_resource_id: Option<Uuid>,
    pub command_id: Option<Uuid>,
}

/// Opens (or counts another occurrence of) the OPEN incident with `dedupe_key`.
pub async fn raise<C: ConnectionTrait>(
    db: &C,
    category: Category,
    dedupe_key: &str,
    subject: Subject,
    detail: Value,
    now: DateTime<Utc>,
) -> Result<Uuid, DbErr> {
    crate::metrics::inc(
        "numbering_incidents_total",
        &[("category", category.as_str())],
    );
    tracing::warn!(
        category = category.as_str(),
        dedupe_key,
        "numbering incident"
    );
    let row = db
        .query_one(stmt(
            "INSERT INTO numbering_incidents (id, dedupe_key, category, severity, status, provider_id, number_resource_id, \
               command_id, detail, first_seen_at, last_seen_at) \
             VALUES ($1, $2, $3, $4, 'OPEN', $5, $6, $7, $8, $9, $9) \
             ON CONFLICT (dedupe_key) WHERE status = 'OPEN' \
             DO UPDATE SET occurrences = numbering_incidents.occurrences + 1, last_seen_at = EXCLUDED.last_seen_at, \
               detail = EXCLUDED.detail \
             RETURNING id",
            vec![
                Uuid::new_v4().into(),
                dedupe_key.into(),
                category.as_str().into(),
                category.severity().into(),
                subject.provider_id.into(),
                subject.number_resource_id.into(),
                subject.command_id.into(),
                detail.into(),
                now.into(),
            ],
        ))
        .await?
        .expect("upsert returns the id");
    row.try_get_by_index(0)
}

/// Resolves the OPEN incident with `dedupe_key` (no-op when none is open).
pub async fn resolve<C: ConnectionTrait>(
    db: &C,
    dedupe_key: &str,
    by: &str,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    Ok(db
        .execute(stmt(
            "UPDATE numbering_incidents SET status = 'RESOLVED', resolved_at = $2, resolved_by = $3 \
             WHERE dedupe_key = $1 AND status = 'OPEN'",
            vec![dedupe_key.into(), now.into(), by.into()],
        ))
        .await?
        .rows_affected()
        > 0)
}

/// Incidents, newest first (operators, tests).
pub async fn list<C: ConnectionTrait>(db: &C, limit: u64) -> Result<Vec<IncidentRow>, DbErr> {
    IncidentRow::find_by_statement(stmt(
        "SELECT * FROM numbering_incidents ORDER BY last_seen_at DESC, id LIMIT $1",
        vec![(limit.clamp(1, 1_000) as i64).into()],
    ))
    .all(db)
    .await
}

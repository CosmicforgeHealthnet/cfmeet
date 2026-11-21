//! ADR 0068 A7 — the daily datasets the HQ admin bridge pulls
//! (`GET /internal/v1/operator/datasets/{datasetId}/days/{yyyy-mm-dd}`, contract
//! `datasets.v1.json`). Counts only, for one UTC day, computed from existing data; a metric
//! that cannot be computed is `null`, never a guessed 0.
//!
//! Every count is a snapshot at the END of the day (`T` = the next midnight UTC, or now
//! for today), reconstructed from append-only history:
//!
//! * `cfmeet.partners_daily`
//!   * `workspaces_active` / `workspaces_restricted` / `workspaces_suspended`: workspaces
//!     created before `T` in that state at `T`. The state at `T` is the `fromState` of the
//!     first state change at or after `T` (operator state changes, owner closure, the owner
//!     activation of a first-party workspace), else the current state. `null` when `T`
//!     is older than the activity history kept (`DEVELOPER_PLATFORM_RETENTION_ACTIVITY_DAYS`,
//!     365 days by default), since state changes may have been purged.
//!   * `partners_live`: workspaces with a production-access request LIVE at `T` (the
//!     request transition history, never purged).
//!   * `partner_mau`: `null` — partner monthly active users are not metered (the usage
//!     ledger has no active-user fact).
//! * `cfmeet.apps_daily`
//!   * `apps_draft` (DRAFT), `apps_in_review` (SUBMITTED or IN_REVIEW), `apps_published`
//!     (PUBLISHED), `apps_suspended` (SUSPENDED): apps by their review status at `T` (the
//!     app transition history, never purged).
//!   * `installs`: installations active at `T` (installed before `T`, not revoked before
//!     `T`); `null` while the installation projection is off
//!     (`DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED`).

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{json, Map, Value};

use super::stmt;
use crate::http::error::{ApiError, ErrorCode};

pub const PARTNERS_DAILY: &str = "cfmeet.partners_daily";
pub const APPS_DAILY: &str = "cfmeet.apps_daily";

/// What a dataset read needs besides the database.
#[derive(Debug, Clone, Copy)]
pub struct DatasetContext {
    /// The installation projection runs (installs are known).
    pub installs_known: bool,
    /// Days of activity history kept (`None`: kept forever).
    pub activity_days: Option<u32>,
    pub now: DateTime<Utc>,
}

/// The end-of-day cutoff of `day` (`None`: a future day).
pub fn cutoff(day: NaiveDate, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let start = day.and_hms_opt(0, 0, 0)?.and_utc();
    if start > now {
        return None;
    }
    Some((start + Duration::days(1)).min(now))
}

fn count(r: &sea_orm::QueryResult, col: &str) -> Result<Value, ApiError> {
    Ok(Value::from(r.try_get::<i64>("", col)?))
}

/// The answer `{datasetId, date, metrics}` (NOT_FOUND: unknown dataset; VALIDATION_ERROR:
/// a future day).
pub async fn day(
    db: &DatabaseConnection,
    dataset: &str,
    date: NaiveDate,
    ctx: DatasetContext,
) -> Result<Value, ApiError> {
    if dataset != PARTNERS_DAILY && dataset != APPS_DAILY {
        return Err(ApiError(ErrorCode::NotFound));
    }
    let t = cutoff(date, ctx.now).ok_or(ApiError(ErrorCode::ValidationError))?;
    let mut metrics = Map::new();
    if dataset == PARTNERS_DAILY {
        let history_kept = ctx
            .activity_days
            .is_none_or(|d| t >= ctx.now - Duration::days(i64::from(d)));
        if history_kept {
            let r = db
                .query_one(stmt(
                    "WITH s AS ( \
                       SELECT COALESCE(( \
                         SELECT e.st FROM ( \
                           SELECT a.occurred_at AS at, a.summary->>'fromState' AS st \
                             FROM devplat_activity_events a \
                            WHERE a.tenant_id = o.tenant_id AND a.outcome = 'SUCCEEDED' \
                              AND a.action IN ('organization.state_changed', 'organization.closed') \
                              AND a.occurred_at >= $1 \
                           UNION ALL \
                           SELECT min(i.decided_at), 'INVITED' FROM devplat_invitations i \
                            WHERE i.tenant_id = o.tenant_id AND o.created_by LIKE 'operator:%' \
                              AND i.role = 'OWNER' AND i.status = 'ACCEPTED' \
                           HAVING min(i.decided_at) >= $1 \
                         ) e WHERE e.st IS NOT NULL ORDER BY e.at LIMIT 1), o.state) AS st \
                       FROM devplat_organizations o WHERE o.created_at < $1) \
                     SELECT count(*) FILTER (WHERE st = 'ACTIVE') AS active, \
                            count(*) FILTER (WHERE st = 'RESTRICTED') AS restricted, \
                            count(*) FILTER (WHERE st = 'SUSPENDED') AS suspended FROM s",
                    vec![t.into()],
                ))
                .await?
                .ok_or(ApiError(ErrorCode::InternalError))?;
            metrics.insert("workspaces_active".into(), count(&r, "active")?);
            metrics.insert("workspaces_restricted".into(), count(&r, "restricted")?);
            metrics.insert("workspaces_suspended".into(), count(&r, "suspended")?);
        } else {
            for k in [
                "workspaces_active",
                "workspaces_restricted",
                "workspaces_suspended",
            ] {
                metrics.insert(k.into(), Value::Null);
            }
        }
        let r = db
            .query_one(stmt(
                "WITH s AS (SELECT DISTINCT ON (request_id) tenant_id, to_state \
                              FROM devplat_production_access_transitions WHERE occurred_at < $1 \
                             ORDER BY request_id, occurred_at DESC, id DESC) \
                 SELECT count(DISTINCT tenant_id) FILTER (WHERE to_state = 'LIVE') AS live FROM s",
                vec![t.into()],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?;
        metrics.insert("partners_live".into(), count(&r, "live")?);
        metrics.insert("partner_mau".into(), Value::Null);
    } else {
        let r = db
            .query_one(stmt(
                "WITH s AS (SELECT DISTINCT ON (app_id) to_status FROM devplat_app_transitions \
                             WHERE occurred_at < $1 ORDER BY app_id, occurred_at DESC, seq DESC) \
                 SELECT count(*) FILTER (WHERE to_status = 'DRAFT') AS draft, \
                        count(*) FILTER (WHERE to_status IN ('SUBMITTED', 'IN_REVIEW')) AS in_review, \
                        count(*) FILTER (WHERE to_status = 'PUBLISHED') AS published, \
                        count(*) FILTER (WHERE to_status = 'SUSPENDED') AS suspended FROM s",
                vec![t.into()],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::InternalError))?;
        metrics.insert("apps_draft".into(), count(&r, "draft")?);
        metrics.insert("apps_in_review".into(), count(&r, "in_review")?);
        metrics.insert("apps_published".into(), count(&r, "published")?);
        metrics.insert("apps_suspended".into(), count(&r, "suspended")?);
        let installs = if ctx.installs_known {
            let r = db
                .query_one(stmt(
                    "SELECT count(*) AS n FROM devplat_app_installations \
                      WHERE installed_at < $1 AND (status = 'ACTIVE' OR updated_at >= $1)",
                    vec![t.into()],
                ))
                .await?
                .ok_or(ApiError(ErrorCode::InternalError))?;
            count(&r, "n")?
        } else {
            Value::Null
        };
        metrics.insert("installs".into(), installs);
    }
    Ok(json!({
        "datasetId": dataset,
        "date": date.format("%Y-%m-%d").to_string(),
        "metrics": metrics,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cutoff_is_the_end_of_the_day_or_now() {
        let now = DateTime::parse_from_rfc3339("2026-11-21T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(
            cutoff(d("2026-11-20"), now).unwrap().to_rfc3339(),
            "2026-11-21T00:00:00+00:00"
        );
        assert_eq!(cutoff(d("2026-11-21"), now), Some(now));
        assert_eq!(cutoff(d("2026-11-22"), now), None);
    }
}

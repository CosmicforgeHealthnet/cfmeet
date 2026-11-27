//! ADR 0068 A7 — the daily dataset pull (design §17.3, brief 2 §3): the admin bridge pulls
//! one UTC day of counts at a time (`X-CF-Operator: hq:dataset-sync`).
//!
//! `GET /internal/v1/operator/datasets/{datasetId}/days/{yyyy-mm-dd}` answers
//! `{"datasetId", "date", "metrics": {"<name>": int | null}}`, computed for that UTC day from
//! the data the Meeting Service already holds. Counts only, never an id or a name. A metric
//! that cannot be computed is `null`, never a guessed 0.
//!
//! | Dataset | Metric | Definition |
//! |---|---|---|
//! | `cfmeet.engagement_daily` | `dau` | distinct registered (non-guest) accounts that signed in (`account_sessions.authenticated_at`) or were connected to a conference (`participant_sessions.connected_at`) during the day |
//! | | `mau` | the same over the 30 UTC days ending with the day |
//! | | `signups` | non-guest accounts created during the day (`users.created_at`) |
//! | `cfmeet.meetings_daily` | `meetings` | conferences started during the day (`conference_sessions.started_at`) |
//! | | `participant_minutes` | connected participant time within the day, in whole minutes (sessions with a known end, or still CONNECTED, clipped to the day) |
//! | | `peak_live_meetings` | the most conferences live at the same instant during the day (started and not yet ended; still LIVE / ENDING ones up to now) |
//!
//! `engagement_daily` needs `account.administer`, `meetings_daily` `meeting.administer`
//! (the bridge holds both). An unknown dataset is `404`; a malformed date or a day after
//! today (UTC) is `400`. Today answers the counts so far.

use axum::{
    extract::{Path, State},
    http::request::Parts,
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Map, Value};

use super::{authenticate, not_found, ok, unavailable, validation, OperatorState};

pub const DATASET_DAY: &str = "/internal/v1/operator/datasets/:dataset_id/days/:date";

pub const ENGAGEMENT_DAILY: &str = "cfmeet.engagement_daily";
pub const MEETINGS_DAILY: &str = "cfmeet.meetings_daily";

/// The metrics of each dataset (`services/admin-bridge/docs/contract/datasets.v1.json`).
pub fn metrics_of(dataset: &str) -> Option<&'static [&'static str]> {
    match dataset {
        ENGAGEMENT_DAILY => Some(&["dau", "mau", "signups"]),
        MEETINGS_DAILY => Some(&["meetings", "participant_minutes", "peak_live_meetings"]),
        _ => None,
    }
}

/// `engagement_daily`: `account.administer`; every other (and an unknown) dataset:
/// `meeting.administer`.
fn permission_of(dataset: &str) -> InternalPermission {
    if dataset == ENGAGEMENT_DAILY {
        InternalPermission::AccountAdminister
    } else {
        InternalPermission::MeetingAdminister
    }
}

/// `yyyy-mm-dd`, not after `today`.
pub fn parse_day(raw: &str, today: NaiveDate) -> Option<NaiveDate> {
    if raw.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .filter(|d| *d <= today)
}

/// `[start, end)` of a UTC day.
fn bounds(day: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = day.and_hms_opt(0, 0, 0).expect("midnight").and_utc();
    (start, start + Duration::days(1))
}

/// Distinct registered accounts active in `[from, to)`.
const ACTIVE_SQL: &str = "SELECT count(DISTINCT a.user_id)::bigint AS n FROM ( \
       SELECT s.user_id FROM account_sessions s \
        WHERE s.authenticated_at >= $1 AND s.authenticated_at < $2 \
       UNION \
       SELECT p.user_id FROM participant_sessions ps \
         JOIN conference_participants p ON p.id = ps.conference_participant_id \
        WHERE p.user_id IS NOT NULL AND ps.connected_at >= $1 AND ps.connected_at < $2 \
     ) a JOIN users u ON u.id = a.user_id WHERE u.role::text <> 'guest'";

const SIGNUPS_SQL: &str = "SELECT count(*)::bigint AS n FROM users \
     WHERE role::text <> 'guest' AND created_at >= $1 AND created_at < $2";

const MEETINGS_SQL: &str = "SELECT count(*)::bigint AS n FROM conference_sessions \
     WHERE started_at >= $1 AND started_at < $2";

const PARTICIPANT_MINUTES_SQL: &str = "SELECT floor(COALESCE(sum(EXTRACT(EPOCH FROM ( \
         LEAST(COALESCE(disconnected_at, CASE WHEN state = 'CONNECTED' THEN now() END), $2) \
         - GREATEST(connected_at, $1)))), 0) / 60)::bigint AS n \
       FROM participant_sessions \
      WHERE connected_at IS NOT NULL AND connected_at < $2 \
        AND COALESCE(disconnected_at, CASE WHEN state = 'CONNECTED' THEN now() END) > $1 \
        AND COALESCE(disconnected_at, CASE WHEN state = 'CONNECTED' THEN now() END) >= connected_at";

/// Sweep line over the day's live intervals (ends sort before starts at the same instant).
const PEAK_SQL: &str = "WITH iv AS ( \
       SELECT GREATEST(started_at, $1) AS s, \
              LEAST(COALESCE(ended_at, CASE WHEN status::text IN ('LIVE', 'ENDING') THEN now() END), $2) AS e \
         FROM conference_sessions \
        WHERE started_at IS NOT NULL AND started_at < $2 \
          AND COALESCE(ended_at, CASE WHEN status::text IN ('LIVE', 'ENDING') THEN now() END) > $1 \
     ), ev AS ( \
       SELECT s AS t, 1 AS d FROM iv WHERE e > s \
       UNION ALL SELECT e AS t, -1 AS d FROM iv WHERE e > s \
     ) \
     SELECT COALESCE(max(live), 0)::bigint AS n FROM ( \
       SELECT sum(d) OVER (ORDER BY t, d ROWS UNBOUNDED PRECEDING) AS live FROM ev \
     ) r";

async fn count<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    from: sea_orm::Value,
    to: sea_orm::Value,
) -> Option<i64> {
    match db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            vec![from, to],
        ))
        .await
    {
        Ok(Some(r)) => r.try_get::<i64>("", "n").ok().filter(|n| *n >= 0),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("dataset metric not computable: {e}");
            None
        }
    }
}

/// The metrics of `dataset` for `day` (`None` for an unknown dataset). A metric whose query
/// fails is `null`; the answer is `Err` only when nothing could be computed.
pub async fn compute<C: ConnectionTrait>(
    db: &C,
    dataset: &str,
    day: NaiveDate,
) -> Option<Result<Map<String, Value>, ()>> {
    let (start, end) = bounds(day);
    let mut m = Map::new();
    let put = |m: &mut Map<String, Value>, k: &str, v: Option<i64>| {
        m.insert(k.to_string(), v.map(Value::from).unwrap_or(Value::Null));
    };
    match dataset {
        ENGAGEMENT_DAILY => {
            put(
                &mut m,
                "dau",
                count(db, ACTIVE_SQL, start.into(), end.into()).await,
            );
            put(
                &mut m,
                "mau",
                count(
                    db,
                    ACTIVE_SQL,
                    (end - Duration::days(30)).into(),
                    end.into(),
                )
                .await,
            );
            // `users.created_at` is a UTC timestamp without a zone.
            put(
                &mut m,
                "signups",
                count(
                    db,
                    SIGNUPS_SQL,
                    start.naive_utc().into(),
                    end.naive_utc().into(),
                )
                .await,
            );
        }
        MEETINGS_DAILY => {
            put(
                &mut m,
                "meetings",
                count(db, MEETINGS_SQL, start.into(), end.into()).await,
            );
            put(
                &mut m,
                "participant_minutes",
                count(db, PARTICIPANT_MINUTES_SQL, start.into(), end.into()).await,
            );
            put(
                &mut m,
                "peak_live_meetings",
                count(db, PEAK_SQL, start.into(), end.into()).await,
            );
        }
        _ => return None,
    }
    if m.values().all(Value::is_null) {
        return Some(Err(()));
    }
    Some(Ok(m))
}

/// `GET /internal/v1/operator/datasets/{datasetId}/days/{yyyy-mm-dd}`.
pub(super) async fn dataset_day(
    State(s): State<OperatorState>,
    Path((dataset, date)): Path<(String, String)>,
    req: Parts,
) -> Response {
    // Authenticated first (an unknown dataset tells nothing to an unknown caller); the
    // token is verified once.
    let rid = match authenticate(&s, &req, permission_of(&dataset)).await {
        Ok(ctx) => ctx.request_id().as_str().to_string(),
        Err(r) => return *r,
    };
    if metrics_of(&dataset).is_none() {
        return not_found(&rid);
    }
    let Some(day) = parse_day(&date, Utc::now().date_naive()) else {
        return validation("date must be a UTC day (yyyy-mm-dd), not after today", &rid);
    };
    match compute(&s.app.db, &dataset, day).await {
        Some(Ok(metrics)) => ok(json!({
            "datasetId": dataset,
            "date": day.format("%Y-%m-%d").to_string(),
            "metrics": metrics,
        })),
        Some(Err(())) => unavailable(&rid),
        None => not_found(&rid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_utc_dates_not_in_the_future() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        assert_eq!(
            parse_day("2026-11-20", today),
            NaiveDate::from_ymd_opt(2026, 11, 20)
        );
        assert_eq!(parse_day("2026-11-21", today), Some(today));
        assert_eq!(parse_day("2026-11-22", today), None);
        assert_eq!(parse_day("2026-11-2", today), None);
        assert_eq!(parse_day("2026-02-30", today), None);
        assert_eq!(parse_day("20261120", today), None);
        let (s, e) = bounds(today);
        assert_eq!(e - s, Duration::days(1));
        assert_eq!(rfc(s), "2026-11-21T00:00:00Z");
    }

    fn rfc(t: DateTime<Utc>) -> String {
        t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    #[test]
    fn the_metric_names_are_the_bridge_contract() {
        assert_eq!(
            metrics_of(ENGAGEMENT_DAILY).unwrap(),
            &["dau", "mau", "signups"]
        );
        assert_eq!(
            metrics_of(MEETINGS_DAILY).unwrap(),
            &["meetings", "participant_minutes", "peak_live_meetings"]
        );
        assert!(metrics_of("cfmeet.messaging_daily").is_none());
    }
}

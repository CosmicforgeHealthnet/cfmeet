//! The admin bridge's operator reads (ADR 0068, design §6.5): `/internal/v1/communications/operator/*`.
//!
//! Only `cfmeet.admin-bridge` can hold `communications.administer`, and every request must
//! name the HQ staff member it reads for (`X-CF-Operator`; refused otherwise). The reads are
//! operator-wide (not bound to one delegated platform scope) and expose identifiers,
//! reasons, states, counts and times only: never a phone number, an email address, an
//! account reference or a report's free text (reports carry none).

use crate::{
    error::{Error, Result},
    store::{one, rows},
    State,
};
use axum::{
    extract::{Path, Query, State as HttpState},
    Extension, Json,
};
use cf_service_auth::OperatorAttribution;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// The parsed operator attribution of a request (set by the auth middleware for every
/// request; `None` for callers that sent no attribution headers).
#[derive(Clone, Debug, Default)]
pub struct Operator(pub Option<OperatorAttribution>);

fn require(op: &Operator) -> Result<()> {
    op.0.as_ref().map(|_| ()).ok_or(Error::Forbidden)
}

fn limit(value: Option<i64>) -> Result<i64> {
    let l = value.unwrap_or(50);
    if (1..=100).contains(&l) {
        Ok(l)
    } else {
        Err(Error::Validation)
    }
}

/// `<micros>.<uuid>` keyset cursor (time then id).
fn parse_time_cursor(cursor: Option<&str>) -> Result<Option<(i64, Uuid)>> {
    match cursor {
        None => Ok(None),
        Some(c) => {
            let (t, id) = c.split_once('.').ok_or(Error::Validation)?;
            let t: i64 = t.parse().map_err(|_| Error::Validation)?;
            let id = Uuid::parse_str(id).map_err(|_| Error::Validation)?;
            Ok(Some((t, id)))
        }
    }
}

fn time_cursor(item: &Value, time_key: &str, id_key: &str) -> Option<String> {
    let t = item[time_key]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())?;
    let id = item[id_key].as_str()?;
    Some(format!(
        "{}.{}",
        t.with_timezone(&Utc).timestamp_micros(),
        id
    ))
}

fn rfc3339(value: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|v| {
            DateTime::parse_from_rfc3339(v)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| Error::Validation)
        })
        .transpose()
}

/// `GET /internal/v1/communications/operator/profiles/{cfMeetId}`: one profile by its
/// current CF Meet ID (display name, activation state, operator hold, times).
pub async fn profile(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Path(cf_meet_id): Path<String>,
) -> Result<Json<Value>> {
    require(&op)?;
    let public_id = crate::crypto::normalize_public_id(&cf_meet_id);
    if !crate::crypto::valid_public_id(&public_id) {
        return Err(Error::NotFound);
    }
    let doc: Value = one(
        &s.store.db,
        "SELECT jsonb_build_object('cfMeetId',i.public_id,'displayName',p.display_name, \
           'activationState',p.activation_state,'createdAt',p.created_at,'updatedAt',p.updated_at, \
           'suspendedAt',p.suspended_at,'deactivatedAt',p.deactivated_at,'version',p.version, \
           'hold',CASE WHEN h.profile_id IS NULL THEN NULL ELSE jsonb_build_object('reason',h.reason,'active',h.active,'updatedAt',h.updated_at) END) doc \
         FROM communication_public_ids i JOIN communication_profiles p ON p.id=i.profile_id \
         LEFT JOIN communication_operator_holds h ON h.profile_id=p.id \
         WHERE i.public_id=$1 AND i.current",
        vec![public_id.into()],
    )
    .await?
    .ok_or(Error::NotFound)?;
    Ok(Json(doc))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportsQuery {
    /// OPEN (default), DISMISSED, ACTIONED or ALL.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
}

/// `GET /internal/v1/communications/operator/reports?status&limit&cursor`: profile reports,
/// oldest first: reason, both parties' current CF Meet IDs, the target's report count and
/// the review, if any. Reports carry no free text.
pub async fn reports(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Query(q): Query<ReportsQuery>,
) -> Result<Json<Value>> {
    require(&op)?;
    let status = q.status.as_deref().unwrap_or("OPEN");
    if !matches!(status, "OPEN" | "DISMISSED" | "ACTIONED" | "ALL") {
        return Err(Error::Validation);
    }
    let limit = limit(q.limit)?;
    let after = parse_time_cursor(q.cursor.as_deref())?;
    let (after_t, after_id) = match after {
        Some((t, id)) => (Some(t), Some(id)),
        None => (None, None),
    };
    let items: Vec<Value> = rows(
        &s.store.db,
        "SELECT jsonb_build_object('reportId',r.id,'reason',r.reason,'createdAt',r.created_at, \
           'reporterCfMeetId',(SELECT public_id FROM communication_public_ids WHERE profile_id=r.reporter_profile_id AND current), \
           'targetCfMeetId',(SELECT public_id FROM communication_public_ids WHERE profile_id=r.target_profile_id AND current), \
           'targetReportCount',(SELECT count(*) FROM communication_profile_reports x WHERE x.target_profile_id=r.target_profile_id), \
           'review',CASE WHEN v.report_id IS NULL THEN NULL ELSE jsonb_build_object('decision',v.decision,'reason',v.reason,'held',v.held,'reviewedAt',v.reviewed_at) END) doc \
         FROM communication_profile_reports r LEFT JOIN communication_profile_report_reviews v ON v.report_id=r.id \
         WHERE ($1='ALL' OR ($1='OPEN' AND v.report_id IS NULL) OR v.decision=$1) \
           AND ($2::bigint IS NULL OR (r.created_at, r.id) > (to_timestamp($2::bigint / 1000000.0), $3::uuid)) \
         ORDER BY r.created_at, r.id LIMIT $4",
        vec![status.into(), after_t.into(), after_id.into(), (limit + 1).into()],
    )
    .await?;
    page(items, limit, |i| time_cursor(i, "createdAt", "reportId"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HoldsQuery {
    /// Only active holds by default; `false` lists released ones.
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
}

/// `GET /internal/v1/communications/operator/holds?active&limit&cursor`: operator holds,
/// most recently changed first.
pub async fn holds(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Query(q): Query<HoldsQuery>,
) -> Result<Json<Value>> {
    require(&op)?;
    let limit = limit(q.limit)?;
    let (before_t, before_id) = match parse_time_cursor(q.cursor.as_deref())? {
        Some((t, id)) => (Some(t), Some(id)),
        None => (None, None),
    };
    let items: Vec<Value> = rows(
        &s.store.db,
        "SELECT jsonb_build_object('profileId',h.profile_id,'cfMeetId',(SELECT public_id FROM communication_public_ids WHERE profile_id=h.profile_id AND current), \
           'reason',h.reason,'active',h.active,'version',h.version,'updatedAt',h.updated_at) doc \
         FROM communication_operator_holds h \
         WHERE h.active=$1 \
           AND ($2::bigint IS NULL OR (h.updated_at, h.profile_id) < (to_timestamp($2::bigint / 1000000.0), $3::uuid)) \
         ORDER BY h.updated_at DESC, h.profile_id DESC LIMIT $4",
        vec![q.active.unwrap_or(true).into(), before_t.into(), before_id.into(), (limit + 1).into()],
    )
    .await?;
    let cursor = |i: &Value| time_cursor(i, "updatedAt", "profileId");
    let next = (items.len() as i64 > limit)
        .then(|| items.get(limit as usize - 1).and_then(cursor))
        .flatten();
    let out: Vec<Value> = items
        .into_iter()
        .take(limit as usize)
        .map(|mut i| {
            if let Some(m) = i.as_object_mut() {
                m.remove("profileId");
            }
            i
        })
        .collect();
    Ok(Json(json!({"items": out, "nextCursor": next})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    /// The sequence to continue below (newest first).
    #[serde(default)]
    cursor: Option<i64>,
}

/// `GET /internal/v1/communications/operator/audit?since&until&limit&cursor`: the Hub's
/// audit, newest first by sequence: operation, actor and resource ids, time. The free-form
/// details are not exposed.
pub async fn audit(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>> {
    require(&op)?;
    let limit = limit(q.limit)?;
    let since = rfc3339(q.since.as_deref())?;
    let until = rfc3339(q.until.as_deref())?;
    if let (Some(a), Some(b)) = (since, until) {
        if b <= a || b - a > Duration::days(31) {
            return Err(Error::Validation);
        }
    }
    if q.cursor.is_some_and(|c| c < 1) {
        return Err(Error::Validation);
    }
    let items: Vec<Value> = rows(
        &s.store.db,
        "SELECT jsonb_build_object('seq',seq,'operation',operation,'actorProfileId',actor_profile_id, \
           'resourceId',resource_id,'createdAt',created_at) doc \
         FROM communication_audit \
         WHERE ($1::bigint IS NULL OR seq < $1) AND ($2::timestamptz IS NULL OR created_at >= $2) \
           AND ($3::timestamptz IS NULL OR created_at < $3) \
         ORDER BY seq DESC LIMIT $4",
        vec![q.cursor.into(), since.into(), until.into(), (limit + 1).into()],
    )
    .await?;
    let next = (items.len() as i64 > limit)
        .then(|| {
            items
                .get(limit as usize - 1)
                .and_then(|i| i["seq"].as_i64())
        })
        .flatten();
    let out: Vec<Value> = items.into_iter().take(limit as usize).collect();
    Ok(Json(json!({"items": out, "nextCursor": next})))
}

/// `GET /internal/v1/communications/operator/metrics`: Hub counters (counts only).
pub async fn metrics(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
) -> Result<Json<Value>> {
    require(&op)?;
    let doc: Value = one(
        &s.store.db,
        "SELECT jsonb_build_object('asOf',now(),'profiles',(SELECT count(*) FROM communication_profiles), \
        'connections',(SELECT count(*) FROM communication_connections WHERE state='CONNECTED'), \
        'devicesActive',(SELECT count(*) FROM communication_devices WHERE state='ACTIVE'), \
        'outboxDead',(SELECT count(*) FROM communication_outbox WHERE state='DEAD'), \
        'operatorHolds',(SELECT count(*) FROM communication_operator_holds WHERE active), \
        'reportsOpen',(SELECT count(*) FROM communication_profile_reports r WHERE NOT EXISTS (SELECT 1 FROM communication_profile_report_reviews v WHERE v.report_id=r.id))) doc",
        vec![],
    )
    .await?
    .ok_or(Error::Unavailable)?;
    Ok(Json(doc))
}

fn page(
    items: Vec<Value>,
    limit: i64,
    cursor: impl Fn(&Value) -> Option<String>,
) -> Result<Json<Value>> {
    let next = (items.len() as i64 > limit)
        .then(|| items.get(limit as usize - 1).and_then(&cursor))
        .flatten();
    let out: Vec<Value> = items.into_iter().take(limit as usize).collect();
    Ok(Json(json!({"items": out, "nextCursor": next})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_cursors_round_trip_and_refuse_garbage() {
        let id = Uuid::new_v4();
        let item = json!({"createdAt": "2026-11-20T10:00:00.123456Z", "reportId": id.to_string()});
        let c = time_cursor(&item, "createdAt", "reportId").unwrap();
        let (t, back) = parse_time_cursor(Some(&c)).unwrap().unwrap();
        assert_eq!(back, id);
        assert_eq!(t % 1_000_000, 123_456);
        for bad in [
            "",
            "x",
            "1.notauuid",
            "abc.00000000-0000-0000-0000-000000000000",
        ] {
            assert!(parse_time_cursor(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn limits_are_bounded() {
        assert_eq!(limit(None).unwrap(), 50);
        assert!(limit(Some(0)).is_err() && limit(Some(101)).is_err());
    }
}

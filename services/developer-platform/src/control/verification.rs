//! ADR 0068, phase A3.1 — the verification queue HQ reviews, in HQ's shared case shape
//! (`cfmeet-admin-parallel-workplan.md` §3.6, design doc §17.6). Operator reads over the
//! existing data, served to the admin bridge and the operator console:
//!
//! * `PARTNER_GO_LIVE` — one production-access request (`cfmeet:PARTNER_GO_LIVE:<id>`);
//! * `APP_REVIEW` — one app (`cfmeet:APP_REVIEW:<app id>`); publishing is a step of the
//!   same case.
//!
//! States: PRODUCTION_REQUESTED / SUBMITTED → OPEN, UNDER_REVIEW / IN_REVIEW → CLAIMED,
//! APPROVED and LIVE → APPROVED, REJECTED, PUBLISHED, SUSPENDED, and CANCELLED (a
//! withdrawn request: not queue work, so never listed, but read and history keep it).
//! DRAFT apps are not cases. `version` is the row version, which every transition
//! increments. Evidence is hashes only: no partner text leaves through these reads.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{json, Value};
use uuid::Uuid;

use super::stmt;
use crate::http::error::{ApiError, ErrorCode};

pub const PARTNER_GO_LIVE: &str = "PARTNER_GO_LIVE";
pub const APP_REVIEW: &str = "APP_REVIEW";
/// Two people decide every verification case (HQ's R4 two-person rule).
pub const APPROVALS_REQUIRED: u32 = 2;
const MAX_LIMIT: u64 = 100;
const DEFAULT_LIMIT: u64 = 50;

/// `cfmeet:<KIND>:<uuid>` → (kind, id); the uuid in canonical lowercase only.
pub fn parse_case_id(case_id: &str) -> Option<(&'static str, Uuid)> {
    let (kind, raw) = case_id.strip_prefix("cfmeet:")?.split_once(':')?;
    let kind = match kind {
        PARTNER_GO_LIVE => PARTNER_GO_LIVE,
        APP_REVIEW => APP_REVIEW,
        _ => return None,
    };
    let id = Uuid::parse_str(raw).ok()?;
    (id.to_string() == raw).then_some((kind, id))
}

pub fn case_id(kind: &str, id: Uuid) -> String {
    format!("cfmeet:{kind}:{id}")
}

/// HQ's state for a source state, or `None` when the row is not a case (a DRAFT app).
pub fn hq_state(kind: &str, source: &str) -> Option<&'static str> {
    Some(match (kind, source) {
        (PARTNER_GO_LIVE, "PRODUCTION_REQUESTED") | (APP_REVIEW, "SUBMITTED") => "OPEN",
        (PARTNER_GO_LIVE, "UNDER_REVIEW") | (APP_REVIEW, "IN_REVIEW") => "CLAIMED",
        (PARTNER_GO_LIVE, "APPROVED" | "LIVE") | (APP_REVIEW, "APPROVED") => "APPROVED",
        (_, "REJECTED") => "REJECTED",
        (APP_REVIEW, "PUBLISHED") => "PUBLISHED",
        (_, "SUSPENDED") => "SUSPENDED",
        (PARTNER_GO_LIVE, "CANCELLED") => "CANCELLED",
        _ => return None,
    })
}

/// A reviewer reference as HQ shows it: `hq:<staff>` for bridge decisions, otherwise the
/// operator console's `operator:<name>`.
fn staff_ref(stored: Option<String>) -> Option<String> {
    let s = stored?;
    match s.strip_prefix("operator:") {
        Some(rest) if rest.starts_with("hq:") => Some(rest.to_string()),
        _ => Some(s),
    }
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// One row of either source, normalized.
const CASES_SQL: &str = "\
    SELECT 'PARTNER_GO_LIVE' AS kind, r.id AS id, r.tenant_id, r.application_id AS subject, r.state AS source_state, \
           r.version, r.reviewer_ref, r.created_at AS submitted_at, r.updated_at, \
           CASE WHEN r.use_case_summary IS NULL THEN NULL \
                ELSE encode(sha256(convert_to(r.use_case_summary, 'UTF8')), 'hex') END AS evidence_sha256, \
           NULL::int AS evidence_version \
      FROM devplat_production_access_requests r \
    UNION ALL \
    SELECT 'APP_REVIEW', a.app_id, a.tenant_id, a.app_id, a.status, a.version, a.reviewer_ref, \
           COALESCE((SELECT max(t.occurred_at) FROM devplat_app_transitions t \
                      WHERE t.app_id = a.app_id AND t.to_status = 'SUBMITTED'), a.created_at), \
           a.updated_at, \
           (SELECT encode(sha256(convert_to(v.manifest::text, 'UTF8')), 'hex') FROM devplat_app_versions v \
             WHERE v.app_id = a.app_id AND v.version = COALESCE(a.submitted_version, a.current_version)), \
           COALESCE(a.submitted_version, a.current_version) \
      FROM devplat_apps a WHERE a.status <> 'DRAFT'";

fn case_of(r: &sea_orm::QueryResult) -> Result<Option<Value>, ApiError> {
    let kind: String = r.try_get("", "kind")?;
    let kind: &'static str = if kind == PARTNER_GO_LIVE {
        PARTNER_GO_LIVE
    } else {
        APP_REVIEW
    };
    let id: Uuid = r.try_get("", "id")?;
    let source: String = r.try_get("", "source_state")?;
    let Some(state) = hq_state(kind, &source) else {
        return Ok(None);
    };
    let sha: Option<String> = r.try_get("", "evidence_sha256")?;
    let evidence_version: Option<i32> = r.try_get("", "evidence_version")?;
    let evidence: Vec<Value> = match (kind, sha) {
        (_, None) => vec![],
        (PARTNER_GO_LIVE, Some(h)) => {
            vec![json!({"kind": "USE_CASE_SUMMARY", "ref": format!("{id}:use-case"), "sha256": h})]
        }
        (_, Some(h)) => vec![json!({
            "kind": "APP_MANIFEST",
            "ref": format!("{id}:{}", evidence_version.unwrap_or(1)),
            "sha256": h,
        })],
    };
    let version: i64 = r.try_get("", "version")?;
    Ok(Some(json!({
        "case_id": case_id(kind, id),
        "product": "cfmeet",
        "kind": kind,
        "subject_ref": r.try_get::<Uuid>("", "subject")?.to_string(),
        "state": state,
        "version": version,
        "assignee_ref": staff_ref(r.try_get("", "reviewer_ref")?),
        "submitted_at": ts(r.try_get("", "submitted_at")?),
        "updated_at": ts(r.try_get("", "updated_at")?),
        "evidence": evidence,
        "approvals_required": APPROVALS_REQUIRED,
        "partner_tenant_id": r.try_get::<Uuid>("", "tenant_id")?.to_string(),
    })))
}

fn limit_of(limit: Option<u64>) -> Result<u64, ApiError> {
    let l = limit.unwrap_or(DEFAULT_LIMIT);
    if (1..=MAX_LIMIT).contains(&l) {
        Ok(l)
    } else {
        Err(ApiError(ErrorCode::ValidationError))
    }
}

/// `<updated_at micros>.<P|A>.<uuid>` (P: PARTNER_GO_LIVE, A: APP_REVIEW), newest first;
/// only characters every cursor pattern allows.
fn parse_cursor(c: &str) -> Option<(i64, String, Uuid)> {
    let mut parts = c.splitn(3, '.');
    let micros = parts.next()?.parse::<i64>().ok()?;
    let kind = match parts.next()? {
        "P" => PARTNER_GO_LIVE,
        "A" => APP_REVIEW,
        _ => return None,
    };
    let raw = parts.next()?;
    let id = Uuid::parse_str(raw).ok()?;
    (id.to_string() == raw).then(|| (micros, kind.to_string(), id))
}

fn cursor_of(micros: i64, kind: &str, id: Uuid) -> String {
    let k = if kind == PARTNER_GO_LIVE { "P" } else { "A" };
    format!("{micros}.{k}.{id}")
}

/// `GET /internal/v1/operator/verification/cases?limit=&cursor=`: the queue (every case but
/// CANCELLED), most recently changed first.
pub async fn list(
    db: &DatabaseConnection,
    limit: Option<u64>,
    cursor: Option<&str>,
) -> Result<Value, ApiError> {
    let limit = limit_of(limit)?;
    let after = match cursor {
        None => None,
        Some(c) => Some(parse_cursor(c).ok_or(ApiError(ErrorCode::ValidationError))?),
    };
    let (micros, kind, id) = match &after {
        Some((m, k, i)) => (Some(*m), Some(k.clone()), Some(*i)),
        None => (None, None, None),
    };
    let rows = db
        .query_all(stmt(
            &format!(
                "SELECT * FROM ({CASES_SQL}) c WHERE c.source_state <> 'CANCELLED' \
                 AND ($1::bigint IS NULL OR (floor(extract(epoch FROM c.updated_at) * 1000000)::bigint, c.kind, c.id) \
                      < ($1::bigint, $2::text, $3::uuid)) \
                 ORDER BY floor(extract(epoch FROM c.updated_at) * 1000000)::bigint DESC, c.kind DESC, c.id DESC \
                 LIMIT $4"
            ),
            vec![micros.into(), kind.into(), id.into(), ((limit + 1) as i64).into()],
        ))
        .await?;
    let mut items = Vec::new();
    let mut last: Option<(i64, String, Uuid)> = None;
    for r in rows.iter().take(limit as usize) {
        let updated: DateTime<Utc> = r.try_get("", "updated_at")?;
        let k: String = r.try_get("", "kind")?;
        let i: Uuid = r.try_get("", "id")?;
        last = Some((updated.timestamp_micros(), k, i));
        if let Some(c) = case_of(r)? {
            items.push(c);
        }
    }
    let next = (rows.len() as u64 > limit)
        .then(|| last.map(|(m, k, i)| cursor_of(m, &k, i)))
        .flatten();
    Ok(json!({"items": items, "next_cursor": next}))
}

/// `GET /internal/v1/operator/verification/cases/{case_id}`: one case (CANCELLED
/// included) with its decision details.
pub async fn read(db: &DatabaseConnection, case: &str) -> Result<Value, ApiError> {
    let (kind, id) = parse_case_id(case).ok_or(ApiError(ErrorCode::NotFound))?;
    let row = db
        .query_one(stmt(
            &format!("SELECT * FROM ({CASES_SQL}) c WHERE c.kind = $1 AND c.id = $2"),
            vec![kind.into(), id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let mut v = case_of(&row)?.ok_or(ApiError(ErrorCode::NotFound))?;
    v["details"] = details(db, kind, id).await?;
    Ok(v)
}

async fn details<C: ConnectionTrait>(db: &C, kind: &str, id: Uuid) -> Result<Value, ApiError> {
    if kind == PARTNER_GO_LIVE {
        let r = db
            .query_one(stmt(
                "SELECT requested_capabilities, approved_capabilities FROM devplat_production_access_requests WHERE id = $1",
                vec![id.into()],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::NotFound))?;
        let requested: Vec<String> = r.try_get("", "requested_capabilities")?;
        let approved: Vec<String> = r.try_get("", "approved_capabilities")?;
        Ok(json!({"requested_capabilities": requested, "approved_capabilities": approved}))
    } else {
        let r = db
            .query_one(stmt(
                "SELECT COALESCE(submitted_version, current_version) AS app_version, published_version \
                 FROM devplat_apps WHERE app_id = $1",
                vec![id.into()],
            ))
            .await?
            .ok_or(ApiError(ErrorCode::NotFound))?;
        let app_version: i32 = r.try_get("", "app_version")?;
        let published: Option<i32> = r.try_get("", "published_version")?;
        Ok(json!({"app_version": app_version, "published_version": published}))
    }
}

/// `GET /internal/v1/operator/verification/cases/{case_id}/history?limit=&cursor=`: the
/// case's transitions, oldest first. A partner's own actions show no account id.
pub async fn history(
    db: &DatabaseConnection,
    case: &str,
    limit: Option<u64>,
    cursor: Option<&str>,
) -> Result<Value, ApiError> {
    let (kind, id) = parse_case_id(case).ok_or(ApiError(ErrorCode::NotFound))?;
    let limit = limit_of(limit)?;
    let after: Option<i64> = match cursor {
        None => None,
        Some(c) => Some(
            c.parse()
                .map_err(|_| ApiError(ErrorCode::ValidationError))?,
        ),
    };
    let exists = db
        .query_one(stmt(
            &format!("SELECT 1 AS one FROM ({CASES_SQL}) c WHERE c.kind = $1 AND c.id = $2"),
            vec![kind.into(), id.into()],
        ))
        .await?;
    if exists.is_none() {
        return Err(ApiError(ErrorCode::NotFound));
    }
    let sql = if kind == PARTNER_GO_LIVE {
        "SELECT seq, from_state AS from_s, to_state AS to_s, actor_kind, actor_ref, reason_code, occurred_at \
         FROM devplat_production_access_transitions WHERE request_id = $1 AND ($2::bigint IS NULL OR seq > $2) \
         ORDER BY seq LIMIT $3"
    } else {
        "SELECT seq, from_status AS from_s, to_status AS to_s, actor_kind, actor_ref, reason_code, occurred_at \
         FROM devplat_app_transitions WHERE app_id = $1 AND ($2::bigint IS NULL OR seq > $2) \
         ORDER BY seq LIMIT $3"
    };
    let rows = db
        .query_all(stmt(
            sql,
            vec![id.into(), after.into(), ((limit + 1) as i64).into()],
        ))
        .await?;
    let mut items = Vec::new();
    let mut last = None;
    for r in rows.iter().take(limit as usize) {
        let seq: i64 = r.try_get("", "seq")?;
        last = Some(seq);
        let from: Option<String> = r.try_get("", "from_s")?;
        let to: String = r.try_get("", "to_s")?;
        let actor_kind: String = r.try_get("", "actor_kind")?;
        let actor_ref: String = r.try_get("", "actor_ref")?;
        items.push(json!({
            "sequence": seq,
            "from_state": from.as_deref().and_then(|f| hq_state(kind, f)),
            "to_state": hq_state(kind, &to),
            "actor_kind": match actor_kind.as_str() {
                "OPERATOR" => "STAFF",
                "USER" => "PARTNER",
                _ => "SYSTEM",
            },
            "actor_ref": (actor_kind == "OPERATOR").then(|| staff_ref(Some(actor_ref))).flatten(),
            "reason_code": r.try_get::<Option<String>>("", "reason_code")?,
            "occurred_at": ts(r.try_get("", "occurred_at")?),
        }));
    }
    let next = (rows.len() as u64 > limit)
        .then(|| last.map(|s| s.to_string()))
        .flatten();
    Ok(json!({"case_id": case_id(kind, id), "items": items, "next_cursor": next}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_ids_are_strict() {
        let id = Uuid::new_v4();
        assert_eq!(
            parse_case_id(&case_id(PARTNER_GO_LIVE, id)),
            Some((PARTNER_GO_LIVE, id))
        );
        assert_eq!(
            parse_case_id(&case_id(APP_REVIEW, id)),
            Some((APP_REVIEW, id))
        );
        for bad in [
            format!("cfmeet:APP_PUBLISH:{id}"),
            format!("karryam:APP_REVIEW:{id}"),
            format!("cfmeet:APP_REVIEW:{}", id.to_string().to_uppercase()),
            "cfmeet:APP_REVIEW:not-a-uuid".to_string(),
            format!("cfmeet:APP_REVIEW:{id}:x"),
        ] {
            assert_eq!(parse_case_id(&bad), None, "{bad}");
        }
    }

    #[test]
    fn cursors_round_trip_with_plain_characters() {
        let id = Uuid::new_v4();
        let c = cursor_of(1_795_000_000_123_456, APP_REVIEW, id);
        assert!(c
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'));
        assert_eq!(
            parse_cursor(&c),
            Some((1_795_000_000_123_456, APP_REVIEW.to_string(), id))
        );
        assert_eq!(parse_cursor("1.PARTNER_GO_LIVE.x"), None);
    }

    #[test]
    fn states_map_onto_hq_and_drafts_are_not_cases() {
        assert_eq!(hq_state(PARTNER_GO_LIVE, "LIVE"), Some("APPROVED"));
        assert_eq!(hq_state(PARTNER_GO_LIVE, "CANCELLED"), Some("CANCELLED"));
        assert_eq!(hq_state(APP_REVIEW, "IN_REVIEW"), Some("CLAIMED"));
        assert_eq!(hq_state(APP_REVIEW, "DRAFT"), None);
        assert_eq!(hq_state(APP_REVIEW, "PUBLISHED"), Some("PUBLISHED"));
        assert_eq!(
            staff_ref(Some("operator:hq:staff-1".into())).as_deref(),
            Some("hq:staff-1")
        );
        assert_eq!(
            staff_ref(Some("operator:ops.tester".into())).as_deref(),
            Some("operator:ops.tester")
        );
    }
}

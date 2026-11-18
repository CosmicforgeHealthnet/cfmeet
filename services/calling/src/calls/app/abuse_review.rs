//! Operator review of abuse reports over internal service authentication (C21).
//!
//! The operator console (`cfmeet.operator-console`) holds the receiver-granted
//! permission `calling-abuse-reports.review`; nothing else may call these routes.
//!
//! * `GET  /internal/v1/abuse-reports` — the review queue across scopes, oldest first
//!   (keyset on `created_at, id`), filtered by `status` (default `OPEN`) and optionally
//!   `environmentId` / `reportedPrincipalId`; `limit` 1–100 (default 50), opaque `cursor`.
//! * `POST /internal/v1/abuse-reports/{abuse_report_id}/resolve` — `{ "status":
//!   "REVIEWED" | "DISMISSED" | "OPEN" }`: the same audited transition as
//!   `platform_admin abuse-report set-status` (one transaction with its operator audit
//!   row; the operator is the calling service identity). Unknown report: 404.
//!
//! Responses carry the scope ids, bounded codes and opaque ids only, never free text.

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::{
    abuse_reports::{self, AbuseReportStatus},
    enterprise::stmt,
};
use crate::{
    enterprise::http::QueryParams, error::CallingError, internal_auth::middleware::InternalCaller,
    platform::operator::OperatorContext, state::AppState,
};
use cf_service_auth::InternalPermission;

const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 100;

/// A report as the operator console sees it (with its owning scope).
#[derive(Debug, Clone, Serialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct OperatorAbuseReportView {
    #[serde(rename = "abuseReportId")]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub call_id: Uuid,
    #[serde(rename = "principalId")]
    pub reporting_principal_id: Uuid,
    pub reported_principal_id: Option<Uuid>,
    pub category: String,
    pub comment_code: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const COLUMNS: &str = "id, tenant_id, application_id, environment_id, call_id, \
    reporting_principal_id, reported_principal_id, category, comment_code, status, \
    created_at, reviewed_at, version";

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/abuse-reports", get(list))
        .route("/abuse-reports/:abuse_report_id/resolve", post(resolve))
}

fn reviewer(caller: &InternalCaller) -> Result<(), CallingError> {
    caller.require_permission(InternalPermission::CallingAbuseReportsReview)
}

fn no_store(value: impl Serialize) -> Response {
    let mut r = Json(value).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn uuid_param(value: Option<&str>, name: &'static str) -> Result<Option<Uuid>, CallingError> {
    value
        .map(|v| Uuid::parse_str(v).map_err(|_| CallingError::Validation(name)))
        .transpose()
}

/// `<created_at micros>_<id>`: opaque to the console.
fn encode_cursor(at: DateTime<Utc>, id: Uuid) -> String {
    format!("{}_{}", at.timestamp_micros(), id.simple())
}

fn decode_cursor(cursor: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (micros, id) = cursor.split_once('_')?;
    Some((
        DateTime::from_timestamp_micros(micros.parse().ok()?)?,
        Uuid::parse_str(id).ok()?,
    ))
}

async fn list(
    State(state): State<AppState>,
    caller: InternalCaller,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    reviewer(&caller)?;
    let q = QueryParams::parse(
        query,
        &[
            "status",
            "environmentId",
            "reportedPrincipalId",
            "limit",
            "cursor",
        ],
    )?;
    let status = match q.get("status") {
        None => AbuseReportStatus::Open,
        Some(v) => AbuseReportStatus::parse(v).ok_or(CallingError::Validation(
            "status must be OPEN, REVIEWED or DISMISSED.",
        ))?,
    };
    let environment = uuid_param(q.get("environmentId"), "environmentId must be a UUID.")?;
    let reported = uuid_param(
        q.get("reportedPrincipalId"),
        "reportedPrincipalId must be a UUID.",
    )?;
    let limit = match q.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => v
            .parse::<u64>()
            .ok()
            .filter(|l| (1..=MAX_LIMIT).contains(l))
            .ok_or(CallingError::Validation(
                "limit must be an integer between 1 and 100.",
            ))?,
    };
    let after = q
        .get("cursor")
        .map(|c| decode_cursor(c).ok_or(CallingError::Validation("The cursor is invalid.")))
        .transpose()?;

    let mut sql = format!("SELECT {COLUMNS} FROM calling_abuse_reports WHERE status = $1");
    let mut values: Vec<sea_orm::Value> = vec![status.as_str().into()];
    if let Some(e) = environment {
        values.push(e.into());
        sql.push_str(&format!(" AND environment_id = ${}", values.len()));
    }
    if let Some(p) = reported {
        values.push(p.into());
        sql.push_str(&format!(" AND reported_principal_id = ${}", values.len()));
    }
    if let Some((at, id)) = after {
        values.push(at.into());
        values.push(id.into());
        sql.push_str(&format!(
            " AND (created_at, id) > (${}, ${})",
            values.len() - 1,
            values.len()
        ));
    }
    values.push(((limit + 1) as i64).into());
    sql.push_str(&format!(" ORDER BY created_at, id LIMIT ${}", values.len()));
    let mut rows = OperatorAbuseReportView::find_by_statement(stmt(&sql, values))
        .all(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "abuse report review list");
            CallingError::Internal
        })?;
    let next = if rows.len() as u64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|r| encode_cursor(r.created_at, r.id))
    } else {
        None
    };
    Ok(no_store(json!({ "data": rows, "nextCursor": next })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveBody {
    status: String,
}

async fn resolve(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(report_id): Path<String>,
    body: Result<Json<ResolveBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    reviewer(&caller)?;
    let report_id = Uuid::parse_str(&report_id).map_err(|_| CallingError::NotFound)?;
    let Json(body) = body.map_err(|_| CallingError::Validation("The request body is invalid."))?;
    let status = AbuseReportStatus::parse(&body.status).ok_or(CallingError::Validation(
        "status must be OPEN, REVIEWED or DISMISSED.",
    ))?;
    let operator = OperatorContext::new(caller.context().authenticated_service().as_str())?;
    abuse_reports::operator_set_status(&state.db, &operator, report_id, status, state.clock.now())
        .await?;
    let view = OperatorAbuseReportView::find_by_statement(stmt(
        &format!("SELECT {COLUMNS} FROM calling_abuse_reports WHERE id = $1"),
        vec![report_id.into()],
    ))
    .one(&state.db)
    .await
    .map_err(|_| CallingError::Internal)?
    .ok_or(CallingError::NotFound)?;
    tracing::info!(
        operator = %caller.context().authenticated_service(),
        abuse_report_id = %report_id,
        status = status.as_str(),
        "abuse report resolved"
    );
    Ok(no_store(view))
}

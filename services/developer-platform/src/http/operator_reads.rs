//! The admin bridge's operator reads of partner resources (ADR 0068, design §6.7 / §6.9):
//! `/internal/v1/operator/*`, permission `developer-platform.administer` AND a named
//! operator (the bridge's `hq:<staff>` attribution, or the operator console's operator).
//!
//! Read-only, keyset-paginated, newest first. They select identifiers, names, states,
//! counts and timestamps only: never an invitee email, a support email, a webhook secret
//! or URL path, a push credential, a project description or label, or a manifest's URLs.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, QueryResult, Statement, Value as DbValue};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
    operator::operator_of,
};
use crate::{
    control::pagination::{Page, PageQuery},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/operator/organizations/:organization_id/projects",
            get(projects),
        )
        .route(
            "/operator/organizations/:organization_id/environments",
            get(environments),
        )
        .route(
            "/operator/organizations/:organization_id/members",
            get(members),
        )
        .route(
            "/operator/organizations/:organization_id/invitations",
            get(invitations),
        )
        .route(
            "/operator/organizations/:organization_id/push-providers",
            get(push_providers),
        )
        .route(
            "/operator/organizations/:organization_id/webhooks",
            get(webhooks),
        )
        .route(
            "/operator/organizations/:organization_id/webhook-deliveries",
            get(webhook_deliveries),
        )
        .route("/operator/app-search", get(app_search))
        .route("/operator/apps/:app_id/versions", get(app_versions))
        .route(
            "/operator/apps/:app_id/installations",
            get(app_installations),
        )
        // ADR 0068 A3: the verification queue (HQ's case shape) and the bridge's settlement
        // read of an applied command.
        .route("/operator/verification/cases", get(verification_cases))
        .route(
            "/operator/verification/cases/:case_id",
            get(verification_case),
        )
        .route(
            "/operator/verification/cases/:case_id/history",
            get(verification_history),
        )
        .route(
            "/operator/operations/:idempotency_key",
            get(operation_record),
        )
        // ADR 0068 A4: a workspace's usage and the recorded platform incidents.
        .route(
            "/operator/organizations/:organization_id/usage",
            get(organization_usage),
        )
        .route("/operator/incidents", get(incidents))
        // ADR 0068 A4: the state of an owner-delivered secret (a reference, never the
        // secret, a link or the address).
        .route(
            "/operator/owner-deliveries/:delivery_ref",
            get(owner_delivery),
        )
        // ADR 0068 A7: the daily datasets the admin bridge pulls.
        .route("/operator/datasets/:dataset_id/days/:day", get(dataset_day))
}

/// `GET /internal/v1/operator/datasets/{datasetId}/days/{yyyy-mm-dd}` — the admin bridge
/// only (404 for every other caller and for an unknown dataset): one UTC day's counts.
async fn dataset_day(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((dataset, day)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    if caller.bridge_operator().is_none() {
        return Err(ApiError(ErrorCode::NotFound));
    }
    let date = crate::control::usage::parse_day(&day)?;
    // The activity history the purge keeps (the worker's own setting, read the same way).
    let retention =
        crate::retention::RetentionSettings::from_lookup(&|k: &str| std::env::var(k).ok())
            .unwrap_or_default();
    let activity_days = if retention.enabled {
        retention.days(crate::retention::Class::ActivityEvents)
    } else {
        None
    };
    let ctx = crate::control::datasets::DatasetContext {
        installs_known: s.build.install_events,
        activity_days,
        now: Utc::now(),
    };
    Ok(Json(
        crate::control::datasets::day(&s.db, &dataset, date, ctx).await?,
    ))
}

/// `GET /internal/v1/operator/owner-deliveries/{deliveryRef}` — the admin bridge only
/// (404 for every other caller and for an unknown or purged reference).
async fn owner_delivery(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(delivery_ref): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    if caller.bridge_operator().is_none() {
        return Err(ApiError(ErrorCode::NotFound));
    }
    crate::control::owner_delivery::view(&s.db, delivery_ref)
        .await?
        .map(Json)
        .ok_or(ApiError(ErrorCode::NotFound))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageParams {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// `GET …/organizations/{id}/usage?since&until` — the workspace's metered usage per
/// environment and usage type over `[since, until)` (UTC days, at most 31; default the
/// last 30 days), from the daily rollups: quantities and counts only. Offset-paginated
/// over a stable order (the aggregate is bounded by environments × usage types).
async fn organization_usage(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<UsageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let (since, until) = window(q.since.as_deref(), q.until.as_deref())?;
    let p = Page::from_query(&PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    })?;
    let offset = match (p.has_cursor(), p.cursor_seq()) {
        (false, _) => 0,
        (true, Some(n)) if n >= 0 => n,
        _ => return Err(ApiError(ErrorCode::ValidationError)),
    };
    existing(&s.db, id).await?;
    let until = until.unwrap_or_else(|| since.map_or_else(Utc::now, |s| s + Duration::days(30)));
    let since = since.unwrap_or(until - Duration::days(30));
    if until <= since || until - since > Duration::days(MAX_WINDOW_DAYS) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    // UTC days touched by the window: `[from, end)`; a window ending inside a day
    // includes that day.
    let from = since.date_naive();
    let end = if until.time() == chrono::NaiveTime::MIN {
        until.date_naive()
    } else {
        until.date_naive() + Duration::days(1)
    };
    let rows = s
        .db
        .query_all(stmt(
            "SELECT jsonb_build_object('projectId', application_id, 'environmentId', environment_id,                     'environmentKind', environment_kind, 'usageType', usage_type, 'unit', unit,                     'quantity', SUM(quantity)::bigint, 'factCount', SUM(fact_count)::bigint) AS v              FROM devplat_usage_rollups_daily              WHERE tenant_id = $1 AND usage_day >= $2 AND usage_day < $3              GROUP BY application_id, environment_id, environment_kind, usage_type, unit              ORDER BY environment_id, usage_type, unit OFFSET $4 LIMIT $5",
            vec![
                id.into(),
                from.into(),
                end.into(),
                offset.into(),
                (p.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    let mut items = Vec::new();
    for r in rows.iter().take(p.limit) {
        items.push(r.try_get::<Value>("", "v")?);
    }
    let next = (rows.len() > p.limit).then(|| Page::seq_cursor(offset + p.limit as i64));
    Ok(Json(json!({
        "items": items,
        "nextCursor": next,
        "from": from.to_string(),
        "to": (end - Duration::days(1)).to_string(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IncidentParams {
    #[serde(default)]
    organization_id: Option<Uuid>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// `GET /operator/incidents?organizationId&since&until` — the platform incidents recorded
/// against workspaces (every record, newest first; the support profile keeps only the
/// last): the incident reference and times, never ticket content or the operator.
async fn incidents(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Query(q): Query<IncidentParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let (since, until) = window(q.since.as_deref(), q.until.as_deref())?;
    let p = page(q.limit, q.cursor)?;
    if let Some(o) = q.organization_id {
        existing(&s.db, o).await?;
    }
    Ok(Json(
        keyset(
            &s.db,
            "SELECT e.occurred_at AS k_at, e.id AS k_id, jsonb_build_object(                  'incidentRecordId', e.id, 'organizationId', e.tenant_id,                  'incidentRef', e.summary->>'incidentRef', 'occurredAt', e.summary->>'occurredAt',                  'recordedAt', e.occurred_at) AS v              FROM devplat_activity_events e              WHERE ($1::uuid IS NULL OR e.tenant_id = $1)                AND e.action = 'support_profile.incident_recorded' AND e.outcome = 'SUCCEEDED'                AND ($2::timestamptz IS NULL OR (e.occurred_at, e.id) < ($2, $3::uuid))                AND ($5::timestamptz IS NULL OR e.occurred_at >= $5)                AND ($6::timestamptz IS NULL OR e.occurred_at < $6)              ORDER BY e.occurred_at DESC, e.id DESC LIMIT $4",
            vec![q.organization_id.into(), since.into(), until.into()],
            &p,
        )
        .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CasePage {
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn verification_cases(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Query(q): Query<CasePage>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    Ok(Json(
        crate::control::verification::list(&s.db, q.limit, q.cursor.as_deref()).await?,
    ))
}

async fn verification_case(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(case_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    Ok(Json(
        crate::control::verification::read(&s.db, &case_id).await?,
    ))
}

async fn verification_history(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(case_id): Path<String>,
    Query(q): Query<CasePage>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    Ok(Json(
        crate::control::verification::history(&s.db, &case_id, q.limit, q.cursor.as_deref())
            .await?,
    ))
}

async fn operation_record(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(key): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    // Only the bridge's own commands are recorded; the console has nothing to settle.
    if caller.bridge_operator().is_none() {
        return Err(ApiError(ErrorCode::NotFound));
    }
    Ok(Json(
        crate::control::bridge_command::operation_record(&s.db, key).await?,
    ))
}

fn stmt(sql: &str, values: Vec<DbValue>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

const MAX_WINDOW_DAYS: i64 = 31;

fn rfc3339(v: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(v)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| ApiError(ErrorCode::ValidationError))
}

/// An optional `[since, until)` window.
pub type Window = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Optional `[since, until)` window, at most 31 days when both are given.
pub fn window(since: Option<&str>, until: Option<&str>) -> Result<Window, ApiError> {
    let since = since.map(rfc3339).transpose()?;
    let until = until.map(rfc3339).transpose()?;
    if let (Some(s), Some(u)) = (since, until) {
        if u <= s || u - s > Duration::days(MAX_WINDOW_DAYS) {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    Ok((since, until))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageParams {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowParams {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchParams {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

pub(crate) fn page(limit: Option<usize>, cursor: Option<String>) -> Result<Page, ApiError> {
    let p = Page::from_query(&PageQuery { limit, cursor })?;
    if p.has_cursor() && p.cursor_key().is_none() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok(p)
}

/// The organization (tenant) must exist: 404 NOT_FOUND otherwise.
async fn existing<C: ConnectionTrait>(db: &C, tenant_id: Uuid) -> Result<(), ApiError> {
    db.query_one(stmt(
        "SELECT 1 AS x FROM devplat_organizations WHERE tenant_id = $1",
        vec![tenant_id.into()],
    ))
    .await?
    .map(|_| ())
    .ok_or(ApiError(ErrorCode::NotFound))
}

/// Runs a keyset page query whose rows carry `k_at`, `k_id` and `v` (a JSON object), the
/// cursor bound as `$2` / `$3` and the limit (+1) as `$4`.
pub(crate) async fn keyset<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    mut values: Vec<DbValue>,
    p: &Page,
) -> Result<Value, ApiError> {
    let (at, id) = p
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    values.insert(1, at.into());
    values.insert(2, id.into());
    values.insert(3, (p.limit as i64 + 1).into());
    let rows: Vec<QueryResult> = db.query_all(stmt(sql, values)).await?;
    let mut items = Vec::new();
    let mut last = None;
    for r in rows.iter().take(p.limit) {
        items.push(r.try_get::<Value>("", "v")?);
        last = Some((
            r.try_get::<DateTime<Utc>>("", "k_at")?,
            r.try_get::<Uuid>("", "k_id")?,
        ));
    }
    let next = (rows.len() > p.limit)
        .then(|| last.map(|(at, id)| Page::key_cursor(at, id)))
        .flatten();
    Ok(json!({ "items": items, "nextCursor": next }))
}

/// `GET …/organizations/{id}/projects` — the workspace's projects.
async fn projects(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing(&s.db, id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT p.created_at AS k_at, p.application_id AS k_id, jsonb_build_object( \
                 'projectId', p.application_id, 'displayName', p.display_name, 'status', p.status, \
                 'statusReason', p.status_reason, 'environments', \
                 (SELECT count(*) FROM devplat_environment_profiles e WHERE e.application_id = p.application_id), \
                 'createdAt', p.created_at, 'updatedAt', p.updated_at, 'archivedAt', p.archived_at, \
                 'version', p.version) AS v \
             FROM devplat_project_profiles p WHERE p.tenant_id = $1 \
               AND ($2::timestamptz IS NULL OR (p.created_at, p.application_id) < ($2, $3::uuid)) \
             ORDER BY p.created_at DESC, p.application_id DESC LIMIT $4",
            vec![id.into()],
            &p,
        )
        .await?,
    ))
}

/// `GET …/organizations/{id}/environments` — every environment of the workspace's projects.
async fn environments(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing(&s.db, id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT e.created_at AS k_at, e.environment_id AS k_id, jsonb_build_object( \
                 'environmentId', e.environment_id, 'projectId', e.application_id, 'kind', e.kind, \
                 'displayName', e.display_name, 'status', e.status, \
                 'productionAccessState', e.production_access_state, \
                 'createdAt', e.created_at, 'updatedAt', e.updated_at, 'version', e.version) AS v \
             FROM devplat_environment_profiles e WHERE e.tenant_id = $1 \
               AND ($2::timestamptz IS NULL OR (e.created_at, e.environment_id) < ($2, $3::uuid)) \
             ORDER BY e.created_at DESC, e.environment_id DESC LIMIT $4",
            vec![id.into()],
            &p,
        )
        .await?,
    ))
}

/// `GET …/organizations/{id}/members` — workspace memberships (opaque CF Meet user ids).
async fn members(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing(&s.db, id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT m.created_at AS k_at, m.id AS k_id, jsonb_build_object( \
                 'membershipId', m.id, 'userId', m.user_id, 'role', m.role, 'status', m.status, \
                 'joinedVia', m.joined_via, 'createdAt', m.created_at, 'updatedAt', m.updated_at, \
                 'removedAt', m.removed_at, 'version', m.version) AS v \
             FROM devplat_workspace_members m WHERE m.tenant_id = $1 \
               AND ($2::timestamptz IS NULL OR (m.created_at, m.id) < ($2, $3::uuid)) \
             ORDER BY m.created_at DESC, m.id DESC LIMIT $4",
            vec![id.into()],
            &p,
        )
        .await?,
    ))
}

/// `GET …/organizations/{id}/invitations` — invitations without the invitee's address.
async fn invitations(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing(&s.db, id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT i.created_at AS k_at, i.id AS k_id, jsonb_build_object( \
                 'invitationId', i.id, 'role', i.role, 'status', i.status, \
                 'createdAt', i.created_at, 'expiresAt', i.expires_at, 'decidedAt', i.decided_at) AS v \
             FROM devplat_invitations i WHERE i.tenant_id = $1 \
               AND ($2::timestamptz IS NULL OR (i.created_at, i.id) < ($2, $3::uuid)) \
             ORDER BY i.created_at DESC, i.id DESC LIMIT $4",
            vec![id.into()],
            &p,
        )
        .await?,
    ))
}

/// `GET …/organizations/{id}/push-providers` — push credential DETAILS (provider ids and
/// fingerprints; the sealed credential never leaves the vault).
async fn push_providers(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing(&s.db, id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT pp.created_at AS k_at, pp.id AS k_id, jsonb_build_object( \
                 'pushProviderId', pp.id, 'projectId', pp.application_id, \
                 'environmentId', pp.environment_id, 'environmentKind', pp.environment_kind, \
                 'provider', pp.provider, 'apnsKeyId', pp.apns_key_id, 'apnsTeamId', pp.apns_team_id, \
                 'apnsBundleId', pp.apns_bundle_id, 'apnsHost', pp.apns_host, \
                 'fcmProjectId', pp.fcm_project_id, 'fingerprint', pp.fingerprint, \
                 'verification', pp.verification, 'lastVerifiedAt', pp.last_verified_at, \
                 'lastErrorCode', pp.last_error_code, 'createdAt', pp.created_at, \
                 'updatedAt', pp.updated_at, 'state', pp.state, 'disabledAt', pp.disabled_at, \
                 'version', pp.version) AS v \
             FROM devplat_push_providers pp WHERE pp.tenant_id = $1 \
               AND ($2::timestamptz IS NULL OR (pp.created_at, pp.id) < ($2, $3::uuid)) \
             ORDER BY pp.created_at DESC, pp.id DESC LIMIT $4",
            vec![id.into()],
            &p,
        )
        .await?,
    ))
}

/// `GET …/organizations/{id}/webhooks` — subscriptions (endpoint HOST only; never the
/// signing secret, the URL path or query, or the description).
async fn webhooks(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing(&s.db, id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT w.created_at AS k_at, w.id AS k_id, jsonb_build_object( \
                 'subscriptionId', w.id, 'projectId', w.application_id, 'environmentId', w.environment_id, \
                 'environmentKind', w.environment_kind, \
                 'endpointHost', lower(substring(w.url from '^https?://([^/:?#]+)')), \
                 'eventTypes', to_jsonb(w.event_types), 'status', w.status, \
                 'consecutiveFailures', w.consecutive_failures, 'lastSuccessAt', w.last_success_at, \
                 'lastFailureAt', w.last_failure_at, 'disabledReason', w.disabled_reason, \
                 'origin', w.origin, 'createdAt', w.created_at, 'updatedAt', w.updated_at) AS v \
             FROM devplat_webhook_subscriptions w WHERE w.tenant_id = $1 AND w.deleted_at IS NULL \
               AND ($2::timestamptz IS NULL OR (w.created_at, w.id) < ($2, $3::uuid)) \
             ORDER BY w.created_at DESC, w.id DESC LIMIT $4",
            vec![id.into()],
            &p,
        )
        .await?,
    ))
}

/// `GET …/organizations/{id}/webhook-deliveries?since&until` — delivery outcomes (no
/// payload, no response body).
async fn webhook_deliveries(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<WindowParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let (since, until) = window(q.since.as_deref(), q.until.as_deref())?;
    let p = page(q.limit, q.cursor)?;
    existing(&s.db, id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT d.created_at AS k_at, d.id AS k_id, jsonb_build_object( \
                 'deliveryId', d.id, 'subscriptionId', d.subscription_id, 'eventId', d.event_id, \
                 'environmentId', d.environment_id, 'status', d.status, 'attemptCount', d.attempt_count, \
                 'lastHttpStatus', d.last_http_status, 'failureCategory', d.failure_category, \
                 'createdAt', d.created_at, 'completedAt', d.completed_at) AS v \
             FROM devplat_webhook_deliveries d WHERE d.tenant_id = $1 \
               AND ($2::timestamptz IS NULL OR (d.created_at, d.id) < ($2, $3::uuid)) \
               AND ($5::timestamptz IS NULL OR d.created_at >= $5) \
               AND ($6::timestamptz IS NULL OR d.created_at < $6) \
             ORDER BY d.created_at DESC, d.id DESC LIMIT $4",
            vec![id.into(), since.into(), until.into()],
            &p,
        )
        .await?,
    ))
}

fn like_literal(text: &str) -> String {
    let mut out = String::from("%");
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

const APP_SUMMARY: &str = "jsonb_build_object( \
    'appId', a.app_id, 'organizationId', a.tenant_id, 'name', v.manifest->>'name', \
    'distribution', v.manifest->>'distribution', 'status', a.status, \
    'currentVersion', a.current_version, 'publishedVersion', a.published_version, \
    'publishedAt', a.published_at, 'createdAt', a.created_at, 'updatedAt', a.updated_at)";

/// `GET /operator/app-search?q&limit&cursor` — apps by the current manifest's name
/// (case-insensitive substring, wildcards literal) or exact app id; newest first.
async fn app_search(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Query(q): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let text = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if text.is_some_and(|t| t.chars().count() > 128 || t.chars().any(char::is_control)) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let exact = text.and_then(|t| Uuid::parse_str(t).ok());
    let pattern = text.filter(|_| exact.is_none()).map(like_literal);
    let p = page(q.limit, q.cursor)?;
    Ok(Json(
        keyset(
            &s.db,
            &format!(
                "SELECT a.created_at AS k_at, a.app_id AS k_id, {APP_SUMMARY} AS v \
                 FROM devplat_apps a \
                 JOIN devplat_app_versions v ON v.app_id = a.app_id AND v.version = a.current_version \
                 WHERE ($1::uuid IS NULL OR a.app_id = $1) \
                   AND ($5::text IS NULL OR (v.manifest->>'name') ILIKE $5 ESCAPE '\\') \
                   AND ($2::timestamptz IS NULL OR (a.created_at, a.app_id) < ($2, $3::uuid)) \
                 ORDER BY a.created_at DESC, a.app_id DESC LIMIT $4"
            ),
            vec![exact.into(), pattern.into()],
            &p,
        )
        .await?,
    ))
}

async fn existing_app<C: ConnectionTrait>(db: &C, app_id: Uuid) -> Result<(), ApiError> {
    db.query_one(stmt(
        "SELECT 1 AS x FROM devplat_apps WHERE app_id = $1",
        vec![app_id.into()],
    ))
    .await?
    .map(|_| ())
    .ok_or(ApiError(ErrorCode::NotFound))
}

/// `GET /operator/apps/{appId}/versions` — manifest versions (name and distribution only).
async fn app_versions(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(app_id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing_app(&s.db, app_id).await?;
    // Versions are append-only and created in increasing order, so their creation time
    // orders them; the cursor carries the last version's creation time (the app id in the
    // key is constant here, `$3` is accepted and ignored).
    Ok(Json(
        keyset(
            &s.db,
            "SELECT v.created_at AS k_at, v.app_id AS k_id, jsonb_build_object( \
                 'version', v.version, 'name', v.manifest->>'name', \
                 'distribution', v.manifest->>'distribution', 'createdAt', v.created_at) AS v \
             FROM devplat_app_versions v WHERE v.app_id = $1 \
               AND ($2::timestamptz IS NULL OR v.created_at < $2) AND ($3::uuid IS NULL OR TRUE) \
             ORDER BY v.created_at DESC, v.version DESC LIMIT $4",
            vec![app_id.into()],
            &p,
        )
        .await?,
    ))
}

/// `GET /operator/apps/{appId}/installations` — installs (grant ids, targets, status).
async fn app_installations(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(app_id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let p = page(q.limit, q.cursor)?;
    existing_app(&s.db, app_id).await?;
    Ok(Json(
        keyset(
            &s.db,
            "SELECT i.installed_at AS k_at, i.grant_id AS k_id, jsonb_build_object( \
                 'grantId', i.grant_id, 'targetKind', i.target_kind, 'organizationId', i.organization_id, \
                 'principalId', i.principal_id, 'scopeCount', cardinality(i.scopes), 'status', i.status, \
                 'revokedReason', i.revoked_reason, 'installedAt', i.installed_at, \
                 'updatedAt', i.updated_at) AS v \
             FROM devplat_app_installations i WHERE i.app_id = $1 \
               AND ($2::timestamptz IS NULL OR (i.installed_at, i.grant_id) < ($2, $3::uuid)) \
             ORDER BY i.installed_at DESC, i.grant_id DESC LIMIT $4",
            vec![app_id.into()],
            &p,
        )
        .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_bounded() {
        assert!(window(Some("2026-11-01T00:00:00Z"), Some("2026-11-20T00:00:00Z")).is_ok());
        assert!(window(Some("2026-10-01T00:00:00Z"), Some("2026-11-20T00:00:00Z")).is_err());
        assert!(window(Some("2026-11-20T00:00:00Z"), Some("2026-11-01T00:00:00Z")).is_err());
        assert!(window(Some("yesterday"), None).is_err());
        assert_eq!(window(None, None).unwrap(), (None, None));
    }

    #[test]
    fn search_text_is_matched_literally() {
        assert_eq!(like_literal("50%_off"), "%50\\%\\_off%");
    }
}

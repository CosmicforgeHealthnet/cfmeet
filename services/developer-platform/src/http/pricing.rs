//! ADR 0068 A6 routes: pricing and plans (`/internal/v1/operator/pricing/**`) and the
//! finance reports (`/internal/v1/operator/finance/**`). Admin bridge ONLY: permission
//! `pricing.administer` (grantable to `cfmeet.admin-bridge` alone) AND its verified
//! `hq:<staff>` attribution; every command is two-person (R3 / R4). See
//! [`crate::pricing`].

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::{Duration, NaiveTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
    operator_reads::{keyset, page, window},
};
use crate::{
    control::{
        bridge_command::{BridgeCommand, Expect},
        pagination::{Page, PageQuery},
    },
    domain::{current_context, RequestContext},
    pricing::{self, VersionCommand},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operator/pricing/price-books", get(price_books))
        .route(
            "/operator/pricing/price-books/:price_book_id/versions",
            get(versions).post(draft),
        )
        .route(
            "/operator/pricing/price-books/:price_book_id/activations",
            get(activations),
        )
        .route(
            "/operator/pricing/price-books/:price_book_id/rollback",
            post(rollback),
        )
        .route("/operator/pricing/versions/:version_id", get(read_version))
        .route(
            "/operator/pricing/versions/:version_id/:command",
            post(version_command),
        )
        .route("/operator/pricing/freezes", get(freezes))
        .route(
            "/operator/pricing/markets/:market_code/:command",
            post(freeze),
        )
        .route("/operator/pricing/plans", get(plans))
        .route(
            "/operator/pricing/plan-assignments",
            get(assignments).post(assign),
        )
        .route(
            "/operator/pricing/overrides",
            get(overrides).post(change_override),
        )
        .route("/operator/pricing/simulations", post(simulate))
        .route("/operator/finance/usage", get(finance_usage))
        .route(
            "/operator/finance/estimated-charges",
            get(finance_estimated),
        )
        .route("/operator/finance/plan-spread", get(finance_spread))
}

/// `pricing.administer` and the admin bridge's attribution (anyone else: 403).
fn pricing_caller(caller: &InternalCaller) -> Result<RequestContext, ApiError> {
    caller.require_permission(InternalPermission::PricingAdminister)?;
    if caller.bridge_operator().is_none() {
        return Err(ApiError(ErrorCode::InternalPermissionDenied));
    }
    current_context().ok_or(ApiError(ErrorCode::InternalError))
}

/// A two-person pricing command at `expected` (`absent`: 0 allowed for a row never made).
fn command(
    caller: &InternalCaller,
    headers: &HeaderMap,
    expected: Option<i64>,
    absent: bool,
) -> Result<(RequestContext, BridgeCommand), ApiError> {
    let ctx = pricing_caller(caller)?;
    let expect = if absent {
        Expect::VersionOrAbsent
    } else {
        Expect::Version
    };
    Ok((
        ctx,
        BridgeCommand::required(caller, headers, expected, true, expect)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListParams {
    #[serde(default)]
    product_line: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn price_books(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    if q.state.is_some()
        || q.product_line.as_deref().is_some_and(|l| {
            ![pricing::rates::PARTNER_PLATFORM, pricing::rates::CONSUMER].contains(&l)
        })
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let p = page(q.limit, q.cursor)?;
    Ok(Json(
        keyset(&s.db, pricing::BOOKS_SQL, vec![q.product_line.into()], &p).await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageParams {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn versions(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(book): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    let p = page(q.limit, q.cursor)?;
    pricing::existing_book(&s.db, book).await?;
    Ok(Json(
        keyset(&s.db, &pricing::versions_sql(), vec![book.into()], &p).await?,
    ))
}

async fn activations(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(book): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    let p = page(q.limit, q.cursor)?;
    pricing::existing_book(&s.db, book).await?;
    Ok(Json(
        keyset(&s.db, pricing::ACTIVATIONS_SQL, vec![book.into()], &p).await?,
    ))
}

async fn read_version(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    Ok(Json(pricing::read_version(&s.db, id).await?))
}

async fn draft(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(book): Path<Uuid>,
    Json(body): Json<pricing::DraftBody>,
) -> Result<Json<Value>, ApiError> {
    let (ctx, cmd) = command(&caller, &headers, body.expected_version, false)?;
    Ok(Json(pricing::draft(&s.db, &ctx, book, body, &cmd).await?))
}

fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|_| ApiError(ErrorCode::ValidationError))
}

/// `POST …/versions/{versionId}/{edit|submit|approve|reject|schedule|activate}`.
async fn version_command(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (expected, c) = match name.as_str() {
        "edit" => {
            let b: pricing::EditBody = parse(body)?;
            (b.expected_version, VersionCommand::Edit(b.rates))
        }
        "submit" | "approve" | "activate" => {
            let b: pricing::DecisionBody = parse(body)?;
            if b.reason_code.is_some() {
                return Err(ApiError(ErrorCode::ValidationError));
            }
            let c = match name.as_str() {
                "submit" => VersionCommand::Submit,
                "approve" => VersionCommand::Approve,
                _ => VersionCommand::Activate,
            };
            (b.expected_version, c)
        }
        "reject" => {
            let b: pricing::DecisionBody = parse(body)?;
            let code = b.reason_code.unwrap_or_else(|| "NOT_APPROVED".into());
            if !crate::control::operator::valid_reason(&code) {
                return Err(ApiError(ErrorCode::ValidationError));
            }
            (b.expected_version, VersionCommand::Reject(code))
        }
        "schedule" => {
            let b: pricing::ScheduleBody = parse(body)?;
            (
                b.expected_version,
                VersionCommand::Schedule(b.effective_from),
            )
        }
        _ => return Err(ApiError(ErrorCode::NotFound)),
    };
    let (ctx, cmd) = command(&caller, &headers, expected, false)?;
    Ok(Json(
        pricing::version_command(&s.db, &ctx, id, c, &cmd).await?,
    ))
}

async fn rollback(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(book): Path<Uuid>,
    Json(body): Json<pricing::RollbackBody>,
) -> Result<Json<Value>, ApiError> {
    let (ctx, cmd) = command(&caller, &headers, body.expected_version, false)?;
    Ok(Json(
        pricing::rollback(&s.db, &ctx, book, body, &cmd).await?,
    ))
}

async fn freezes(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    if q.product_line.is_some()
        || q.state
            .as_deref()
            .is_some_and(|st| !["FROZEN", "UNFROZEN"].contains(&st))
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let p = page(q.limit, q.cursor)?;
    Ok(Json(
        keyset(&s.db, pricing::FREEZES_SQL, vec![q.state.into()], &p).await?,
    ))
}

/// `POST …/markets/{marketCode}/{freeze|unfreeze}`.
async fn freeze(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((market, name)): Path<(String, String)>,
    Json(body): Json<pricing::FreezeBody>,
) -> Result<Json<Value>, ApiError> {
    let on = match name.as_str() {
        "freeze" => true,
        "unfreeze" => false,
        _ => return Err(ApiError(ErrorCode::NotFound)),
    };
    let (ctx, cmd) = command(&caller, &headers, body.expected_version, true)?;
    Ok(Json(
        pricing::set_freeze(&s.db, &ctx, &market, on, body, &cmd).await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlansParams {
    #[serde(default)]
    price_book_id: Option<Uuid>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn plans(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PlansParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    // The plan list is bounded (lineages × at most 20 plans): one page.
    let p = Page::from_query(&PageQuery {
        limit: q.limit,
        cursor: None,
    })?;
    if q.cursor.is_some() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let _ = p;
    Ok(Json(pricing::plans(&s.db, q.price_book_id).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HolderParams {
    #[serde(default)]
    holder_id: Option<Uuid>,
    #[serde(default)]
    holder_kind: Option<String>,
    #[serde(default)]
    plan_code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn assignments(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<HolderParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    if q.state.is_some()
        || q.holder_kind
            .as_deref()
            .is_some_and(|k| !["USER", "ORGANIZATION"].contains(&k))
        || q.plan_code
            .as_deref()
            .is_some_and(|c| !pricing::rates::plan_code(c))
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let p = page(q.limit, q.cursor)?;
    Ok(Json(
        keyset(
            &s.db,
            pricing::ASSIGNMENTS_SQL,
            vec![q.holder_id.into(), q.holder_kind.into(), q.plan_code.into()],
            &p,
        )
        .await?,
    ))
}

async fn assign(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(body): Json<pricing::AssignBody>,
) -> Result<Json<Value>, ApiError> {
    let (ctx, cmd) = command(&caller, &headers, body.expected_version, true)?;
    Ok(Json(pricing::assign(&s.db, &ctx, body, &cmd).await?))
}

async fn overrides(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<HolderParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    if q.holder_kind.is_some()
        || q.plan_code.is_some()
        || q.state
            .as_deref()
            .is_some_and(|st| !["ACTIVE", "REVOKED"].contains(&st))
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let p = page(q.limit, q.cursor)?;
    Ok(Json(
        keyset(
            &s.db,
            pricing::OVERRIDES_SQL,
            vec![q.holder_id.into(), q.state.into()],
            &p,
        )
        .await?,
    ))
}

async fn change_override(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(body): Json<pricing::OverrideBody>,
) -> Result<Json<Value>, ApiError> {
    let (ctx, cmd) = command(&caller, &headers, body.expected_version, true)?;
    Ok(Json(
        pricing::change_override(&s.db, &ctx, body, &cmd).await?,
    ))
}

/// `pricing.charges.simulate`: a POST read (no Idempotency-Key, no side effect).
async fn simulate(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<pricing::SimulationBody>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    Ok(Json(pricing::simulate(&s.db, body).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReportParams {
    #[serde(default)]
    price_book_id: Option<Uuid>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// `[from, end)` UTC days of a window (default: the last 30 days; at most 31) and the
/// offset page.
fn report_window(
    q: &ReportParams,
) -> Result<(chrono::NaiveDate, chrono::NaiveDate, i64, usize), ApiError> {
    let (since, until) = window(q.since.as_deref(), q.until.as_deref())?;
    let until = until.unwrap_or_else(|| since.map_or_else(Utc::now, |s| s + Duration::days(30)));
    let since = since.unwrap_or(until - Duration::days(30));
    if until <= since || until - since > Duration::days(31) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let end = if until.time() == NaiveTime::MIN {
        until.date_naive()
    } else {
        until.date_naive() + Duration::days(1)
    };
    let p = Page::from_query(&PageQuery {
        limit: q.limit,
        cursor: q.cursor.clone(),
    })?;
    let offset = match (p.has_cursor(), p.cursor_seq()) {
        (false, _) => 0,
        (true, Some(n)) if n >= 0 => n,
        _ => return Err(ApiError(ErrorCode::ValidationError)),
    };
    Ok((since.date_naive(), end, offset, p.limit))
}

fn next(more: bool, offset: i64, limit: usize) -> Value {
    json!(more.then(|| Page::seq_cursor(offset + limit as i64)))
}

async fn finance_usage(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<ReportParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    if q.price_book_id.is_some() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let (from, end, offset, limit) = report_window(&q)?;
    let (items, more) = pricing::usage_report(&s.db, from, end, offset, limit).await?;
    Ok(Json(json!({
        "items": items, "nextCursor": next(more, offset, limit),
        "from": from.to_string(), "to": (end - Duration::days(1)).to_string(),
    })))
}

async fn finance_estimated(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<ReportParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    let book = q
        .price_book_id
        .ok_or(ApiError(ErrorCode::ValidationError))?;
    let (from, end, offset, limit) = report_window(&q)?;
    let (mut head, items, more) =
        pricing::estimated_charges(&s.db, book, from, end, offset, limit).await?;
    head["items"] = json!(items);
    head["nextCursor"] = next(more, offset, limit);
    head["from"] = json!(from.to_string());
    head["to"] = json!((end - Duration::days(1)).to_string());
    Ok(Json(head))
}

async fn finance_spread(
    State(s): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<ReportParams>,
) -> Result<Json<Value>, ApiError> {
    pricing_caller(&caller)?;
    if q.price_book_id.is_some() || q.limit.is_some() || q.cursor.is_some() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let (since, until) = window(q.since.as_deref(), q.until.as_deref())?;
    let at = until.unwrap_or_else(Utc::now);
    let items = pricing::plan_spread(&s.db, since, at).await?;
    Ok(Json(
        json!({ "items": items, "nextCursor": null, "asOf": at }),
    ))
}

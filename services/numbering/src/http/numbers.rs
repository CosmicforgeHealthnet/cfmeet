//! Inventory search and reservations (Workstream H), always in the caller's TRUSTED
//! scope from Platform Access:
//!
//! * `GET  /numbers/countries` (`numbers.search`) — the supported countries of the
//!   caller's inventory class;
//! * `GET  /numbers/countries/{countryCode}/number-types` (`numbers.search`) — number
//!   types with their capability metadata (generic catalog entries only);
//! * `GET  /numbers/search` (`numbers.search`) — provider-neutral inventory search:
//!   bounded pages, freshness, capability provenance, KYC indicators, per-environment
//!   quota (429 with `Retry-After`);
//! * `POST /numbers/reservations` (`numbers.manage`, `Idempotency-Key` required) — 202,
//!   PENDING until the provider hold is confirmed;
//! * `GET  /numbers/reservations` (`numbers.read`) — the caller's reservations, keyset
//!   pages, optional status filter;
//! * `POST /numbers/reservations/{reservationId}/cancel` (`numbers.manage`).
//!
//! Another scope's reservation is indistinguishable from a missing one; provider
//! identifiers never appear.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    catalog::{country_view, type_view, NUMBERS_SEARCH},
    customer::{reservation_view, NUMBERS_READ},
    error::{ApiError, ErrorCode},
    middleware::CustomerCaller,
};
use crate::{
    acquisition::{
        reservations::ReservationRequest,
        search::{SearchOutcome, SearchRequest, SearchResult},
    },
    domain::current_context,
    numbers::{Capability, NumberType, ReservationStatus},
    state::AppState,
};

pub const NUMBERS_MANAGE: &str = "numbers.manage";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/numbers/countries", get(countries))
        .route(
            "/numbers/countries/:country_code/number-types",
            get(number_types),
        )
        .route("/numbers/search", get(search))
        .route(
            "/numbers/reservations",
            get(list_reservations).post(create_reservation),
        )
        .route(
            "/numbers/reservations/:reservation_id/cancel",
            post(cancel_reservation),
        )
}

pub(crate) fn ctx() -> crate::domain::RequestContext {
    current_context().unwrap_or_else(crate::domain::RequestContext::generate)
}

/// The required `Idempotency-Key` (exactly one, well-formed).
pub(crate) fn required_idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let mut values = headers.get_all(super::kyc::IDEMPOTENCY_HEADER).iter();
    match (values.next(), values.next()) {
        (Some(v), None) => v
            .to_str()
            .ok()
            .filter(|k| crate::infra::idempotency::valid_key(k))
            .map(str::to_string)
            .ok_or(ApiError::Code(ErrorCode::ValidationError)),
        _ => Err(ApiError::Code(ErrorCode::ValidationError)),
    }
}

/// 202 for new work, 200 with `Idempotent-Replayed: true` for a replay.
pub(crate) fn accepted(replayed: bool, body: Value) -> Response {
    let status = if replayed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    let mut r = (status, Json(body)).into_response();
    if replayed {
        r.headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    r
}

fn parse_capabilities(raw: Option<&String>) -> Result<Vec<Capability>, ApiError> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(vec![]);
    };
    if raw.len() > 200 {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    raw.split(',')
        .map(|c| Capability::parse(c.trim()).ok_or(ApiError::Code(ErrorCode::ValidationError)))
        .collect()
}

const SEARCH_PARAMS: &[&str] = &[
    "countryCode",
    "region",
    "numberType",
    "capabilities",
    "prefix",
    "limit",
    "cursor",
];

async fn countries(
    State(s): State<AppState>,
    caller: CustomerCaller,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_SEARCH)?;
    let rows = s.acquisition().countries(&tc.owner_scope()).await?;
    Ok(Json(json!({
        "data": rows.iter().map(country_view).collect::<Vec<_>>(),
        "notice": crate::numbers::limitations::EXTERNAL_VERIFICATION_NOTICE,
    })))
}

async fn number_types(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(country): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_SEARCH)?;
    let rows = s
        .acquisition()
        .number_types(&tc.owner_scope(), &country)
        .await?;
    Ok(Json(json!({
        "countryCode": country,
        "data": rows.iter().map(type_view).collect::<Vec<_>>(),
        "notice": crate::numbers::limitations::EXTERNAL_VERIFICATION_NOTICE,
    })))
}

/// The customer view of a search result (contract `NumberSearchResult`).
pub fn search_result_view(r: &SearchResult) -> Value {
    let res = &r.resource;
    let supported: Vec<Value> = r
        .resolution
        .capabilities
        .iter()
        .filter(|c| c.supported)
        .map(|c| {
            json!({
                "capability": c.capability,
                "evidenceStatus": c.provenance.evidence_status,
                "synthetic": c.provenance.synthetic,
                "carrierConfirmed": c.provenance.carrier_confirmed,
            })
        })
        .collect();
    json!({
        "numberResourceId": res.id,
        "e164": res.e164,
        "countryCode": res.country_code,
        "region": res.region,
        "numberType": res.number_type,
        "inventoryClass": r.resolution.inventory_class,
        "capabilities": supported,
        "restrictionFlags": r.resolution.restriction_flags,
        "kyc": {
            "status": r.kyc.status,
            "ownershipClasses": r.kyc.ownership_classes,
            "numberFlagged": r.kyc.number_flagged,
        },
        "cost": {
            "monthlyMinor": res.monthly_cost_minor,
            "setupMinor": res.setup_cost_minor,
            "currency": res.currency,
            "evidence": res.cost_evidence,
        },
        "observation": {
            "observedAt": res.last_observed_at,
            "freshUntil": res.observation_fresh_until,
            "availability": r.availability,
            "guaranteed": false,
        },
    })
}

async fn search(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_SEARCH)?;
    if params.keys().any(|k| !SEARCH_PARAMS.contains(&k.as_str())) {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    let request = SearchRequest {
        country: params
            .get("countryCode")
            .cloned()
            .ok_or(ApiError::Code(ErrorCode::ValidationError))?,
        region: params.get("region").cloned(),
        number_type: params
            .get("numberType")
            .map(|t| NumberType::parse(t).ok_or(ApiError::Code(ErrorCode::ValidationError)))
            .transpose()?,
        capabilities: parse_capabilities(params.get("capabilities"))?,
        prefix: params.get("prefix").cloned(),
        limit: params
            .get("limit")
            .map(|l| {
                l.parse::<u64>()
                    .map_err(|_| ApiError::Code(ErrorCode::ValidationError))
            })
            .transpose()?,
        cursor: params.get("cursor").cloned(),
    };
    match s
        .acquisition()
        .search(&ctx(), &tc.owner_scope(), tc.live_telecom_allowed, &request)
        .await?
    {
        SearchOutcome::Throttled {
            retry_after_seconds,
        } => Err(ApiError::RateLimited {
            retry_after_seconds,
        }),
        SearchOutcome::Page(page) => Ok(Json(json!({
            "data": page.results.iter().map(search_result_view).collect::<Vec<_>>(),
            "nextCursor": page.next_cursor,
            "inventory": {
                "inventoryClass": page.inventory.inventory_class,
                "refresh": page.inventory.refresh,
                "refreshedAt": page.inventory.refreshed_at,
                "guaranteed": false,
            },
            "notice": crate::numbers::limitations::EXTERNAL_VERIFICATION_NOTICE,
        }))
        .into_response()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateReservationBody {
    pub number_resource_id: Uuid,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub hold_seconds: Option<i64>,
    #[serde(default)]
    pub eligibility_decision_id: Option<Uuid>,
}

async fn create_reservation(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateReservationBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let key = required_idempotency_key(&headers)?;
    let out = s
        .acquisition()
        .create_reservation(
            &ctx(),
            &tc.owner_scope(),
            ReservationRequest {
                number_resource_id: body.number_resource_id,
                capabilities: body.capabilities,
                hold_seconds: body.hold_seconds,
                eligibility_decision_id: body.eligibility_decision_id,
            },
            &key,
        )
        .await?;
    Ok(accepted(out.replayed, reservation_view(&out.reservation)))
}

fn encode_page_cursor(at: DateTime<Utc>, id: Uuid) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({ "t": at, "i": id }).to_string())
}

fn decode_page_cursor(cursor: &str) -> Result<(DateTime<Utc>, Uuid), ApiError> {
    let bad = || ApiError::Code(ErrorCode::ValidationError);
    if cursor.len() > 200 {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| bad())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    let at = v["t"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .ok_or_else(bad)?
        .with_timezone(&Utc);
    let id = v["i"]
        .as_str()
        .and_then(|i| Uuid::parse_str(i).ok())
        .ok_or_else(bad)?;
    Ok((at, id))
}

async fn list_reservations(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    if params
        .keys()
        .any(|k| !["status", "limit", "cursor"].contains(&k.as_str()))
    {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    let status = params
        .get("status")
        .map(|v| ReservationStatus::parse(v).ok_or(ApiError::Code(ErrorCode::ValidationError)))
        .transpose()?;
    let limit = match params.get("limit") {
        None => 50,
        Some(l) => l
            .parse::<u64>()
            .ok()
            .filter(|l| (1..=100).contains(l))
            .ok_or(ApiError::Code(ErrorCode::ValidationError))?,
    };
    let before = params
        .get("cursor")
        .map(|c| decode_page_cursor(c))
        .transpose()?;
    let rows = s
        .numbers
        .list_reservations(&tc.owner_scope(), status, before, limit + 1)
        .await?;
    let more = rows.len() as u64 > limit;
    let page: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = more.then(|| {
        let last = page.last().expect("a full page");
        encode_page_cursor(last.created_at, last.id)
    });
    Ok(Json(json!({
        "data": page.iter().map(reservation_view).collect::<Vec<_>>(),
        "nextCursor": next,
    })))
}

async fn cancel_reservation(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let row = s
        .acquisition()
        .cancel_reservation(&ctx(), &tc.owner_scope(), id)
        .await?;
    Ok(Json(reservation_view(&row)))
}

//! `GET /v1/usage` and `GET /v1/limits` (Part G §69 / §70). Both need `usage.read`; the
//! scope is the authenticated credential's application environment only.

use axum::{
    extract::{rejection::QueryRejection, Query, State},
    http::StatusCode,
    response::Response,
    routing::get,
    Router,
};
use chrono::{DateTime, Utc};

use super::{UsageFilter, RESOURCE_CLASSES};
use crate::{
    calls::{app::http::respond, command::CallScope},
    enterprise::{cursor, http::QueryParams},
    error::CallingError,
    platform::{context::PlatformAuthContext, scopes::names},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/usage", get(list_usage))
        .route("/v1/limits", get(get_limits))
}

const USAGE_TYPES: &[&str] = &[
    "CALL_CONNECTED_TIME",
    "PARTICIPANT_CONNECTED_TIME",
    "SESSION_CONNECTED_TIME",
    "LEG_CONNECTED_TIME",
    "VOICEMAIL_ARTIFACT",
    "RECORDING_ARTIFACT",
];

fn instant(q: &QueryParams, key: &str) -> Result<Option<DateTime<Utc>>, CallingError> {
    q.get(key)
        .map(|v| {
            DateTime::parse_from_rfc3339(v)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| CallingError::Validation("from and to must be RFC 3339 instants."))
        })
        .transpose()
}

fn filter(q: &QueryParams) -> Result<UsageFilter, CallingError> {
    let (from, to) = (instant(q, "from")?, instant(q, "to")?);
    if let (Some(f), Some(t)) = (from, to) {
        if f >= t {
            return Err(CallingError::Validation("from must be before to."));
        }
    }
    let by_type: Option<Vec<&'static str>> = q
        .get("usageType")
        .map(|v| {
            USAGE_TYPES
                .iter()
                .find(|t| **t == v)
                .map(|t| vec![*t])
                .ok_or(CallingError::Validation(
                    "usageType is not a known usage type.",
                ))
        })
        .transpose()?;
    let by_class: Option<&'static [&'static str]> = q
        .get("resourceClass")
        .map(|class| {
            RESOURCE_CLASSES
                .iter()
                .find(|(c, _)| *c == class)
                .map(|(_, types)| *types)
                .ok_or(CallingError::Validation(
                    "resourceClass is not a known resource class.",
                ))
        })
        .transpose()?;
    // Both filters: their intersection (possibly empty: nothing matches).
    let usage_types = match (by_type, by_class) {
        (None, None) => None,
        (Some(t), None) => Some(t),
        (None, Some(c)) => Some(c.to_vec()),
        (Some(t), Some(c)) => Some(t.into_iter().filter(|t| c.contains(t)).collect()),
    };
    Ok(UsageFilter {
        from,
        to,
        usage_types,
    })
}

async fn list_usage(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::USAGE_READ)?;
    let q = QueryParams::parse(
        query,
        &[
            "from",
            "to",
            "usageType",
            "resourceClass",
            "limit",
            "cursor",
        ],
    )?;
    let filter = filter(&q)?;
    let limit = match q.get("limit") {
        None => 50,
        Some(v) => match v.parse::<u64>() {
            Ok(l) if (1..=100).contains(&l) => l,
            _ => {
                return Err(CallingError::Validation(
                    "limit must be an integer between 1 and 100.",
                ))
            }
        },
    };
    let cursor = q
        .get("cursor")
        .map(|c| cursor::decode(c).map_err(|_| CallingError::Validation("cursor is invalid.")))
        .transpose()?;
    let list = state
        .usage
        .list(CallScope::of_platform(&ctx), &filter, limit, cursor)
        .await?;
    Ok(respond(StatusCode::OK, list, false))
}

async fn get_limits(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::USAGE_READ)?;
    QueryParams::parse(query, &[])?;
    let view = state.usage.limits(&ctx).await?;
    Ok(respond(StatusCode::OK, view, false))
}

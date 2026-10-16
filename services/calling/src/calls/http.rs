//! Public, read-only call API (contract `packages/contracts/calling/http/v1.openapi.json`):
//!
//! * `GET /v1/calls` — keyset-paginated list of the caller's Calls; with `principalId`,
//!   that principal's canonical history (Part F §62: participant-relative direction and
//!   outcome derived from Call evidence, `direction` / `outcome` / `missed` filters);
//!   `enterpriseDestinationType` + `enterpriseDestinationId` narrow either listing to the
//!   Calls routed through one extension, ring group, queue, IVR flow or mailbox;
//! * `GET /v1/calls/{call_id}` — one Call with participants, sessions and durations.
//!
//! Both require platform credential authentication (applied to every customer route by
//! `routes::app`) and the `calls.read` scope. Tenant / application / environment come only
//! from the authenticated [`PlatformAuthContext`]; a Call of another scope answers exactly
//! like a missing one (404). There are deliberately no call creation or control routes
//! yet, and no route lets a client set a state.

use axum::{
    extract::{rejection::QueryRejection, Path, Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::{
    app::history::{
        CallHistoryListView, HistoryDirection, HistoryFilter, ParticipantOutcome,
        HISTORY_DESTINATIONS,
    },
    command::CallScope,
    ids::CallId,
    read_model::{CallCursor, CallListView, CallView},
    service::ListCallsQuery,
    state::CallState,
};
use crate::{
    domain::PrincipalId,
    enterprise::destinations::{Destination, DestinationType},
    error::CallingError,
    platform::{context::PlatformAuthContext, scopes::names},
    state::AppState,
};

/// The public call routes (mounted behind platform credential authentication).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calls", get(list_calls))
        .route("/v1/calls/:call_id", get(get_call))
}

/// Call data is customer data: never cached by intermediaries or clients.
fn no_store<T: Serialize>(body: T) -> Response {
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn get_call(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_READ)?;
    // A malformed id cannot exist: same answer as an unknown or foreign Call.
    let call_id = Uuid::parse_str(&call_id)
        .map(CallId::from_uuid)
        .map_err(|_| CallingError::NotFound)?;
    let detail = state
        .calls
        .call_detail(CallScope::of_platform(&ctx), call_id)
        .await?;
    Ok(no_store(CallView::from(&detail)))
}

async fn list_calls(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_READ)?;
    let Query(pairs) =
        query.map_err(|_| CallingError::Validation("The query string is invalid."))?;
    let request = parse_list_query(&pairs, true)?;
    let scope = CallScope::of_platform(&ctx);
    if let Some(principal) = request.principal {
        let page = state
            .app
            .call_history(
                scope,
                principal,
                &request.query,
                request.filter,
                request.destination,
            )
            .await?;
        return Ok(no_store(CallHistoryListView::from(&page)));
    }
    if !request.filter.is_empty() {
        return Err(CallingError::Validation(
            "direction, outcome and missed require principalId.",
        ));
    }
    let page = match request.destination {
        Some(destination) => {
            state
                .app
                .calls_through(scope, &request.query, destination)
                .await?
        }
        None => state.calls.list_calls(scope, request.query).await?,
    };
    Ok(no_store(CallListView::from(&page)))
}

/// A parsed listing request: the call filters, and the history ones.
#[derive(Debug, Clone, Default)]
pub(crate) struct ListRequest {
    pub query: ListCallsQuery,
    /// Participant-scoped history of this principal.
    pub principal: Option<PrincipalId>,
    pub filter: HistoryFilter,
    pub destination: Option<Destination>,
}

const INVALID_LIMIT: &str = "limit must be an integer between 1 and 100.";

/// Parses the bounded list filters. Unknown or repeated parameters are refused, so a
/// misspelled filter never silently widens a listing. `principalId` is accepted only
/// where the caller may name a principal (`allow_principal`).
pub(crate) fn parse_list_query(
    pairs: &[(String, String)],
    allow_principal: bool,
) -> Result<ListRequest, CallingError> {
    let mut request = ListRequest {
        query: ListCallsQuery {
            limit: ListCallsQuery::DEFAULT_LIMIT,
            ..Default::default()
        },
        ..Default::default()
    };
    let query = &mut request.query;
    let mut destination_type = None;
    let mut destination_id = None;
    let mut seen: Vec<&str> = Vec::new();
    for (key, value) in pairs {
        if seen.contains(&key.as_str()) {
            return Err(CallingError::Validation("A query parameter is repeated."));
        }
        seen.push(key);
        let timestamp = |v: &str| {
            DateTime::parse_from_rfc3339(v)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| CallingError::Validation("Timestamps must be RFC 3339."))
        };
        match key.as_str() {
            "limit" => {
                query.limit = value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=ListCallsQuery::MAX_LIMIT).contains(n))
                    .ok_or(CallingError::Validation(INVALID_LIMIT))?;
            }
            "cursor" => {
                query.cursor = Some(
                    CallCursor::decode(value)
                        .ok_or(CallingError::Validation("The cursor is invalid."))?,
                );
            }
            "state" => {
                query.state = Some(
                    CallState::parse(value)
                        .ok_or(CallingError::Validation("The state filter is invalid."))?,
                );
            }
            "createdAfter" => query.created_after = Some(timestamp(value)?),
            "createdBefore" => query.created_before = Some(timestamp(value)?),
            "initiatorPrincipalId" => {
                query.initiator_principal_id =
                    Some(PrincipalId::from_uuid(Uuid::parse_str(value).map_err(
                        |_| CallingError::Validation("The initiator principal filter is invalid."),
                    )?));
            }
            "principalId" if allow_principal => {
                request.principal =
                    Some(PrincipalId::from_uuid(Uuid::parse_str(value).map_err(
                        |_| CallingError::Validation("The principal filter is invalid."),
                    )?));
            }
            "direction" => {
                request.filter.direction = Some(
                    HistoryDirection::parse(value)
                        .ok_or(CallingError::Validation("The direction filter is invalid."))?,
                );
            }
            "outcome" => {
                request.filter.outcome = Some(
                    ParticipantOutcome::parse(value)
                        .ok_or(CallingError::Validation("The outcome filter is invalid."))?,
                );
            }
            "missed" => {
                request.filter.missed = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(CallingError::Validation("missed must be true or false.")),
                });
            }
            "enterpriseDestinationType" => {
                destination_type = Some(
                    DestinationType::parse(value)
                        .filter(|t| HISTORY_DESTINATIONS.contains(t))
                        .ok_or(CallingError::Validation(
                            "The enterprise destination type filter is invalid.",
                        ))?,
                );
            }
            "enterpriseDestinationId" => {
                destination_id = Some(Uuid::parse_str(value).map_err(|_| {
                    CallingError::Validation("The enterprise destination filter is invalid.")
                })?);
            }
            _ => return Err(CallingError::Validation("Unknown query parameter.")),
        }
    }
    request.destination = match (destination_type, destination_id) {
        (Some(kind), Some(id)) => Some(Destination::new(kind, id)),
        (None, None) => None,
        _ => {
            return Err(CallingError::Validation(
                "enterpriseDestinationType and enterpriseDestinationId go together.",
            ))
        }
    };
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(pairs: &[(&str, &str)], allow_principal: bool) -> Result<ListRequest, CallingError> {
        parse_list_query(
            &pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>(),
            allow_principal,
        )
    }

    fn q(pairs: &[(&str, &str)]) -> Result<ListCallsQuery, CallingError> {
        r(pairs, true).map(|r| r.query)
    }

    #[test]
    fn list_filters_are_bounded_and_strict() {
        assert_eq!(q(&[]).unwrap().limit, ListCallsQuery::DEFAULT_LIMIT);
        assert_eq!(q(&[("limit", "100")]).unwrap().limit, 100);
        for bad in ["0", "101", "-1", "ten", "1e2"] {
            assert!(q(&[("limit", bad)]).is_err(), "{bad}");
        }
        assert!(q(&[("state", "CONNECTED")]).is_ok());
        assert!(q(&[("state", "SIP_200")]).is_err());
        assert!(q(&[("tenantId", "x")]).is_err());
        assert!(q(&[("limit", "5"), ("limit", "6")]).is_err());
        assert!(q(&[("cursor", "c1xyz")]).is_err());
        assert!(q(&[("createdAfter", "2031-01-01T00:00:00Z")]).is_ok());
        assert!(q(&[("createdAfter", "yesterday")]).is_err());
        assert!(q(&[("initiatorPrincipalId", "not-a-uuid")]).is_err());
    }

    #[test]
    fn history_filters_are_bounded_and_strict() {
        let p = uuid::Uuid::new_v4().to_string();
        let req = r(
            &[
                ("principalId", &p),
                ("missed", "true"),
                ("direction", "INBOUND"),
            ],
            true,
        )
        .unwrap();
        assert_eq!(req.principal.unwrap().as_uuid().to_string(), p);
        assert_eq!(req.filter.missed, Some(true));
        assert_eq!(req.filter.direction, Some(HistoryDirection::Inbound));
        // The endpoint's own history never names another principal.
        assert!(r(&[("principalId", &p)], false).is_err());
        for (k, bad) in [
            ("missed", "yes"),
            ("direction", "SIDEWAYS"),
            ("outcome", "PUSH_OPENED"),
            ("principalId", "nope"),
            ("enterpriseDestinationType", "PUBLIC_CONTACT"),
            ("enterpriseDestinationType", "PRINCIPAL"),
            ("enterpriseDestinationId", "x"),
        ] {
            assert!(r(&[(k, bad)], true).is_err(), "{k}={bad}");
        }
        // Type and id go together.
        assert!(r(&[("enterpriseDestinationType", "QUEUE")], true).is_err());
        assert!(r(&[("enterpriseDestinationId", &p)], true).is_err());
        let req = r(
            &[
                ("enterpriseDestinationType", "RING_GROUP"),
                ("enterpriseDestinationId", &p),
            ],
            true,
        )
        .unwrap();
        assert_eq!(req.destination.unwrap().kind, DestinationType::RingGroup);
    }
}

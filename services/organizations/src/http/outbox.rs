//! Organization event delivery: operator status and dead-letter replay
//! (`organization.administer`).
use axum::{body::Bytes, extract::State, http::StatusCode, response::Response};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{middleware::Caller, parse, respond};
use crate::{
    error::{ApiError, ApiResult},
    outbox,
    state::AppState,
    store,
};

/// `GET /internal/v1/admin/event-delivery` — per-consumer delivery counts, the oldest
/// undelivered event, the last delivery and the last failure class (no payloads).
pub async fn status(State(state): State<AppState>, caller: Caller) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let destinations = outbox::destination_status(&state.db).await?;
    Ok(respond(StatusCode::OK, json!({ "destinations": destinations })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplayBody {
    destination: Option<String>,
    #[serde(default)]
    event_ids: Vec<Uuid>,
}

/// `POST /internal/v1/admin/event-delivery/replay` — returns dead-lettered rows (all, one
/// consumer's, or listed events) to PENDING with a fresh attempt budget. Audited.
pub async fn replay(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let (req, _): (ReplayBody, Value) = parse(&body)?;
    if let Some(d) = &req.destination {
        if d == outbox::RECONCILIATION || !d.starts_with("cfmeet.") || d.len() > 64 {
            return Err(ApiError::validation("destination is not an event consumer"));
        }
    }
    if req.event_ids.len() > 500 {
        return Err(ApiError::validation("at most 500 event ids per replay"));
    }
    let ctx = caller.as_operator();
    let txn = sea_orm::TransactionTrait::begin(&state.db).await?;
    let replayed = outbox::replay_dead(&txn, req.destination.as_deref(), &req.event_ids).await?;
    let now = crate::db::now(&txn).await?;
    store::audit(
        &txn,
        &ctx,
        now,
        None,
        "event_delivery.replayed",
        "outbox",
        None,
        json!({ "destination": req.destination, "eventCount": req.event_ids.len(), "replayed": replayed }),
    )
    .await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, json!({ "replayed": replayed })))
}

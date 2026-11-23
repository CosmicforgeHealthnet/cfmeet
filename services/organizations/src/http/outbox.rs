//! Organization event delivery: operator status and dead-letter replay
//! (`organization.administer`).
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    admin_bridge::{self, command, BridgeCommand, Outcome, Risk, Subject},
    middleware::Caller,
    parse, respond,
};
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
    Ok(respond(
        StatusCode::OK,
        json!({ "destinations": destinations }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplayBody {
    destination: Option<String>,
    #[serde(default)]
    event_ids: Vec<Uuid>,
}

/// The admin bridge's answer for a replay (unversioned; `replayedCount` is `null` when a
/// repeated key answers without replaying again).
fn replay_outcome(req: &ReplayBody, replayed: Option<u64>) -> Outcome {
    Outcome::new(
        "REPLAYED",
        None,
        json!({
            "destination": req.destination,
            "requestedEventCount": req.event_ids.len(),
            "replayedCount": replayed,
        }),
    )
}

/// `POST /internal/v1/admin/event-delivery/replay` — returns dead-lettered rows (all, one
/// consumer's, or listed events) to PENDING with a fresh attempt budget. Audited.
///
/// The admin bridge (R2) also sends `Idempotency-Key` and gets the bridge answer; the
/// command is recorded with the replay (subject: event delivery, nil id).
pub async fn replay(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let bridge = BridgeCommand::of(&caller, &headers, Risk::R2)?;
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
    if let Some(b) = &bridge {
        if admin_bridge::replayed(
            &txn,
            b,
            command::EVENTS_REPLAY,
            Subject::EventDelivery,
            Some(Uuid::nil()),
        )
        .await?
        .is_some()
        {
            txn.commit().await?;
            return Ok(replay_outcome(&req, None).response());
        }
    }
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
    if let Some(b) = &bridge {
        let outcome = replay_outcome(&req, Some(replayed));
        admin_bridge::record(
            &txn,
            b,
            command::EVENTS_REPLAY,
            Subject::EventDelivery,
            Uuid::nil(),
            None,
            &outcome,
            now,
        )
        .await?;
        txn.commit().await?;
        return Ok(outcome.response());
    }
    txn.commit().await?;
    Ok(respond(StatusCode::OK, json!({ "replayed": replayed })))
}

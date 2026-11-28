//! Events routes (ADR 0071; nested under `/v1/messaging`; acting principal required).
//!
//! | Route | Scope |
//! |---|---|
//! | `GET /events`, `GET /events/calendar`, `GET /events/{eventId}`, `…/attendees`, `…/ics` | `messages.read` |
//! | `POST /events` (`Idempotency-Key`), `PATCH` / `DELETE /events/{eventId}`, `…/cancel`, `…/rsvp`, `…/invitations`, `…/reminders` | `messages.send` |
//! | Cover media `…/events/media/**` (images only) | `messages.read` / `messages.send` |

use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::ListQuery;
use crate::{
    conversations::http::MESSAGES_READ,
    http::{
        error::ApiError,
        middleware::{Acting, CustomerCaller},
    },
    membership::http::MESSAGES_SEND,
    messages::http::acting_with,
    social::{
        http::{
            media_cancel, media_complete, media_create, media_get, media_grants, media_part, Reply,
        },
        idempotency_key,
        media::{self, MediaKind, Purpose},
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/events", get(list).post(create))
        .route("/events/calendar", get(calendar))
        .route("/events/media", post(cover_create))
        .route(
            "/events/media/:media_id",
            get(cover_get).delete(cover_cancel),
        )
        .route("/events/media/:media_id/parts/:part", post(cover_part))
        .route("/events/media/:media_id/complete", post(cover_complete))
        .route(
            "/events/media/:media_id/download-grants",
            post(cover_grants),
        )
        .route(
            "/events/:event_id",
            get(get_event).patch(update).delete(delete_event),
        )
        .route("/events/:event_id/cancel", post(cancel))
        .route("/events/:event_id/rsvp", put(rsvp))
        .route("/events/:event_id/attendees", get(attendees))
        .route("/events/:event_id/invitations", post(invite))
        .route(
            "/events/:event_id/invitations/:principal_id",
            axum::routing::delete(uninvite),
        )
        .route("/events/:event_id/reminders", put(reminders))
        .route("/events/:event_id/ics", get(ics))
}

fn ok(v: Value) -> Reply {
    Ok(Json(v).into_response())
}

fn with_status(status: u16, v: Value) -> Reply {
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(v),
    )
        .into_response())
}

fn optional<T: Default + DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::validation(format!("invalid body: {e}")))
}

async fn reader(
    state: &AppState,
    caller: &CustomerCaller,
    headers: &HeaderMap,
) -> Result<Acting, ApiError> {
    acting_with(state, caller, headers, MESSAGES_READ).await
}

async fn writer(
    state: &AppState,
    caller: &CustomerCaller,
    headers: &HeaderMap,
) -> Result<Acting, ApiError> {
    acting_with(state, caller, headers, MESSAGES_SEND).await
}

const LIST_KEYS: &[&str] = &[
    "view",
    "from",
    "to",
    "tz",
    "conversationId",
    "q",
    "type",
    "limit",
    "cursor",
];

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    if let Some(k) = query.keys().find(|k| !LIST_KEYS.contains(&k.as_str())) {
        return Err(ApiError::validation(format!(
            "Unsupported query parameter {k}"
        )));
    }
    let q = ListQuery {
        view: query.get("view").cloned(),
        from: query.get("from").cloned(),
        to: query.get("to").cloned(),
        tz: query.get("tz").cloned(),
        conversation_id: query.get("conversationId").cloned(),
        q: query.get("q").cloned(),
        kind: query.get("type").cloned(),
        limit: query.get("limit").cloned(),
        cursor: query.get("cursor").cloned(),
    };
    ok(super::list(&state, caller.context(), &acting, q).await?)
}

async fn create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<super::CreateEvent>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let key = idempotency_key(&headers)?;
    let (s, v) = super::create(&state, caller.context(), &acting, body, key).await?;
    with_status(s, v)
}

async fn calendar(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(super::export_calendar(&state, caller.context(), &acting).await?)
}

async fn get_event(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(super::get(&state, caller.context(), &acting, id).await?)
}

async fn update(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Map<String, Value>>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(super::update(&state, caller.context(), &acting, id, body).await?)
}

async fn delete_event(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(super::delete(&state, caller.context(), &acting, id).await?)
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelBody {
    #[serde(default)]
    reason: Option<String>,
}

async fn cancel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let body: CancelBody = optional(&body)?;
    ok(super::cancel(&state, caller.context(), &acting, id, body.reason).await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RsvpBody {
    response: String,
}

async fn rsvp(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RsvpBody>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(super::respond(&state, caller.context(), &acting, id, &body.response).await?)
}

async fn attendees(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(super::attendees(&state, caller.context(), &acting, id).await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InviteBody {
    principal_ids: Vec<Uuid>,
}

async fn invite(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<InviteBody>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(super::invite(&state, caller.context(), &acting, id, body.principal_ids).await?)
}

async fn uninvite(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, who)): Path<(Uuid, Uuid)>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(super::uninvite(&state, caller.context(), &acting, id, who).await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemindersBody {
    minutes_before: Vec<i32>,
}

async fn reminders(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RemindersBody>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(super::set_reminders(&state, caller.context(), &acting, id, body.minutes_before).await?)
}

async fn ics(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(super::export_one(&state, caller.context(), &acting, id).await?)
}

// ------------------------------------------------------------------- cover media

async fn cover_create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<media::CreateMedia>,
) -> Reply {
    if body.kind != MediaKind::Image {
        return Err(ApiError::validation("An event cover is a photo"));
    }
    media_create(state, caller, headers, Purpose::Event, body).await
}

async fn cover_get(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_get(state, caller, headers, Purpose::Event, id).await
}

async fn cover_cancel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_cancel(state, caller, headers, Purpose::Event, id).await
}

async fn cover_part(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(path): Path<(Uuid, String)>,
    Json(body): Json<media::PartRequest>,
) -> Reply {
    media_part(state, caller, headers, Purpose::Event, path, body).await
}

async fn cover_complete(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_complete(state, caller, headers, Purpose::Event, id).await
}

async fn cover_grants(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_grants(state, caller, headers, Purpose::Event, id).await
}

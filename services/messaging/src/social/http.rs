//! Status and Channels routes (nested under `/v1/messaging`; acting principal required).
//!
//! | Route | Scope |
//! |---|---|
//! | `GET /status/feed`, `GET /status/mine`, `GET /status/{statusId}`, `GET /status/{statusId}/viewers`, `POST /status/rings` | `messages.read` |
//! | `POST /status` (`Idempotency-Key`), `PATCH /status/{statusId}` (allowSharing), `POST /status/delete`, `DELETE /status/{statusId}` | `messages.send` |
//! | `POST /status/{statusId}/view`, `PUT` / `DELETE /status/{statusId}/reaction`, `POST …/replies`, `POST …/forwards`, `POST …/reshares` | `messages.send` |
//! | `GET` / `PUT /status/audience`, `GET /status/eligible-contacts`, `GET` / `PUT /status/close-friends`, audience lists, mutes | `messages.read` / `messages.send` |
//! | Status media `…/status/media/**`, channel media `…/channels/media/**` | `messages.read` / `messages.send` |
//! | Channels `…/channels/**` | `messages.read` / `messages.send` |

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize};
use uuid::Uuid;

use super::{audience, channels, idempotency_key, media, media::Purpose, status};
use crate::{
    conversations::http::MESSAGES_READ,
    http::{
        error::ApiError,
        middleware::{Acting, CustomerCaller},
    },
    membership::http::MESSAGES_SEND,
    messages::http::acting_with,
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/status", post(create_status))
        .route("/status/feed", get(feed))
        .route("/status/mine", get(mine))
        .route("/status/delete", post(delete_many))
        .route("/status/rings", post(rings))
        .route("/status/audience", get(get_audience).put(put_audience))
        .route("/status/eligible-contacts", get(eligible))
        .route(
            "/status/close-friends",
            get(get_close_friends).put(put_close_friends),
        )
        .route("/status/audience-lists", get(lists).post(create_list))
        .route(
            "/status/audience-lists/:list_id",
            axum::routing::patch(update_list).delete(delete_list),
        )
        .route("/status/mutes", get(mutes))
        .route("/status/mutes/:principal_id", put(mute).delete(unmute))
        .route("/status/media", post(status_media_create))
        .route(
            "/status/media/:media_id",
            get(status_media_get).delete(status_media_cancel),
        )
        .route(
            "/status/media/:media_id/parts/:part",
            post(status_media_part),
        )
        .route(
            "/status/media/:media_id/complete",
            post(status_media_complete),
        )
        .route(
            "/status/media/:media_id/download-grants",
            post(status_media_grants),
        )
        .route(
            "/status/:status_id",
            get(get_status).patch(update_status).delete(delete_status),
        )
        .route("/status/:status_id/view", post(view_status))
        .route("/status/:status_id/reaction", put(react).delete(unreact))
        .route("/status/:status_id/viewers", get(viewers))
        .route("/status/:status_id/replies", post(reply))
        .route("/status/:status_id/forwards", post(forward))
        .route("/status/:status_id/reshares", post(reshare))
        .route("/channels", get(directory).post(create_channel))
        .route("/channels/following", get(following))
        .route("/channels/media", post(channel_media_create))
        .route(
            "/channels/media/:media_id",
            get(channel_media_get).delete(channel_media_cancel),
        )
        .route(
            "/channels/media/:media_id/parts/:part",
            post(channel_media_part),
        )
        .route(
            "/channels/media/:media_id/complete",
            post(channel_media_complete),
        )
        .route(
            "/channels/media/:media_id/download-grants",
            post(channel_media_grants),
        )
        .route(
            "/channels/:channel_id",
            get(get_channel)
                .patch(update_channel)
                .delete(delete_channel),
        )
        .route("/channels/:channel_id/follow", put(follow).delete(unfollow))
        .route("/channels/:channel_id/posts", get(posts).post(create_post))
        .route(
            "/channels/:channel_id/posts/:post_id",
            axum::routing::delete(delete_post),
        )
        .route(
            "/channels/:channel_id/posts/:post_id/reaction",
            put(react_post).delete(unreact_post),
        )
}

pub(crate) type Reply = Result<axum::response::Response, ApiError>;

fn ok(v: serde_json::Value) -> Reply {
    Ok(Json(v).into_response())
}

fn with_status(status: u16, v: serde_json::Value) -> Reply {
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(v),
    )
        .into_response())
}

/// An optional JSON body (empty: the default).
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

// ----------------------------------------------------------------------------- status

async fn create_status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<status::CreateStatus>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let key = idempotency_key(&headers)?;
    let (s, v) = status::create(&state, caller.context(), &acting, body, key).await?;
    with_status(s, v)
}

async fn feed(State(state): State<AppState>, caller: CustomerCaller, headers: HeaderMap) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(status::feed(&state, caller.context(), &acting).await?)
}

async fn mine(State(state): State<AppState>, caller: CustomerCaller, headers: HeaderMap) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(status::mine(&state, caller.context(), &acting).await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Ids {
    status_ids: Vec<Uuid>,
}

async fn delete_many(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<Ids>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let n = status::delete(&state, caller.context(), &acting, &body.status_ids).await?;
    ok(serde_json::json!({ "deleted": n }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RingsBody {
    principal_ids: Vec<Uuid>,
}

async fn rings(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<RingsBody>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(status::rings(&state, caller.context(), &acting, &body.principal_ids).await?)
}

async fn get_status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(status::get(&state, caller.context(), &acting, id).await?)
}

async fn update_status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<status::UpdateStatus>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(status::update(&state, caller.context(), &acting, id, body).await?)
}

async fn delete_status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    status::delete(&state, caller.context(), &acting, &[id]).await?;
    ok(serde_json::json!({ "statusId": id, "deleted": true }))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ViewBody {
    #[serde(default)]
    liked: Option<bool>,
}

async fn view_status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let body: ViewBody = optional(&body)?;
    ok(status::view(&state, caller.context(), &acting, id, body.liked).await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EmojiBody {
    emoji: String,
}

async fn react(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<EmojiBody>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(status::react(&state, caller.context(), &acting, id, Some(body.emoji)).await?)
}

async fn unreact(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(status::react(&state, caller.context(), &acting, id, None).await?)
}

async fn viewers(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(status::viewers(&state, caller.context(), &acting, id).await?)
}

async fn reply(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<status::ReplyRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    with_status(
        201,
        status::reply(&state, caller.context(), &acting, id, body).await?,
    )
}

async fn forward(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<status::ForwardRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(status::forward(&state, caller.context(), &acting, id, body).await?)
}

async fn reshare(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<status::ReshareRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let key = idempotency_key(&headers)?;
    let (s, v) = status::reshare(&state, caller.context(), &acting, id, body, key).await?;
    with_status(s, v)
}

// --------------------------------------------------------------------------- audience

async fn get_audience(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(audience::get_preference(&state, caller.context(), &acting).await?)
}

async fn put_audience(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<audience::PreferenceRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(audience::put_preference(&state, caller.context(), &acting, body).await?)
}

async fn eligible(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(audience::eligible(&state, caller.context(), &acting).await?)
}

async fn get_close_friends(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(audience::get_close_friends(&state, caller.context(), &acting).await?)
}

async fn put_close_friends(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<audience::CloseFriendsRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(audience::put_close_friends(&state, caller.context(), &acting, body).await?)
}

async fn lists(State(state): State<AppState>, caller: CustomerCaller, headers: HeaderMap) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(audience::lists(&state, caller.context(), &acting).await?)
}

async fn create_list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<audience::ListRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    with_status(
        201,
        audience::create_list(&state, caller.context(), &acting, body).await?,
    )
}

async fn update_list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<audience::ListRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(audience::update_list(&state, caller.context(), &acting, id, body).await?)
}

async fn delete_list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(audience::delete_list(&state, caller.context(), &acting, id).await?)
}

async fn mutes(State(state): State<AppState>, caller: CustomerCaller, headers: HeaderMap) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(audience::mutes(&state, caller.context(), &acting).await?)
}

async fn mute(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(other): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(audience::set_mute(&state, caller.context(), &acting, other, true).await?)
}

async fn unmute(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(other): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(audience::set_mute(&state, caller.context(), &acting, other, false).await?)
}

// ------------------------------------------------------------------------------ media

pub(crate) async fn media_create(
    state: AppState,
    caller: CustomerCaller,
    headers: HeaderMap,
    purpose: Purpose,
    body: media::CreateMedia,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    with_status(
        201,
        media::create(&state, caller.context(), &acting, purpose, body).await?,
    )
}

pub(crate) async fn media_get(
    state: AppState,
    caller: CustomerCaller,
    headers: HeaderMap,
    purpose: Purpose,
    id: Uuid,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(media::get(&state, caller.context(), &acting, purpose, id).await?)
}

pub(crate) async fn media_cancel(
    state: AppState,
    caller: CustomerCaller,
    headers: HeaderMap,
    purpose: Purpose,
    id: Uuid,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(media::cancel(&state, caller.context(), &acting, purpose, id).await?)
}

/// A part number from the path (a bad one answers `400 VALIDATION_ERROR`).
fn part_number(raw: &str) -> Result<i32, ApiError> {
    raw.parse::<i32>()
        .map_err(|_| ApiError::validation("The part number must be a whole number"))
}

pub(crate) async fn media_part(
    state: AppState,
    caller: CustomerCaller,
    headers: HeaderMap,
    purpose: Purpose,
    (id, part): (Uuid, String),
    body: media::PartRequest,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let part = part_number(&part)?;
    ok(media::part_upload(&state, caller.context(), &acting, purpose, id, part, body).await?)
}

pub(crate) async fn media_complete(
    state: AppState,
    caller: CustomerCaller,
    headers: HeaderMap,
    purpose: Purpose,
    id: Uuid,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(media::complete(&state, caller.context(), &acting, purpose, id).await?)
}

pub(crate) async fn media_grants(
    state: AppState,
    caller: CustomerCaller,
    headers: HeaderMap,
    purpose: Purpose,
    id: Uuid,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(media::download_grants(&state, caller.context(), &acting, purpose, id).await?)
}

async fn status_media_create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<media::CreateMedia>,
) -> Reply {
    media_create(state, caller, headers, Purpose::Status, body).await
}

async fn status_media_get(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_get(state, caller, headers, Purpose::Status, id).await
}

async fn status_media_cancel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_cancel(state, caller, headers, Purpose::Status, id).await
}

async fn status_media_part(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(path): Path<(Uuid, String)>,
    Json(body): Json<media::PartRequest>,
) -> Reply {
    media_part(state, caller, headers, Purpose::Status, path, body).await
}

async fn status_media_complete(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_complete(state, caller, headers, Purpose::Status, id).await
}

async fn status_media_grants(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_grants(state, caller, headers, Purpose::Status, id).await
}

async fn channel_media_create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<media::CreateMedia>,
) -> Reply {
    media_create(state, caller, headers, Purpose::Channel, body).await
}

async fn channel_media_get(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_get(state, caller, headers, Purpose::Channel, id).await
}

async fn channel_media_cancel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_cancel(state, caller, headers, Purpose::Channel, id).await
}

async fn channel_media_part(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(path): Path<(Uuid, String)>,
    Json(body): Json<media::PartRequest>,
) -> Reply {
    media_part(state, caller, headers, Purpose::Channel, path, body).await
}

async fn channel_media_complete(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_complete(state, caller, headers, Purpose::Channel, id).await
}

async fn channel_media_grants(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    media_grants(state, caller, headers, Purpose::Channel, id).await
}

// --------------------------------------------------------------------------- channels

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectoryQuery {
    #[serde(default)]
    q: Option<String>,
}

async fn directory(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<DirectoryQuery>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(channels::directory(&state, caller.context(), &acting, q.q.as_deref()).await?)
}

async fn create_channel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<channels::ChannelRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    with_status(
        201,
        channels::create(&state, caller.context(), &acting, body).await?,
    )
}

async fn following(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(channels::following(&state, caller.context(), &acting).await?)
}

async fn get_channel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(channels::get(&state, caller.context(), &acting, id).await?)
}

async fn update_channel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<channels::ChannelRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(channels::update(&state, caller.context(), &acting, id, body).await?)
}

async fn delete_channel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(channels::delete(&state, caller.context(), &acting, id).await?)
}

async fn follow(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(channels::set_following(&state, caller.context(), &acting, id, true).await?)
}

async fn unfollow(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(channels::set_following(&state, caller.context(), &acting, id, false).await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PostsQuery {
    #[serde(default)]
    before: Option<DateTime<Utc>>,
}

async fn posts(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PostsQuery>,
) -> Reply {
    let acting = reader(&state, &caller, &headers).await?;
    ok(channels::posts(&state, caller.context(), &acting, id, q.before).await?)
}

async fn create_post(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<channels::PostRequest>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    let key = idempotency_key(&headers)?;
    let (s, v) = channels::create_post(&state, caller.context(), &acting, id, body, key).await?;
    with_status(s, v)
}

async fn delete_post(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, post)): Path<(Uuid, Uuid)>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(channels::delete_post(&state, caller.context(), &acting, id, post).await?)
}

async fn react_post(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, post)): Path<(Uuid, Uuid)>,
    Json(body): Json<EmojiBody>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(channels::react(
        &state,
        caller.context(),
        &acting,
        id,
        post,
        Some(body.emoji),
    )
    .await?)
}

async fn unreact_post(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, post)): Path<(Uuid, Uuid)>,
) -> Reply {
    let acting = writer(&state, &caller, &headers).await?;
    ok(channels::react(&state, caller.context(), &acting, id, post, None).await?)
}

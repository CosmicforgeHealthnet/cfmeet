//! Updates (WhatsApp-style): status and channels. See [`crate::services::social`].

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::V2Json;
use crate::{
    error::ApiResult,
    services::{
        actor::ActorContext,
        social::{
            self, ChannelPostRequest, ChannelPostView, ChannelRequest, ChannelView,
            CreateMediaRequest, CreateStatusRequest, MediaView, StatusFeed, StatusPostView,
            StatusViewerView,
        },
    },
    state::AppState,
};

/// Start an upload
///
/// Declares a photo, video or voice note to upload for a status (`purpose` STATUS) or a
/// channel post (CHANNEL): kind, content type, size (photo and voice note at most 8 MiB,
/// video at most 64 MiB) and, for a video or voice note, its duration. The answer says how
/// many pieces to send (`chunkCount`, each `chunkBytes` except the last).
#[utoipa::path(
    post, path = "/api/v2/social/media", tag = "Updates (v2)", operation_id = "v2_social_create_media",
    request_body = CreateMediaRequest,
    responses(
        (status = 201, description = "Upload started", body = MediaView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_media(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(req): V2Json<CreateMediaRequest>,
) -> ApiResult<(StatusCode, Json<MediaView>)> {
    let user = actor.require_registered()?;
    Ok((
        StatusCode::CREATED,
        Json(social::create_media(&state.db, user.user_id, &req).await?),
    ))
}

/// Get an upload
///
/// The photo, video or voice note's kind, type, size, duration and pieces. Yours, a
/// channel's, or a live status of one of your contacts.
#[utoipa::path(
    get, path = "/api/v2/social/media/{mediaId}", tag = "Updates (v2)", operation_id = "v2_social_get_media",
    params(("mediaId" = Uuid, Path, description = "Media id")),
    responses(
        (status = 200, description = "Media", body = MediaView),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_media(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    Path(media_id): Path<Uuid>,
) -> ApiResult<Json<MediaView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::readable_media(&state, user, &headers, media_id).await?,
    ))
}

/// Upload a piece
///
/// Piece `seq` (from 0) of your unfinished upload, as raw bytes: exactly `chunkBytes`, the
/// last piece the rest. Sending a piece again replaces it.
#[utoipa::path(
    put, path = "/api/v2/social/media/{mediaId}/chunks/{seq}", tag = "Updates (v2)", operation_id = "v2_social_put_media_chunk",
    params(("mediaId" = Uuid, Path, description = "Media id"), ("seq" = i32, Path, description = "Piece number from 0")),
    request_body(content = Vec<u8>, description = "The piece's bytes", content_type = "application/octet-stream"),
    responses(
        (status = 204, description = "Stored"),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
        (status = 413, description = "REQUEST_TOO_LARGE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn put_media_chunk(
    State(state): State<AppState>,
    actor: ActorContext,
    Path((media_id, seq)): Path<(Uuid, i32)>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let user = actor.require_registered()?;
    social::put_chunk(&state.db, user.user_id, media_id, seq, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Read a piece
///
/// Piece `seq` of a photo, video or voice note you may see (see Get an upload), as raw
/// bytes. Join the pieces in order to get the file.
#[utoipa::path(
    get, path = "/api/v2/social/media/{mediaId}/chunks/{seq}", tag = "Updates (v2)", operation_id = "v2_social_get_media_chunk",
    params(("mediaId" = Uuid, Path, description = "Media id"), ("seq" = i32, Path, description = "Piece number from 0")),
    responses(
        (status = 200, description = "The piece", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_media_chunk(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    Path((media_id, seq)): Path<(Uuid, i32)>,
) -> ApiResult<Response> {
    let user = actor.require_registered()?;
    social::readable_media(&state, user, &headers, media_id).await?;
    let bytes = social::read_chunk(&state.db, media_id, seq).await?;
    let mut response = (StatusCode::OK, bytes).into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// Finish an upload
///
/// Checks that every piece arrived and that the file is the declared kind (recognised by
/// its content; a file that isn't is deleted and answers `400 VALIDATION_ERROR`).
#[utoipa::path(
    post, path = "/api/v2/social/media/{mediaId}/complete", tag = "Updates (v2)", operation_id = "v2_social_complete_media",
    params(("mediaId" = Uuid, Path, description = "Media id")),
    responses(
        (status = 200, description = "Upload complete", body = MediaView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn complete_media(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(media_id): Path<Uuid>,
) -> ApiResult<Json<MediaView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::complete_media(&state.db, user.user_id, media_id).await?,
    ))
}

/// Post a status
///
/// Text (with a background colour and font), or an uploaded photo, voice note (at most
/// 60 s) or video segment (`startMs`–`endMs`, at most 60 s: a longer video is posted as
/// several statuses of the same upload), with an optional caption. Your connected
/// contacts see it for 24 hours; then it is deleted with its views and media.
#[utoipa::path(
    post, path = "/api/v2/social/status", tag = "Updates (v2)", operation_id = "v2_social_create_status",
    request_body = CreateStatusRequest,
    responses(
        (status = 201, description = "Posted", body = StatusPostView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_status(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(req): V2Json<CreateStatusRequest>,
) -> ApiResult<(StatusCode, Json<StatusPostView>)> {
    let user = actor.require_registered()?;
    Ok((
        StatusCode::CREATED,
        Json(social::create_status(&state.db, user.user_id, &req).await?),
    ))
}

/// Status updates
///
/// Your own live statuses and those of your connected contacts (people with unseen
/// statuses first, then the most recent).
#[utoipa::path(
    get, path = "/api/v2/social/status/feed", tag = "Updates (v2)", operation_id = "v2_social_status_feed",
    responses(
        (status = 200, description = "Statuses", body = StatusFeed),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn status_feed(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
) -> ApiResult<Json<StatusFeed>> {
    let user = actor.require_registered()?;
    Ok(Json(social::feed(&state, user, &headers).await?))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewStatusRequest {
    /// Like (`true`) or unlike (`false`) it; omit to only mark it seen.
    pub liked: Option<bool>,
}

/// Mark a status seen
///
/// Records that you saw a contact's status (its owner sees you under "Viewed by"), and
/// optionally that you liked it.
#[utoipa::path(
    post, path = "/api/v2/social/status/{postId}/view", tag = "Updates (v2)", operation_id = "v2_social_view_status",
    params(("postId" = Uuid, Path, description = "Status id")),
    request_body = ViewStatusRequest,
    responses(
        (status = 204, description = "Recorded"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn view_status(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    Path(post_id): Path<Uuid>,
    V2Json(req): V2Json<ViewStatusRequest>,
) -> ApiResult<StatusCode> {
    let user = actor.require_registered()?;
    social::view_status(&state, user, &headers, post_id, req.liked).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Who saw my status
///
/// The people who saw one of your statuses, newest first, and whether they liked it.
#[utoipa::path(
    get, path = "/api/v2/social/status/{postId}/viewers", tag = "Updates (v2)", operation_id = "v2_social_status_viewers",
    params(("postId" = Uuid, Path, description = "Status id")),
    responses(
        (status = 200, description = "Viewers", body = Vec<StatusViewerView>),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn status_viewers(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(post_id): Path<Uuid>,
) -> ApiResult<Json<Vec<StatusViewerView>>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::viewers(&state.db, user.user_id, post_id).await?,
    ))
}

/// Delete my status
#[utoipa::path(
    delete, path = "/api/v2/social/status/{postId}", tag = "Updates (v2)", operation_id = "v2_social_delete_status",
    params(("postId" = Uuid, Path, description = "Status id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_status(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(post_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let user = actor.require_registered()?;
    social::delete_status(&state.db, user.user_id, post_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryQuery {
    /// Words in the channel's name or description.
    pub q: Option<String>,
}

/// Find channels
///
/// The channel directory, most followed first, optionally matching `q` (at most 50).
#[utoipa::path(
    get, path = "/api/v2/social/channels", tag = "Updates (v2)", operation_id = "v2_social_channel_directory",
    params(DirectoryQuery),
    responses(
        (status = 200, description = "Channels", body = Vec<ChannelView>),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn channel_directory(
    State(state): State<AppState>,
    actor: ActorContext,
    Query(q): Query<DirectoryQuery>,
) -> ApiResult<Json<Vec<ChannelView>>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::directory(&state.db, user.user_id, q.q.as_deref()).await?,
    ))
}

/// Create a channel
///
/// A channel you own (name 1–80 characters, description at most 500). You follow it; only
/// you post in it. At most 10 channels each.
#[utoipa::path(
    post, path = "/api/v2/social/channels", tag = "Updates (v2)", operation_id = "v2_social_create_channel",
    request_body = ChannelRequest,
    responses(
        (status = 201, description = "Created", body = ChannelView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_channel(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(req): V2Json<ChannelRequest>,
) -> ApiResult<(StatusCode, Json<ChannelView>)> {
    let user = actor.require_registered()?;
    Ok((
        StatusCode::CREATED,
        Json(social::create_channel(&state.db, user.user_id, &req).await?),
    ))
}

/// Channels I follow
///
/// Channels you follow or own, latest post first, with unread counts.
#[utoipa::path(
    get, path = "/api/v2/social/channels/following", tag = "Updates (v2)", operation_id = "v2_social_channels_following",
    responses(
        (status = 200, description = "Channels", body = Vec<ChannelView>),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn channels_following(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<Vec<ChannelView>>> {
    let user = actor.require_registered()?;
    Ok(Json(social::following(&state.db, user.user_id).await?))
}

/// Get a channel
#[utoipa::path(
    get, path = "/api/v2/social/channels/{channelId}", tag = "Updates (v2)", operation_id = "v2_social_get_channel",
    params(("channelId" = Uuid, Path, description = "Channel id")),
    responses(
        (status = 200, description = "Channel", body = ChannelView),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_channel(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(channel_id): Path<Uuid>,
) -> ApiResult<Json<ChannelView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::channel(&state.db, user.user_id, channel_id).await?,
    ))
}

/// Update my channel
///
/// Rename it or change its description (owner only).
#[utoipa::path(
    patch, path = "/api/v2/social/channels/{channelId}", tag = "Updates (v2)", operation_id = "v2_social_update_channel",
    params(("channelId" = Uuid, Path, description = "Channel id")),
    request_body = ChannelRequest,
    responses(
        (status = 200, description = "Channel", body = ChannelView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_channel(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(channel_id): Path<Uuid>,
    V2Json(req): V2Json<ChannelRequest>,
) -> ApiResult<Json<ChannelView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::update_channel(&state.db, user.user_id, channel_id, &req).await?,
    ))
}

/// Delete my channel
///
/// Deletes the channel with its posts (owner only).
#[utoipa::path(
    delete, path = "/api/v2/social/channels/{channelId}", tag = "Updates (v2)", operation_id = "v2_social_delete_channel",
    params(("channelId" = Uuid, Path, description = "Channel id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_channel(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(channel_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let user = actor.require_registered()?;
    social::delete_channel(&state.db, user.user_id, channel_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Follow a channel
#[utoipa::path(
    put, path = "/api/v2/social/channels/{channelId}/follow", tag = "Updates (v2)", operation_id = "v2_social_follow_channel",
    params(("channelId" = Uuid, Path, description = "Channel id")),
    responses(
        (status = 200, description = "Channel", body = ChannelView),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn follow_channel(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(channel_id): Path<Uuid>,
) -> ApiResult<Json<ChannelView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::set_following(&state.db, user.user_id, channel_id, true).await?,
    ))
}

/// Unfollow a channel
#[utoipa::path(
    delete, path = "/api/v2/social/channels/{channelId}/follow", tag = "Updates (v2)", operation_id = "v2_social_unfollow_channel",
    params(("channelId" = Uuid, Path, description = "Channel id")),
    responses(
        (status = 200, description = "Channel", body = ChannelView),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn unfollow_channel(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(channel_id): Path<Uuid>,
) -> ApiResult<Json<ChannelView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::set_following(&state.db, user.user_id, channel_id, false).await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct PostsQuery {
    /// Posts older than this time (the next page).
    pub before: Option<DateTime<Utc>>,
}

/// Channel posts
///
/// Newest first, 30 at a time (`before` for older). Reading the newest marks them read.
#[utoipa::path(
    get, path = "/api/v2/social/channels/{channelId}/posts", tag = "Updates (v2)", operation_id = "v2_social_channel_posts",
    params(("channelId" = Uuid, Path, description = "Channel id"), PostsQuery),
    responses(
        (status = 200, description = "Posts", body = Vec<ChannelPostView>),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn channel_posts(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(channel_id): Path<Uuid>,
    Query(q): Query<PostsQuery>,
) -> ApiResult<Json<Vec<ChannelPostView>>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::posts(&state.db, user.user_id, channel_id, q.before).await?,
    ))
}

/// Post in my channel
///
/// Text, or an uploaded CHANNEL photo, video or voice note with an optional caption
/// (owner only).
#[utoipa::path(
    post, path = "/api/v2/social/channels/{channelId}/posts", tag = "Updates (v2)", operation_id = "v2_social_create_channel_post",
    params(("channelId" = Uuid, Path, description = "Channel id")),
    request_body = ChannelPostRequest,
    responses(
        (status = 201, description = "Posted", body = ChannelPostView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_channel_post(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(channel_id): Path<Uuid>,
    V2Json(req): V2Json<ChannelPostRequest>,
) -> ApiResult<(StatusCode, Json<ChannelPostView>)> {
    let user = actor.require_registered()?;
    Ok((
        StatusCode::CREATED,
        Json(social::create_post(&state.db, user.user_id, channel_id, &req).await?),
    ))
}

/// Delete a post in my channel
#[utoipa::path(
    delete, path = "/api/v2/social/channels/{channelId}/posts/{postId}", tag = "Updates (v2)", operation_id = "v2_social_delete_channel_post",
    params(("channelId" = Uuid, Path, description = "Channel id"), ("postId" = Uuid, Path, description = "Post id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_channel_post(
    State(state): State<AppState>,
    actor: ActorContext,
    Path((channel_id, post_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let user = actor.require_registered()?;
    social::delete_post(&state.db, user.user_id, channel_id, post_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelReactionRequest {
    /// One emoji.
    #[schema(example = "👍")]
    pub emoji: String,
}

/// React to a channel post
///
/// Your one emoji on the post (replaces an earlier one).
#[utoipa::path(
    put, path = "/api/v2/social/channels/{channelId}/posts/{postId}/reaction", tag = "Updates (v2)", operation_id = "v2_social_react_channel_post",
    params(("channelId" = Uuid, Path, description = "Channel id"), ("postId" = Uuid, Path, description = "Post id")),
    request_body = ChannelReactionRequest,
    responses(
        (status = 200, description = "Post", body = ChannelPostView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn react_channel_post(
    State(state): State<AppState>,
    actor: ActorContext,
    Path((channel_id, post_id)): Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<ChannelReactionRequest>,
) -> ApiResult<Json<ChannelPostView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::react(
            &state.db,
            user.user_id,
            channel_id,
            post_id,
            Some(&req.emoji),
        )
        .await?,
    ))
}

/// Remove my reaction to a channel post
#[utoipa::path(
    delete, path = "/api/v2/social/channels/{channelId}/posts/{postId}/reaction", tag = "Updates (v2)", operation_id = "v2_social_unreact_channel_post",
    params(("channelId" = Uuid, Path, description = "Channel id"), ("postId" = Uuid, Path, description = "Post id")),
    responses(
        (status = 200, description = "Post", body = ChannelPostView),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn unreact_channel_post(
    State(state): State<AppState>,
    actor: ActorContext,
    Path((channel_id, post_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ChannelPostView>> {
    let user = actor.require_registered()?;
    Ok(Json(
        social::react(&state.db, user.user_id, channel_id, post_id, None).await?,
    ))
}

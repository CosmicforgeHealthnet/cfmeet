//! Attachment routes (nested under `/v1/conversations`, acting member).
//!
//! | Route | Scope |
//! |---|---|
//! | `POST /{id}/attachments` (upload intent) | `attachments.write` |
//! | `POST /{id}/attachments/{attachmentId}/finalize` | `attachments.write` (the uploader) |
//! | `GET /{id}/attachments/{attachmentId}` (metadata, lifecycle) | `attachments.read` |
//! | `POST /{id}/attachments/{attachmentId}/download-grants` | `attachments.read` |
//! | `GET /{id}/attachments/{attachmentId}/content` (proxied, re-authorized read) | `attachments.read` |
//! | `POST /{id}/attachments/{attachmentId}/thumbnail-grants` (server thumbnail of a MANAGED image) | `attachments.read` |

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

use super::IntentRequest;
use crate::{
    http::{error::ApiError, middleware::CustomerCaller},
    messages::http::acting_with,
    state::AppState,
};

pub const ATTACHMENTS_WRITE: &str = "attachments.write";
pub const ATTACHMENTS_READ: &str = "attachments.read";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/:conversation_id/attachments", post(intent))
        .route(
            "/:conversation_id/attachments/:attachment_id",
            get(metadata),
        )
        .route(
            "/:conversation_id/attachments/:attachment_id/finalize",
            post(finalize),
        )
        .route(
            "/:conversation_id/attachments/:attachment_id/download-grants",
            post(grant),
        )
        .route(
            "/:conversation_id/attachments/:attachment_id/content",
            get(content),
        )
        .route(
            "/:conversation_id/attachments/:attachment_id/thumbnail-grants",
            post(thumbnail_grant),
        )
}

async fn thumbnail_grant(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, attachment)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, ATTACHMENTS_READ).await?;
    Ok(Json(
        super::derivatives::thumbnail_grant(&state, caller.context(), &acting, id, attachment)
            .await?,
    ))
}

async fn intent(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<IntentRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, ATTACHMENTS_WRITE).await?;
    let (status, view) = super::create_intent(&state, caller.context(), &acting, id, body).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(view),
    ))
}

async fn finalize(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, attachment)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, ATTACHMENTS_WRITE).await?;
    Ok(Json(
        super::finalize(&state, caller.context(), &acting, id, attachment).await?,
    ))
}

async fn metadata(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, attachment)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, ATTACHMENTS_READ).await?;
    Ok(Json(
        super::get(&state, caller.context(), &acting, id, attachment).await?,
    ))
}

async fn grant(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, attachment)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, ATTACHMENTS_READ).await?;
    Ok(Json(
        super::grant(&state, caller.context(), &acting, id, attachment).await?,
    ))
}

async fn content(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, attachment)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let acting = acting_with(&state, &caller, &headers, ATTACHMENTS_READ).await?;
    let (row, bytes) = super::proxied(&state, caller.context(), &acting, id, attachment).await?;
    let mut r = bytes.into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&row.media_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = HeaderValue::from_str(&super::content_disposition(&row)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    Ok(r)
}

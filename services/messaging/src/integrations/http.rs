//! Internal integration routes (`/internal/v1`, service-authenticated, delegated scope).
//!
//! | Route | Permission |
//! |---|---|
//! | `POST /meeting-conversations` | `messaging-meeting.sync` |
//! | `PUT /meeting-conversations/{conversationId}/members/{principalId}` | `messaging-meeting.sync` |
//! | `POST /meeting-conversations/{conversationId}/lifecycle` | `messaging-meeting.sync` |
//! | `POST /meeting-conversations/{conversationId}/legacy-imports` | `messaging-meeting.sync` |
//! | `POST /meeting-conversations/{conversationId}/messages/{messageId}/moderation` | `messaging-meeting.sync` |

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{post, put},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use uuid::Uuid;

use super::meeting::{
    self, LegacyImport, LifecycleSync, MemberSync, MessageModeration, ResolveRequest,
};
use crate::{
    http::{
        error::ApiError,
        middleware::{delegated_context, verify_principal, InternalCaller},
    },
    identity::TrustedContext,
    state::AppState,
};

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/meeting-conversations", post(resolve))
        .route(
            "/meeting-conversations/:conversation_id/members/:principal_id",
            put(member),
        )
        .route(
            "/meeting-conversations/:conversation_id/lifecycle",
            post(lifecycle),
        )
        .route(
            "/meeting-conversations/:conversation_id/legacy-imports",
            post(legacy_import),
        )
        .route(
            "/meeting-conversations/:conversation_id/messages/:message_id/moderation",
            post(moderate),
        )
}

/// The meeting-sync caller's re-validated first-party scope.
async fn meeting_scope(
    state: &AppState,
    caller: &InternalCaller,
) -> Result<TrustedContext, ApiError> {
    caller.require_permission(InternalPermission::MessagingMeetingSync)?;
    let ctx = delegated_context(state, caller).await?;
    if !ctx.first_party {
        return Err(ApiError::code(
            crate::error::ErrorCode::DelegatedContextInvalid,
        ));
    }
    Ok(ctx)
}

async fn resolve(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<ResolveRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = meeting_scope(&state, &caller).await?;
    let owner = verify_principal(&state, &ctx, body.owner_principal_id).await?;
    let (status, view) = meeting::resolve(&state, &ctx, &owner, body).await?;
    state.wake_outbox();
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(view),
    ))
}

async fn member(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path((conversation_id, principal_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<MemberSync>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = meeting_scope(&state, &caller).await?;
    if body.state == "ACTIVE" {
        // Admission requires an ACTIVE messaging principal now; removals never do.
        verify_principal(&state, &ctx, principal_id).await?;
    }
    let view = meeting::sync_member(&state, &ctx, conversation_id, principal_id, body).await?;
    state.wake_outbox();
    Ok(Json(view))
}

async fn lifecycle(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(conversation_id): Path<Uuid>,
    Json(body): Json<LifecycleSync>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = meeting_scope(&state, &caller).await?;
    let view = meeting::sync_lifecycle(&state, &ctx, conversation_id, body).await?;
    state.wake_outbox();
    Ok(Json(view))
}

async fn legacy_import(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(conversation_id): Path<Uuid>,
    Json(body): Json<LegacyImport>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = meeting_scope(&state, &caller).await?;
    let view = meeting::import_legacy(&state, &ctx, conversation_id, body).await?;
    state.wake_outbox();
    Ok(Json(view))
}

async fn moderate(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path((conversation_id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<MessageModeration>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = meeting_scope(&state, &caller).await?;
    let view = meeting::moderate_message(&state, &ctx, conversation_id, message_id, body).await?;
    state.wake_outbox();
    Ok(Json(view))
}

//! The endpoint's own signaling operations (Part C §82, §83, §87), authenticated by a
//! short-lived signaling credential — never by a platform API credential. A signaling
//! credential reaches only its own endpoint's offers and sessions.
//!
//! * `GET  /v1/calling/invitations` — recovery: live and recently finished offers with
//!   the authoritative Call state and whether each is still actionable;
//! * `POST /v1/calling/invitations/{invitation_id}/accept|decline` — answer one's own offer
//!   (`expectedVersion` required; Idempotency-Key honoured);
//! * `POST /v1/calling/sessions/{session_id}/media-credentials` — a join credential for one's
//!   own authorized session.

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use super::SignalingIdentity;
use crate::{
    calls::{
        app::{
            http::{command_key, parse_body, respond, uuid_path, EmptyBody, VersionBody},
            media::MediaCredentialActor,
            InvitationActor, RecoveredInvitation,
        },
        ids::{CallInvitationId, CallSessionId},
        kinds::CommandOperation,
    },
    error::CallingError,
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calling/signaling", get(super::ws::signaling))
        .route("/v1/calling/invitations", get(invitations))
        .route(
            "/v1/calling/invitations/:invitation_id/accept",
            post(accept),
        )
        .route(
            "/v1/calling/invitations/:invitation_id/decline",
            post(decline),
        )
        .route(
            "/v1/calling/sessions/:session_id/media-credentials",
            post(media_credentials),
        )
        .layer(DefaultBodyLimit::max(4 * 1024))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InvitationList {
    endpoint_id: uuid::Uuid,
    data: Vec<RecoveredInvitation>,
}

async fn invitations(
    State(state): State<AppState>,
    identity: SignalingIdentity,
) -> Result<Response, CallingError> {
    let data = state.app.invitations_for_endpoint(&identity).await?;
    Ok(respond(
        StatusCode::OK,
        InvitationList {
            endpoint_id: identity.endpoint_id().as_uuid(),
            data,
        },
        false,
    ))
}

async fn answer(
    state: AppState,
    identity: SignalingIdentity,
    headers: HeaderMap,
    invitation_id: String,
    body: Result<Json<VersionBody>, JsonRejection>,
    operation: CommandOperation,
) -> Result<Response, CallingError> {
    let invitation_id = uuid_path(&invitation_id)?;
    let body = parse_body(body)?;
    let key = command_key(&headers, operation, Some(invitation_id), &body, false)?;
    let actor = InvitationActor::Endpoint(identity);
    let invitation_id = CallInvitationId::from_uuid(invitation_id);
    let outcome = if operation == CommandOperation::Accept {
        state
            .app
            .accept_invitation(
                actor,
                None,
                invitation_id,
                body.expected_version,
                key.as_ref(),
            )
            .await?
    } else {
        state
            .app
            .decline_invitation(
                actor,
                None,
                invitation_id,
                body.expected_version,
                key.as_ref(),
            )
            .await?
    };
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

async fn accept(
    State(state): State<AppState>,
    identity: SignalingIdentity,
    headers: HeaderMap,
    Path(invitation_id): Path<String>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    answer(
        state,
        identity,
        headers,
        invitation_id,
        body,
        CommandOperation::Accept,
    )
    .await
}

async fn decline(
    State(state): State<AppState>,
    identity: SignalingIdentity,
    headers: HeaderMap,
    Path(invitation_id): Path<String>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    answer(
        state,
        identity,
        headers,
        invitation_id,
        body,
        CommandOperation::Decline,
    )
    .await
}

async fn media_credentials(
    State(state): State<AppState>,
    identity: SignalingIdentity,
    Path(session_id): Path<String>,
    body: Result<Json<EmptyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    if let Err(rejection) = &body {
        if !matches!(rejection, JsonRejection::MissingJsonContentType(_)) {
            return Err(CallingError::Validation("The request body is invalid."));
        }
    }
    let session_id = CallSessionId::from_uuid(uuid_path(&session_id)?);
    let credential = state
        .app
        .issue_media_credential(MediaCredentialActor::Endpoint(identity), None, session_id)
        .await?;
    Ok(respond(StatusCode::OK, credential, false))
}

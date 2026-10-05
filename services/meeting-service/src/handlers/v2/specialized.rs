//! Part H transport: meeting-profile policy, webinar registration and backstage.
//! Handlers resolve identity server-side and delegate to the domain services.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

// ---------------------------- profile policy -------------------------------

/// Stored space override plus the effective (resolved, clamped) meeting profile.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/profile-policy", tag = "Profiles (v2)", operation_id = "v2_get_profile_policy",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses(
        (status = 200, description = "Profile policy", body = ProfilePolicyResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "MEETING_SPACE_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_profile_policy(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ProfilePolicyResponse>> {
    Ok(Json(state.meeting.profile_policies.get(&actor, id).await?))
}

/// Update the space's sparse profile override (cannot exceed org / security ceilings).
#[utoipa::path(
    patch, path = "/api/v2/meeting-spaces/{id}/profile-policy", tag = "Profiles (v2)", operation_id = "v2_update_profile_policy",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = ProfilePolicyUpdateRequest,
    responses(
        (status = 200, description = "Updated profile policy", body = ProfilePolicyResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / PROFILE_POLICY_DENIED", body = ErrorEnvelope),
        (status = 409, description = "MEETING_VERSION_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_profile_policy(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<ProfilePolicyUpdateRequest>,
) -> ApiResult<Json<ProfilePolicyResponse>> {
    Ok(Json(
        state
            .meeting
            .profile_policies
            .update(&actor, id, &req)
            .await?,
    ))
}

// ----------------------------- registration --------------------------------

/// Register for a webinar / town hall (public when the profile enables registration).
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/registrations", tag = "Registrations (v2)", operation_id = "v2_register",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = RegistrationRequest,
    responses(
        (status = 201, description = "Registered (admission token shown once)", body = RegistrationCreated),
        (status = 200, description = "Idempotent repeat", body = RegistrationCreated),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 404, description = "REGISTRATION_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "REGISTRATION_CLOSED / REGISTRATION_DUPLICATE", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
    ),
    security((), ("bearer_auth" = []))
)]
pub async fn register(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<RegistrationRequest>,
) -> ApiResult<(StatusCode, Json<RegistrationCreated>)> {
    let r = state
        .meeting
        .registrations
        .register(&actor, id, &req)
        .await?;
    let status = if r.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(r)))
}

/// Registrations of a meeting space (organizer view).
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/registrations", tag = "Registrations (v2)", operation_id = "v2_list_registrations",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses(
        (status = 200, description = "Registrations", body = RegistrationListResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_registrations(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<RegistrationListResponse>> {
    Ok(Json(RegistrationListResponse {
        registrations: state.meeting.registrations.list(&actor, id).await?,
    }))
}

/// One registration (organizer view).
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/registrations/{registrationId}", tag = "Registrations (v2)", operation_id = "v2_get_registration",
    params(("id" = Uuid, Path, description = "Meeting space id"), ("registrationId" = Uuid, Path, description = "Registration id")),
    responses(
        (status = 200, description = "Registration", body = RegistrationView),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "REGISTRATION_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_registration(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, rid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<RegistrationView>> {
    Ok(Json(
        state.meeting.registrations.get(&actor, id, rid).await?,
    ))
}

/// Cancel a registration (revokes its admission token).
#[utoipa::path(
    delete, path = "/api/v2/meeting-spaces/{id}/registrations/{registrationId}", tag = "Registrations (v2)", operation_id = "v2_cancel_registration",
    params(("id" = Uuid, Path, description = "Meeting space id"), ("registrationId" = Uuid, Path, description = "Registration id")),
    responses(
        (status = 200, description = "Cancelled", body = RegistrationView),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "REGISTRATION_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_registration(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, rid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<RegistrationView>> {
    Ok(Json(
        state.meeting.registrations.cancel(&actor, id, rid).await?,
    ))
}

// ------------------------------- backstage ---------------------------------

/// Move the caller into the backstage media room (presenters / hosts only).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/backstage", tag = "Backstage (v2)", operation_id = "v2_enter_backstage",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "In backstage; refresh the media credential", body = BackstageResponse),
        (status = 403, description = "BACKSTAGE_ACCESS_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_NOT_LIVE / FEATURE_DISABLED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn enter_backstage(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<BackstageResponse>> {
    Ok(Json(state.meeting.backstage.enter(&actor, id).await?))
}

/// Leave backstage (return to the main stage room).
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/backstage", tag = "Backstage (v2)", operation_id = "v2_leave_backstage",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Back on the main stage; refresh the media credential", body = BackstageResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn leave_backstage(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<BackstageResponse>> {
    Ok(Json(state.meeting.backstage.leave(&actor, id).await?))
}

//! Account sessions and the account security audit (Blueprint v1.0 Section 2: ACC-007,
//! ACC-008 groundwork, SEC-013).

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::V2Path;
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        account_security::{
            self as security, ActorKind, Outcome, RevokeReason, SecurityEvent, SecurityEventRecord,
            SessionRecord,
        },
        actor::ActorContext,
    },
    state::AppState,
};

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionListQuery {
    /// Also return revoked and expired sessions (most recent 100 in total).
    #[serde(default)]
    pub include_ended: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountSessionListResponse {
    pub sessions: Vec<SessionRecord>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountSessionRevokeResponse {
    /// Number of sessions this request ended (0 when the session had already ended).
    pub revoked_count: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityEventQuery {
    pub user_id: Option<Uuid>,
    /// Exact event type, for example LOGIN, LOGOUT_ALL or REFRESH_TOKEN_REUSE.
    pub event_type: Option<String>,
    /// `nextCursor` from the previous page.
    pub before: Option<i64>,
    /// 1-200, default 50.
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityEventPage {
    pub events: Vec<SecurityEventRecord>,
    pub next_cursor: Option<i64>,
}

/// The session id of the bearer token on this request, if it carries one.
fn current_session(state: &AppState, headers: &HeaderMap) -> Option<Uuid> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    state.jwt_service.verify_token(token).ok()?.session_id()
}

/// List my sessions
///
/// Lists the signed-in user's account sessions (one per sign-in), newest first, marking the
/// session this request was made with as `current`. Sessions last at most 30 days.
#[utoipa::path(
    get, path = "/api/v2/me/sessions", tag = "Account sessions (v2)", operation_id = "v2_list_my_sessions",
    params(SessionListQuery),
    responses(
        (status = 200, description = "Sessions", body = AccountSessionListResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_my_sessions(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    Query(query): Query<SessionListQuery>,
) -> ApiResult<Json<AccountSessionListResponse>> {
    let user = actor.require_registered()?;
    let sessions = security::list_sessions(
        &state.db,
        user.user_id,
        query.include_ended,
        current_session(&state, &headers),
    )
    .await?;
    Ok(Json(AccountSessionListResponse { sessions }))
}

/// Sign out one session
///
/// Ends one of the signed-in user's sessions (for example a lost device): its access and refresh
/// tokens stop working immediately on every CF Meet service that checks account sessions, and
/// everything that sign-in holds ends with it: its MANAGED-realtime session devices and the
/// Communications devices it linked (Messaging device, push registration, Calling endpoint).
/// Unknown or foreign session ids get the same 404.
#[utoipa::path(
    post, path = "/api/v2/me/sessions/{sessionId}/revoke", tag = "Account sessions (v2)", operation_id = "v2_revoke_my_session",
    params(("sessionId" = Uuid, Path, description = "Session id")),
    responses(
        (status = 200, description = "Revoked (or already ended)", body = AccountSessionRevokeResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "DEVICE_NOT_TRUSTED (ending a trusted device's sign-in from a sign-in that is not on a trusted device)", body = ErrorEnvelope),
        (status = 404, description = "SESSION_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_my_session(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(session_id): V2Path<Uuid>,
) -> ApiResult<Json<AccountSessionRevokeResponse>> {
    let user = actor.require_registered()?;
    if !security::owns_session(&state.db, user.user_id, session_id).await? {
        return Err(ApiError::domain(
            ErrorCode::SessionNotFound,
            "Session not found",
        ));
    }
    // Gap audit A5: a password-only (or undeclared) sign-in may end its own session and
    // other unclassified sign-ins, but never a sign-in of a TRUSTED device: that needs the
    // caller to be on a trusted device of the account too.
    if Some(session_id) != user.account_session_id
        && crate::services::account_devices::session_belongs_to_trusted_device(
            &state.db,
            user.user_id,
            session_id,
        )
        .await?
        && !crate::services::account_devices::session_on_trusted_device(
            &state.db,
            user.user_id,
            user.account_session_id,
        )
        .await?
    {
        return Err(ApiError::domain(
            ErrorCode::DeviceNotTrusted,
            "Ending a trusted device's sign-in needs a trusted device of this account",
        ));
    }
    let revoked = security::revoke_session(
        &state.db,
        user.user_id,
        session_id,
        RevokeReason::UserRevoked,
    )
    .await?;
    security::record_event(
        &state.db,
        SecurityEvent::new("SESSION_REVOKED", Outcome::Success)
            .by_user(user.user_id)
            .session(session_id),
    )
    .await?;
    // Blueprint Sections 13-14: the session's MANAGED-realtime session devices and the
    // Hub devices it linked end with it (ADR 0057 amendment A1).
    crate::services::communications::session_ended(&state, user.user_id, session_id).await?;
    Ok(Json(AccountSessionRevokeResponse {
        revoked_count: u64::from(revoked),
    }))
}

/// Sign out everywhere
///
/// Ends every session of the signed-in user, including the current one, and every token issued
/// before sessions existed. Connected CF Meet services are told through the account's durable
/// revocation events. Sign in again afterwards.
#[utoipa::path(
    post, path = "/api/v2/me/sessions/revoke-all", tag = "Account sessions (v2)", operation_id = "v2_revoke_all_my_sessions",
    responses(
        (status = 200, description = "Every session ended", body = AccountSessionRevokeResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "DEVICE_NOT_TRUSTED (the account has a trusted device and this sign-in is not on one)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_all_my_sessions(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<AccountSessionRevokeResponse>> {
    let user = actor.require_registered()?;
    let user_id = user.user_id;
    // Gap audit A5: signing out everywhere ends every trusted device's sign-in; once the
    // account has a trusted device, only a trusted device may do it.
    if crate::services::account_devices::has_trusted_device(&state.db, user_id).await?
        && !crate::services::account_devices::session_on_trusted_device(
            &state.db,
            user_id,
            user.account_session_id,
        )
        .await?
    {
        return Err(ApiError::domain(
            ErrorCode::DeviceNotTrusted,
            "Signing out everywhere needs a trusted device of this account once it has one",
        ));
    }
    let revoked = security::revoke_all(&state.db, user_id, RevokeReason::LogoutAll).await?;
    security::record_event(
        &state.db,
        SecurityEvent::new("LOGOUT_ALL", Outcome::Success)
            .by_user(user_id)
            .detail(serde_json::json!({ "sessionsEnded": revoked })),
    )
    .await?;
    // Existing account-level fan-out: Communications epoch + Hub event, Messaging sessions.
    crate::services::communications::record_logout(&state, user_id).await?;
    let _ = crate::services::communications::deliver_due(&state).await;
    if let Some(um) = state.meeting.core.unified.as_ref() {
        um.revoke_sessions_for_user(user_id, "LOGOUT").await;
    }
    Ok(Json(AccountSessionRevokeResponse {
        revoked_count: revoked,
    }))
}

/// Sign out every other session
///
/// Gap audit A20: ends every open sign-in of the account except the one making the
/// request (each with its Hub, Messaging and Calling credentials and meeting media). The
/// account stays signed in here and keeps its linked devices. Once the account has a
/// trusted device, only a trusted device may do this.
#[utoipa::path(
    post, path = "/api/v2/me/sessions/revoke-others", tag = "Account sessions (v2)", operation_id = "v2_revoke_other_account_sessions",
    responses(
        (status = 200, description = "Every other session ended", body = AccountSessionRevokeResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "DEVICE_NOT_TRUSTED (the account has a trusted device and this sign-in is not on one); REAUTHENTICATION_REQUIRED (a sign-in without a session id: sign in again)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_other_sessions(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<AccountSessionRevokeResponse>> {
    let user = actor.require_registered()?;
    let user_id = user.user_id;
    let Some(current) = user.account_session_id else {
        return Err(ApiError::domain(
            ErrorCode::ReauthenticationRequired,
            "Signing out other sessions needs a session-bound sign-in; sign in again",
        ));
    };
    if crate::services::account_devices::has_trusted_device(&state.db, user_id).await?
        && !crate::services::account_devices::session_on_trusted_device(
            &state.db,
            user_id,
            Some(current),
        )
        .await?
    {
        return Err(ApiError::domain(
            ErrorCode::DeviceNotTrusted,
            "Signing out other sessions needs a trusted device of this account once it has one",
        ));
    }
    let revoked =
        security::revoke_others(&state, user_id, Some(current), RevokeReason::UserRevoked).await?;
    security::record_event(
        &state.db,
        SecurityEvent::new("LOGOUT_OTHERS", Outcome::Success)
            .by_user(user_id)
            .session(current)
            .detail(serde_json::json!({ "sessionsEnded": revoked })),
    )
    .await?;
    Ok(Json(AccountSessionRevokeResponse {
        revoked_count: revoked,
    }))
}

/// End every session of a user (platform administrators)
///
/// Gap audit A20: ends every open sign-in of the account and every credential issued
/// before now (the account revocation epoch), without suspending or deactivating it; the
/// user can sign in again. Hub, Messaging and Calling credentials end with them. Requires a
/// platform administrator and an audited reason.
#[utoipa::path(
    post, path = "/api/v2/admin/users/{userId}/sessions/revoke", tag = "Account sessions (v2)", operation_id = "v2_admin_revoke_user_sessions",
    params(("userId" = Uuid, Path, description = "The account")),
    request_body = AdminRevokeSessionsRequest,
    responses(
        (status = 200, description = "Every session ended", body = AccountSessionRevokeResponse),
        (status = 400, description = "VALIDATION_ERROR (reason 1-500 characters)", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "USER_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_revoke_user_sessions(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(user_id): V2Path<Uuid>,
    super::V2Json(body): super::V2Json<AdminRevokeSessionsRequest>,
) -> ApiResult<Json<AccountSessionRevokeResponse>> {
    let admin = actor.require_registered()?;
    if !admin.platform_admin {
        return Err(ApiError::domain(
            ErrorCode::PermissionDenied,
            "Platform administrator required",
        ));
    }
    let reason = body.reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "reason must be 1-500 characters",
        ));
    }
    use sea_orm::EntityTrait;
    if crate::models::users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await?
        .is_none()
    {
        return Err(ApiError::domain(ErrorCode::UserNotFound, "User not found"));
    }
    let revoked = security::revoke_all(&state.db, user_id, RevokeReason::AdminRevoked).await?;
    security::record_event(
        &state.db,
        SecurityEvent::new("ADMIN_SESSIONS_REVOKED", Outcome::Success)
            .by_user(admin.user_id)
            .detail(serde_json::json!({
                "targetUserId": user_id,
                "sessionsEnded": revoked,
                // The audit row is bounded (2 KiB): the first 200 characters are kept.
                "reason": reason.chars().take(200).collect::<String>(),
            })),
    )
    .await?;
    crate::services::communications::credentials_ended(&state, user_id, "ADMIN_REVOKED").await;
    Ok(Json(AccountSessionRevokeResponse {
        revoked_count: revoked,
    }))
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminRevokeSessionsRequest {
    /// Why the sessions are ended (1-500 characters; recorded in the security audit).
    pub reason: String,
}

/// Read the account security audit (platform administrators)
///
/// Append-only account security events — sign-ins, sign-outs, password changes and resets,
/// Google linking, refresh-token reuse — newest first. Requires a platform administrator.
/// Events never contain passwords, tokens, email addresses or network addresses.
#[utoipa::path(
    get, path = "/api/v2/admin/audit-events", tag = "Account sessions (v2)", operation_id = "v2_admin_list_account_audit_events",
    params(SecurityEventQuery),
    responses(
        (status = 200, description = "Events", body = SecurityEventPage),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_account_audit_events(
    State(state): State<AppState>,
    actor: ActorContext,
    Query(query): Query<SecurityEventQuery>,
) -> ApiResult<Json<SecurityEventPage>> {
    let admin = actor.require_registered()?;
    if !admin.platform_admin {
        return Err(ApiError::domain(
            ErrorCode::PermissionDenied,
            "Platform administrator required",
        ));
    }
    if query
        .event_type
        .as_deref()
        .is_some_and(|t| t.is_empty() || t.len() > 48)
    {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "eventType is invalid",
        ));
    }
    let (events, next_cursor) = security::list_events(
        &state.db,
        query.user_id,
        query.event_type.clone(),
        query.before,
        query.limit.unwrap_or(50),
    )
    .await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("SECURITY_AUDIT_READ", Outcome::Success)
            .actor(ActorKind::Admin, Some(admin.user_id))
            .detail(serde_json::json!({ "filteredUser": query.user_id.is_some() })),
    )
    .await;
    Ok(Json(SecurityEventPage {
        events,
        next_cursor,
    }))
}

//! CF Meet Apps (ADR 0064 / 0065): the OAuth consent relay, Connected apps, organization
//! Apps and in-meeting apps. Thin transport over [`crate::services::apps`]. Each feature is
//! behind its default-off flag (`404 FEATURE_DISABLED` while off).

use axum::{
    extract::State,
    http::{header::CACHE_CONTROL, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use uuid::Uuid;

use super::{V2Json, V2Path, V2Query};
use crate::{dto::apps::*, error::ApiResult, services::actor::ActorContext, state::AppState};

fn no_store<T: serde::Serialize>(status: StatusCode, body: T) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Consent data for an app's authorization request.
///
/// The CF Meet consent page (`https://cfmeet.com/oauth/authorize`) forwards the app's
/// authorization request unchanged. The signed-in account is required; Platform Access
/// validates the client, the exact redirect URI, the scopes and PKCE (S256 only) for that
/// account. A workspace install (`install=organization&organization_id=…`) also reports
/// whether the organization allows this account to install the app.
#[utoipa::path(
    get, path = "/api/v2/oauth/authorize/context", tag = "Apps (v2)", operation_id = "v2_oauth_authorize_context",
    params(AuthorizeContextQuery),
    responses(
        (status = 200, description = "Consent data", body = AuthorizeContextResponse),
        (status = 400, description = "OAUTH_REQUEST_INVALID (the message starts with INVALID_CLIENT, INVALID_REDIRECT_URI, INVALID_SCOPE, PKCE_REQUIRED or APP_NOT_AVAILABLE; never redirect after INVALID_CLIENT / INVALID_REDIRECT_URI) / VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn authorize_context(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Query(q): V2Query<AuthorizeContextQuery>,
) -> ApiResult<Response> {
    let body = state.apps.authorize_context(&actor, &q).await?;
    Ok(no_store(StatusCode::OK, body))
}

/// Approve or deny an app's authorization request.
///
/// `APPROVE` creates (or extends) the grant at Platform Access — for a workspace install
/// only if the organization's app policy allows this account — and returns
/// `redirect_uri?code=…&state=…`; `DENY` returns `redirect_uri?error=access_denied&state=…`.
/// The browser navigates to `redirectTo`.
#[utoipa::path(
    post, path = "/api/v2/oauth/authorize/approve", tag = "Apps (v2)", operation_id = "v2_oauth_authorize_decide",
    request_body = AuthorizeDecisionRequest,
    responses(
        (status = 200, description = "Where to send the browser", body = AuthorizeDecisionResponse),
        (status = 400, description = "OAUTH_REQUEST_INVALID / VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED (the organization does not allow this install; the message starts with NOT_A_MEMBER, ROLE_REQUIRED, NOT_ALLOWLISTED or ORGANIZATION_NOT_READY)", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn authorize_decide(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<AuthorizeDecisionRequest>,
) -> ApiResult<Response> {
    let body = state.apps.authorize_decision(&actor, &body).await?;
    Ok(no_store(StatusCode::OK, body))
}

/// The signed-in account's connected apps (personal installs).
///
/// Every app the account authorized for itself (USER grants), with the scopes granted,
/// for the Connected apps settings page.
#[utoipa::path(
    get, path = "/api/v2/me/apps", tag = "Apps (v2)", operation_id = "v2_list_my_apps",
    responses(
        (status = 200, description = "Connected apps", body = ConnectedAppList),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_my_apps(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Response> {
    let body = state.apps.my_apps(&actor).await?;
    Ok(no_store(StatusCode::OK, body))
}

/// Disconnect one of the signed-in account's apps.
///
/// Revokes the grant and every token issued for it at Platform Access. A grant that is
/// not the account's own is `APP_NOT_FOUND`.
#[utoipa::path(
    delete, path = "/api/v2/me/apps/{grantId}", tag = "Apps (v2)", operation_id = "v2_revoke_my_app",
    params(("grantId" = Uuid, Path, description = "Grant id")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "APP_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_my_app(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(grant_id): V2Path<Uuid>,
) -> ApiResult<StatusCode> {
    state.apps.revoke_my_app(&actor, grant_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The apps installed for an organization.
///
/// Workspace installs (ORGANIZATION grants) of the organization, for its OWNERs and
/// ADMINs only (organization role checked with the Organizations Service).
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/apps", tag = "Apps (v2)", operation_id = "v2_list_organization_apps",
    params(("id" = Uuid, Path, description = "Organization id")),
    responses(
        (status = 200, description = "Installed apps", body = ConnectedAppList),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED (organization administrators only)", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_organization_apps(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Response> {
    let body = state.apps.organization_apps(&actor, id).await?;
    Ok(no_store(StatusCode::OK, body))
}

/// Uninstall an app from an organization.
///
/// Revokes the grant, every token and the app's bot. Organization OWNERs and ADMINs only;
/// a grant of another organization is `APP_NOT_FOUND`.
#[utoipa::path(
    delete, path = "/api/v2/organizations/{id}/apps/{grantId}", tag = "Apps (v2)", operation_id = "v2_revoke_organization_app",
    params(("id" = Uuid, Path, description = "Organization id"), ("grantId" = Uuid, Path, description = "Grant id")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED (organization administrators only)", body = ErrorEnvelope),
        (status = 404, description = "APP_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_organization_app(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, grant_id)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    state
        .apps
        .revoke_organization_app(&actor, id, grant_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Apps with an in-meeting panel for this conference.
///
/// Apps installed for the meeting owner (personal install) or for the meeting's
/// organization that declare a meeting surface. Admitted participants only.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/apps", tag = "Apps (v2)", operation_id = "v2_list_conference_apps",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "In-meeting apps", body = ConferenceAppList),
        (status = 404, description = "CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_conference_apps(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Response> {
    let body = state.apps.conference_apps(&state, &actor, id).await?;
    Ok(no_store(StatusCode::OK, body))
}

/// A 5-minute app-context token for an in-meeting app.
///
/// An Ed25519 JWS (`aud` = the app's client id) carrying the conference, a pairwise
/// participant id and the participant's role, plus the user's pairwise `sub` only if that
/// user granted the app. Verify it with `GET /api/v2/app-context-keys`.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/apps/{appId}/context", tag = "Apps (v2)", operation_id = "v2_create_app_context",
    params(("id" = Uuid, Path, description = "Conference id"), ("appId" = Uuid, Path, description = "App id (OAuth client id)")),
    responses(
        (status = 200, description = "Context token", body = AppContextTokenResponse),
        (status = 404, description = "CONFERENCE_NOT_FOUND / APP_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn create_app_context(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, app_id)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let body = state.apps.app_context(&state, &actor, id, app_id).await?;
    Ok(no_store(StatusCode::OK, body))
}

/// Public keys of app-context tokens (JWKS).
///
/// The Ed25519 keys (OKP JWKs, `kid` = RFC 7638 thumbprint) apps use to verify the
/// app-context tokens of their in-meeting panel. Anonymous; cacheable for 5 minutes.
#[utoipa::path(
    get, path = "/api/v2/app-context-keys", tag = "Apps (v2)", operation_id = "v2_app_context_keys",
    responses(
        (status = 200, description = "JWKS", body = AppContextKeySet),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
    ),
    security(())
)]
pub async fn app_context_keys(State(state): State<AppState>) -> ApiResult<Response> {
    let jwks = state.apps.app_context_keys()?;
    let mut r = (StatusCode::OK, Json(jwks)).into_response();
    r.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    Ok(r)
}

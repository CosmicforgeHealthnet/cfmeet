//! Part VI §51 receiver branch: platform-credential requests forwarded by the API Gateway.
//!
//! Applied to every `/api/v2` route. A request WITHOUT an internal service token is left
//! untouched (CF Meet sessions, participant tokens and anonymous callers keep exactly
//! their existing rules). A request WITH one (`X-CF-Service-Authorization`) is a
//! gateway-forwarded machine caller and must pass, in order:
//!
//! 1. no `Authorization` / `X-Participant-Token` header next to it (never mixed);
//! 2. platform access enabled (`MEETING_PLATFORM_ACCESS_ENABLED`);
//! 3. the token verifies against the Meeting Service's own trust bundle, its issuer is
//!    `cfmeet.api-gateway` AND is granted `meeting-request.forward`, and its signed
//!    delegated context names the credential the gateway authenticated
//!    (`initiating_credential_id`);
//! 4. the matched route is a machine-callable operation
//!    ([`crate::services::platform::operations`]);
//! 5. Platform Access re-verifies that credential LIVE
//!    (`/internal/v1/credentials/delegated-authority`): the returned chain must be exactly
//!    the delegated tenant / application / environment / service account / credential,
//!    with FULL authority;
//! 6. the credential holds the operation's scope (403 otherwise — defence in depth over
//!    the gateway's pre-check);
//! 7. a meeting named in the path belongs to the caller's application environment —
//!    otherwise 404, exactly as for an unknown id (user-owned and other environments'
//!    meetings do not exist for a machine caller).
//!
//! Every failure of 1–5 is the same normalized 401 (`AUTHENTICATION_REQUIRED`); Platform
//! Access unreachable is 503 (fail closed). The bounded reason is logged for operators,
//! never a header value. On success the [`PlatformActor`] (from the authority chain only)
//! is inserted into the request extensions — the only way an `ActorContext` becomes a
//! platform actor — and the request continues the gateway's signed correlation id.

use axum::{
    extract::{MatchedPath, OriginalUri, Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{headers::SERVICE_AUTHORIZATION, InternalPermission, ServiceId};
use sea_orm::EntityTrait;
use uuid::Uuid;

use crate::{
    error::{ApiError, ErrorCode},
    services::{
        actor::{PlatformActor, PARTICIPANT_TOKEN_HEADER},
        apps::delegated::{self, DelegatedUser},
        meeting_core::{find_conference, find_space, platform_owns},
        platform::{
            authority::{Authentication, OauthCredential, OauthCredentialKind},
            operations::{self, PlatformOperation, Target},
            with_correlation,
        },
    },
    state::AppState,
};

fn refuse(reason: &'static str, route: &str) -> Response {
    tracing::info!(reason, route, "gateway-forwarded platform request refused");
    ApiError::domain(ErrorCode::AuthenticationRequired, "Authentication failed").into_response()
}

/// The value of the `:id` segment of `route` in `path` (both `/`-separated).
fn id_param<'a>(route: &str, path: &'a str) -> Option<&'a str> {
    let r: Vec<&str> = route.split('/').collect();
    let p: Vec<&'a str> = path.split('/').collect();
    if r.len() != p.len() {
        return None;
    }
    r.iter()
        .zip(p)
        .find(|(seg, _)| **seg == ":id")
        .map(|(_, v)| v)
}

/// Step 7: the named meeting must be owned by the actor's environment (404 otherwise).
async fn guard_target(
    state: &AppState,
    op: &PlatformOperation,
    path: &str,
    actor: &PlatformActor,
) -> Result<(), ApiError> {
    let db = &state.meeting.core.db;
    match op.target {
        Target::None => Ok(()),
        Target::Space => {
            let not_found = || ApiError::domain(ErrorCode::MeetingNotFound, "Meeting not found");
            let id = id_param(op.route, path).ok_or_else(not_found)?;
            let id = urlencoding::decode(id).map_err(|_| not_found())?;
            let space = match state.meeting.compat.resolve(&id).await {
                Ok(r) => r.space,
                Err(e) if e.code() == Some(ErrorCode::MeetingNotFound) => return Err(not_found()),
                Err(e) => return Err(e),
            };
            platform_owns(&space, actor)
                .then_some(())
                .ok_or_else(not_found)
        }
        Target::Conference => {
            let not_found =
                || ApiError::domain(ErrorCode::ConferenceNotFound, "Conference not found");
            let id = id_param(op.route, path)
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or_else(not_found)?;
            let conference = match find_conference(db, id).await {
                Ok(c) => c,
                Err(e) if e.code() == Some(ErrorCode::ConferenceNotFound) => {
                    return Err(not_found())
                }
                Err(e) => return Err(e),
            };
            let space = match find_space(db, conference.meeting_space_id).await {
                Ok(s) => s,
                Err(e) if e.code() == Some(ErrorCode::MeetingNotFound) => return Err(not_found()),
                Err(e) => return Err(e),
            };
            platform_owns(&space, actor)
                .then_some(())
                .ok_or_else(not_found)
        }
    }
}

/// Step 7 for a delegated user: write = meetings the user owns; read = owned, or HOST /
/// CO_HOST member. Anything else is 404, exactly as for an unknown id.
async fn guard_delegated(
    state: &AppState,
    op: &PlatformOperation,
    path: &str,
    user: &DelegatedUser,
) -> Result<(), ApiError> {
    let db = &state.meeting.core.db;
    let space = match op.target {
        Target::None => return Ok(()),
        Target::Space => {
            let not_found = || delegated::not_found(false);
            let id = id_param(op.route, path).ok_or_else(not_found)?;
            let id = urlencoding::decode(id).map_err(|_| not_found())?;
            match state.meeting.compat.resolve(&id).await {
                Ok(r) => r.space,
                Err(e) if e.code() == Some(ErrorCode::MeetingNotFound) => return Err(not_found()),
                Err(e) => return Err(e),
            }
        }
        Target::Conference => {
            let not_found = || delegated::not_found(true);
            let id = id_param(op.route, path)
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or_else(not_found)?;
            let conference = match find_conference(db, id).await {
                Ok(c) => c,
                Err(e) if e.code() == Some(ErrorCode::ConferenceNotFound) => {
                    return Err(not_found())
                }
                Err(e) => return Err(e),
            };
            match find_space(db, conference.meeting_space_id).await {
                Ok(s) => s,
                Err(e) if e.code() == Some(ErrorCode::MeetingNotFound) => return Err(not_found()),
                Err(e) => return Err(e),
            }
        }
    };
    if user.may_act_on(db, &space, op.scope).await? {
        Ok(())
    } else {
        Err(delegated::not_found(op.target == Target::Conference))
    }
}

/// ADR 0064: the `DelegatedUser` branch (credential kind `OAUTH_ACCESS`).
#[allow(clippy::too_many_arguments)]
async fn delegated_user(
    state: &AppState,
    mut request: Request,
    next: Next,
    op: &'static PlatformOperation,
    route: &str,
    delegated_ctx: &cf_service_auth::DelegatedPlatformContext,
    oauth: Result<OauthCredential, ()>,
    scopes: std::collections::BTreeSet<String>,
    correlation: cf_service_auth::CorrelationId,
) -> Response {
    if !state.apps.delegated_enabled {
        return refuse("oauth_credentials_disabled", route);
    }
    let Ok(oauth) = oauth else {
        return refuse("oauth_credential_incomplete", route);
    };
    if oauth.kind != OauthCredentialKind::Access {
        return refuse("oauth_bot_not_a_meetings_actor", route);
    }
    // The gateway names the restricted principal; it must be exactly the credential's.
    if let Some(p) = delegated_ctx.initiating_principal() {
        if Some(p.principal_id) != oauth.restricted_principal
            || p.principal_kind != cf_service_auth::DelegatedPrincipalKind::NativeCfUser
        {
            return refuse(delegated::Refusal::PrincipalMismatch.as_str(), route);
        }
    }
    let account = match delegated::account_of(&state.db, &oauth).await {
        Ok(Ok(a)) => a,
        Ok(Err(r)) => return refuse(r.as_str(), route),
        Err(e) => return e.into_response(),
    };
    let user = match crate::models::users::Entity::find_by_id(account)
        .one(&state.db)
        .await
    {
        Ok(Some(u)) => u,
        Ok(None) => return refuse(delegated::Refusal::UnknownAccount.as_str(), route),
        Err(e) => return ApiError::from(e).into_response(),
    };
    let cutoff = match crate::services::account_security::credentials_cutoff(state, account).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    if let Err(r) = delegated::check_account(
        user.status.clone(),
        user.role.clone(),
        cutoff,
        oauth.issued_at,
    ) {
        return refuse(r.as_str(), route);
    }
    let user = DelegatedUser {
        account_id: account,
        app_id: oauth.app_id,
        grant_id: oauth.grant_id,
        scopes,
    };
    // 6. (OAuth scopes map to the same internal meetings scopes.)
    if !user.has_scope(op.scope) {
        tracing::info!(route, scope = op.scope, "delegated scope check denied");
        return ApiError::domain(
            ErrorCode::PermissionDenied,
            "The app was not granted the scope this operation requires",
        )
        .into_response();
    }
    // 7.
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    if let Err(e) = guard_delegated(state, op, &path, &user).await {
        return e.into_response();
    }
    tracing::info!(route, account_id = %user.account_id, app_id = %user.app_id,
        grant_id = %user.grant_id, "delegated app request admitted (via app)");
    let (app, grant) = (user.app_id, user.grant_id);
    request.extensions_mut().insert(user);
    with_correlation(
        correlation,
        delegated::with_via_app(app, grant, next.run(request)),
    )
    .await
}

pub async fn platform_credential(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if !headers.contains_key(SERVICE_AUTHORIZATION) {
        return next.run(request).await;
    }
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "<unmatched>".into());
    // 1. A service token never travels next to a customer / participant credential.
    if headers.contains_key(header::AUTHORIZATION)
        || headers.contains_key(&PARTICIPANT_TOKEN_HEADER)
    {
        return refuse("credential_next_to_service_token", &route);
    }
    // 2.
    let Some(receiver) = state.platform.clone() else {
        return refuse("platform_access_disabled", &route);
    };
    // 3. SEC-012: bound to this request (method + received path, before nest
    // stripping); single-use on mutating methods.
    let received = request
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    let method = request.method().clone();
    let verified = receiver
        .verifier
        .verify_request(&method, &received, headers)
        .await;
    let caller = match verified {
        Ok(c) => c,
        Err(e) => {
            let d = &e.diagnostics;
            tracing::info!(
                failure = e.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                "service token on a meetings route did not verify"
            );
            return refuse("service_token_invalid", &route);
        }
    };
    if caller.authenticated_service() != &ServiceId::api_gateway()
        || caller
            .require_permission(InternalPermission::MeetingRequestForward)
            .is_err()
    {
        tracing::info!(caller = %caller.authenticated_service(), "service token without meeting-request.forward");
        return refuse("forward_not_permitted", &route);
    }
    let Some(delegated) = caller
        .delegated_platform_context()
        .filter(|d| d.initiating_credential_id().is_some())
        .cloned()
    else {
        return refuse("delegated_credential_missing", &route);
    };
    // 4.
    let Some(op) = operations::lookup(request.method(), &route) else {
        return refuse("not_a_platform_operation", &route);
    };
    // 5.
    let correlation = caller.correlation_id().clone();
    let chain = match receiver
        .authority
        .delegated_authority(&delegated, &correlation)
        .await
    {
        Ok(Authentication::Authenticated(chain)) => chain,
        Ok(Authentication::Rejected { reason }) => {
            tracing::info!(%reason, credential_id = ?delegated.initiating_credential_id(),
                "platform credential rejected by Platform Access");
            return refuse("credential_rejected", &route);
        }
        Err(e) => {
            tracing::error!(error = %e, "credential authority (Platform Access) unavailable; request refused");
            return ApiError::domain(
                ErrorCode::ServiceUnavailable,
                "The service is temporarily unavailable. Please retry.",
            )
            .into_response();
        }
    };
    if !chain.is_named_by(&delegated) {
        return refuse("delegated_scope_mismatch", &route);
    }
    if !chain.full_authority {
        return refuse("scope_suspended", &route);
    }
    // ADR 0064: an OAuth credential is never a machine caller. With delegated apps on, an
    // access token acts for its user (`DelegatedUser`); bot tokens never touch meetings.
    if let Some(oauth) = chain.oauth.clone() {
        return delegated_user(
            &state,
            request,
            next,
            op,
            &route,
            &delegated,
            oauth,
            chain.actor.scopes.clone(),
            correlation,
        )
        .await;
    }
    let actor = chain.actor;
    // 6.
    if !actor.has_scope(op.scope) {
        tracing::info!(route, scope = op.scope, "scope check denied");
        return ApiError::domain(
            ErrorCode::PermissionDenied,
            "The credential does not hold the scope this operation requires",
        )
        .into_response();
    }
    // 7.
    // Nested routers see a prefix-stripped URI; the matched template is the full path.
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    if let Err(e) = guard_target(&state, op, &path, &actor).await {
        return e.into_response();
    }
    let span = tracing::Span::current();
    tracing::info!(parent: &span, route, tenant_id = %actor.tenant_id, environment_id = %actor.environment_id,
        service_account_id = %actor.service_account_id, credential_id = %actor.credential_id,
        "platform credential request admitted");
    request.extensions_mut().insert(actor);
    with_correlation(correlation, next.run(request)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_segment_is_read_from_the_matched_template() {
        assert_eq!(
            id_param("/api/v2/conferences/:id/end", "/api/v2/conferences/abc/end"),
            Some("abc")
        );
        assert_eq!(
            id_param(
                "/api/v2/meeting-spaces/:id/invitations/:invitation_id/revoke",
                "/api/v2/meeting-spaces/s1/invitations/i1/revoke"
            ),
            Some("s1")
        );
        assert_eq!(
            id_param("/api/v2/conferences/:id", "/api/v2/conferences/a/b"),
            None
        );
        assert_eq!(
            id_param("/api/v2/meeting-spaces", "/api/v2/meeting-spaces"),
            None
        );
    }
}

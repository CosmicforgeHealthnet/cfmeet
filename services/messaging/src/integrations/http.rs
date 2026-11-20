//! Internal integration routes (`/internal/v1`, service-authenticated, delegated scope).
//!
//! | Route | Permission |
//! |---|---|
//! | `POST /meeting-conversations` | `messaging-meeting.sync` |
//! | `PUT /meeting-conversations/{conversationId}/members/{principalId}` | `messaging-meeting.sync` |
//! | `POST /meeting-conversations/{conversationId}/lifecycle` | `messaging-meeting.sync` |
//! | `POST /meeting-conversations/{conversationId}/legacy-imports` | `messaging-meeting.sync` |
//! | `POST /meeting-conversations/{conversationId}/messages/{messageId}/moderation` | `messaging-meeting.sync` |
//! | `POST /context-authorities` | `messaging-context-authority.apply` (Privacy Relay) |
//! | `PUT /team-channels` | `messaging-organization.govern` (Meeting Service, gap audit G7) |

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
        middleware::{
            delegated_context, delegated_context_for_restriction, verify_principal, InternalCaller,
        },
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
        .route("/context-authorities", post(context_authority))
        .route("/team-channels", put(team_channel))
}

/// The meeting-sync caller's re-validated scope: a first-party scope
/// (`messaging-meeting.sync`), or — ADR 0062 §8 partner meeting chat, only while
/// `MESSAGING_PARTNER_SESSION_DEVICES_ENABLED` is on — an ACTIVE EXTERNAL environment
/// (`messaging-meeting.sync-platform`; Platform Access re-validates the signed delegated
/// scope). With the partner flag off this is exactly the first-party rule.
async fn meeting_scope(
    state: &AppState,
    caller: &InternalCaller,
) -> Result<TrustedContext, ApiError> {
    let first_party_sync = caller
        .context()
        .has_permission(InternalPermission::MessagingMeetingSync);
    let platform_sync = state.partner.settings.session_devices_enabled
        && caller
            .context()
            .has_permission(InternalPermission::MessagingMeetingSyncPlatform);
    if !platform_sync {
        caller.require_permission(InternalPermission::MessagingMeetingSync)?;
    }
    let ctx = delegated_context(state, caller).await?;
    match (ctx.first_party, first_party_sync, platform_sync) {
        (true, true, _) => Ok(ctx),
        (true, false, _) => Err(ApiError::code(
            crate::error::ErrorCode::InternalPermissionDenied,
        )),
        (false, _, true) => {
            state
                .partner
                .ensure_baseline(&state.db, &ctx.scope, state.now())
                .await;
            Ok(ctx)
        }
        (false, _, false) => Err(ApiError::code(
            crate::error::ErrorCode::DelegatedContextInvalid,
        )),
    }
}

async fn resolve(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<ResolveRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = meeting_scope(&state, &caller).await?;
    // ADR 0062 §8: a partner meeting's chat is MANAGED (partner users read it with their
    // MANAGED-only partner session devices).
    if !ctx.first_party && body.security_profile != crate::domain::SecurityProfile::Managed {
        return Err(ApiError::reason(
            crate::error::ErrorCode::SecurityProfileNotAllowed,
            "PARTNER_MEETING_CHAT_MANAGED_ONLY",
        ));
    }
    let owner = verify_principal(&state, &ctx, body.owner_principal_id).await?;
    // The owner of a partner meeting's chat is the environment's meeting system
    // principal (`cfmeet:meeting-system`, BOT_OR_SERVICE).
    if !ctx.first_party && owner.kind != "BOT_OR_SERVICE" {
        return Err(ApiError::reason(
            crate::error::ErrorCode::PrincipalNotAuthorized,
            "PARTNER_MEETING_OWNER_KIND",
        ));
    }
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
        let member = verify_principal(&state, &ctx, principal_id).await?;
        // ADR 0062 §8: a partner meeting admits that environment's partner users only
        // (another environment's principal is already unknown here).
        if !ctx.first_party
            && !matches!(
                member.kind.as_str(),
                "EXTERNAL_APPLICATION_PRINCIPAL" | "TEMPORARY_EXTERNAL_PARTICIPANT"
            )
        {
            return Err(ApiError::reason(
                crate::error::ErrorCode::PrincipalNotAuthorized,
                "PARTNER_MEETING_MEMBER_KIND",
            ));
        }
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

/// Gap audit G7: create / resync a team channel from a team snapshot, in the caller's
/// (the organization's) first-party scope.
async fn team_channel(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<super::team_channels::TeamSnapshot>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_permission(InternalPermission::MessagingOrganizationGovern)?;
    let ctx = delegated_context(&state, &caller).await?;
    if !ctx.first_party {
        return Err(ApiError::code(
            crate::error::ErrorCode::DelegatedContextInvalid,
        ));
    }
    body.validate()?;
    let owner = verify_principal(&state, &ctx, body.owner_principal_id).await?;
    // Only principals ACTIVE now join; one that is not is reported (and, if it joined by
    // the team before, removed).
    let mut inactive = std::collections::BTreeSet::new();
    for p in body
        .member_principal_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
    {
        if verify_principal(&state, &ctx, *p).await.is_err() {
            inactive.insert(*p);
        }
    }
    let (status, view) = super::team_channels::sync(&state, &ctx, &owner, body, inactive).await?;
    state.wake_outbox();
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(view),
    ))
}

async fn context_authority(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<super::context_authority::AuthorityUpdate>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_permission(InternalPermission::MessagingContextAuthorityApply)?;
    // Restrictions apply even to a suspended tenant / application; granting (ACTIVE)
    // needs the full authority of the chain.
    let (ctx, full) = delegated_context_for_restriction(&state, &caller).await?;
    if body.state == "ACTIVE" && !full {
        return Err(ApiError::code(crate::error::ErrorCode::AuthoritySuspended));
    }
    let issuer = caller.context().authenticated_service().to_string();
    let view = super::context_authority::apply(&state, &ctx, &issuer, body).await?;
    state.wake_outbox();
    Ok(Json(view))
}

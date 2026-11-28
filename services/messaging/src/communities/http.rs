//! Communities routes (ADR 0073; nested under `/v1/messaging`; acting principal required).
//! Reads need `messages.read`, changes `messages.send`.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::de::DeserializeOwned;
use uuid::Uuid;

use super::{groups, invites, PageQuery};
use crate::{
    conversations::http::MESSAGES_READ,
    http::{
        error::ApiError,
        middleware::{Acting, CustomerCaller},
    },
    membership::http::MESSAGES_SEND,
    messages::http::acting_with,
    social::idempotency_key,
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/communities", get(list).post(create))
        .route(
            "/communities/preferences",
            get(preferences).put(set_preferences),
        )
        .route("/communities/invitations", get(my_invitations))
        .route(
            "/communities/invitations/:invitation_id/accept",
            post(accept_invitation),
        )
        .route(
            "/communities/invitations/:invitation_id/decline",
            post(decline_invitation),
        )
        .route("/communities/invite-links/preview", post(preview_link))
        .route("/communities/invite-links/join", post(join_link))
        .route("/communities/join-requests", get(my_requests))
        .route(
            "/communities/join-requests/:request_id/cancel",
            post(cancel_request),
        )
        .route(
            "/communities/:community_id",
            get(get_community)
                .patch(update_community)
                .delete(delete_community),
        )
        .route("/communities/:community_id/owner", post(transfer))
        .route("/communities/:community_id/leave", post(leave))
        .route("/communities/:community_id/members", get(members))
        .route(
            "/communities/:community_id/members/:principal_id",
            axum::routing::delete(remove_member),
        )
        .route(
            "/communities/:community_id/members/:principal_id/role",
            put(set_role),
        )
        .route("/communities/:community_id/bans", get(bans).post(ban))
        .route(
            "/communities/:community_id/bans/:principal_id",
            axum::routing::delete(unban),
        )
        .route(
            "/communities/:community_id/invitations",
            get(invitations).post(invite),
        )
        .route(
            "/communities/:community_id/invitations/:invitation_id",
            axum::routing::delete(revoke_invitation),
        )
        .route(
            "/communities/:community_id/invite-links",
            get(links).post(create_link),
        )
        .route(
            "/communities/:community_id/invite-links/:link_id/revoke",
            post(revoke_link),
        )
        .route("/communities/:community_id/join-requests", get(requests))
        .route(
            "/communities/:community_id/join-requests/:request_id/approve",
            post(approve_request),
        )
        .route(
            "/communities/:community_id/join-requests/:request_id/reject",
            post(reject_request),
        )
        .route(
            "/communities/:community_id/groups",
            get(list_groups).post(create_group),
        )
        .route("/communities/:community_id/groups/link", post(link_group))
        .route(
            "/communities/:community_id/groups/order",
            put(reorder_groups),
        )
        .route(
            "/communities/:community_id/groups/:conversation_id",
            axum::routing::patch(update_group).delete(unlink_group),
        )
        .route(
            "/communities/:community_id/groups/:conversation_id/join",
            post(join_group),
        )
}

type Reply = Result<axum::response::Response, ApiError>;

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

// ------------------------------------------------------------------------- communities

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(super::list(&state, caller.context(), a.principal_id, q).await?)
}

async fn create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<super::CreateRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    let key = idempotency_key(&headers)?;
    let (s, v) = super::create(&state, caller.context(), &a, body, key).await?;
    with_status(s, v)
}

async fn get_community(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(super::get(&state, caller.context(), a.principal_id, id).await?)
}

async fn update_community(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<super::UpdateRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(super::update(&state, caller.context(), &a, id, body).await?)
}

async fn delete_community(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(super::delete(&state, caller.context(), &a, id).await?)
}

async fn transfer(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<super::OwnerRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(super::transfer(&state, caller.context(), &a, id, body).await?)
}

async fn leave(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(super::leave(&state, caller.context(), &a, id).await?)
}

async fn preferences(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(super::preferences(&state, caller.context(), a.principal_id).await?)
}

async fn set_preferences(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<super::Preferences>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(super::set_preferences(&state, caller.context(), a.principal_id, body).await?)
}

// ----------------------------------------------------------------------------- members

async fn members(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(super::members(&state, caller.context(), a.principal_id, id, q).await?)
}

async fn set_role(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, principal)): Path<(Uuid, Uuid)>,
    Json(body): Json<super::RoleRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(super::set_role(&state, caller.context(), &a, id, principal, body).await?)
}

async fn remove_member(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, principal)): Path<(Uuid, Uuid)>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(super::remove(&state, caller.context(), &a, id, principal).await?)
}

async fn bans(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(invites::bans(&state, caller.context(), a.principal_id, id).await?)
}

async fn ban(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<invites::BanRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    with_status(
        201,
        invites::ban(&state, caller.context(), &a, id, body).await?,
    )
}

async fn unban(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, principal)): Path<(Uuid, Uuid)>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(invites::unban(&state, caller.context(), &a, id, principal).await?)
}

// ------------------------------------------------------------------------- invitations

async fn invitations(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(invites::invitations(&state, caller.context(), a.principal_id, id).await?)
}

async fn invite(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<invites::InviteRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(invites::invite(&state, caller.context(), &a, id, body).await?)
}

async fn revoke_invitation(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, invitation)): Path<(Uuid, Uuid)>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(invites::revoke_invitation(&state, caller.context(), &a, id, invitation).await?)
}

async fn my_invitations(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(invites::my_invitations(&state, caller.context(), a.principal_id).await?)
}

async fn accept_invitation(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(invitation): Path<Uuid>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    let (s, v) = invites::answer(&state, caller.context(), &a, invitation, true).await?;
    with_status(s, v)
}

async fn decline_invitation(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(invitation): Path<Uuid>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    let (s, v) = invites::answer(&state, caller.context(), &a, invitation, false).await?;
    with_status(s, v)
}

// ------------------------------------------------------------------------ invite links

async fn links(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(invites::links(&state, caller.context(), a.principal_id, id).await?)
}

async fn create_link(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    let body: invites::CreateLinkRequest = optional(&body)?;
    with_status(
        201,
        invites::create_link(&state, caller.context(), &a, id, body).await?,
    )
}

async fn revoke_link(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, link)): Path<(Uuid, Uuid)>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(invites::revoke_link(&state, caller.context(), &a, id, link).await?)
}

async fn preview_link(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<invites::TokenRequest>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(invites::preview_link(&state, caller.context(), a.principal_id, body).await?)
}

async fn join_link(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<invites::TokenRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    let (s, v) = invites::join_link(&state, caller.context(), &a, body).await?;
    with_status(s, v)
}

// ----------------------------------------------------------------------- join requests

async fn requests(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<invites::RequestQuery>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(invites::requests(&state, caller.context(), a.principal_id, id, q).await?)
}

async fn approve_request(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, request)): Path<(Uuid, Uuid)>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(invites::decide(&state, caller.context(), &a, id, request, true).await?)
}

async fn reject_request(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, request)): Path<(Uuid, Uuid)>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(invites::decide(&state, caller.context(), &a, id, request, false).await?)
}

async fn my_requests(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(invites::my_requests(&state, caller.context(), a.principal_id, q).await?)
}

async fn cancel_request(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(request): Path<Uuid>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(invites::cancel_request(&state, caller.context(), a.principal_id, request).await?)
}

// ------------------------------------------------------------------------------ groups

async fn list_groups(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let a = reader(&state, &caller, &headers).await?;
    ok(groups::list(&state, caller.context(), a.principal_id, id).await?)
}

async fn create_group(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<groups::CreateGroupRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    let key = idempotency_key(&headers)?;
    let (s, v) = groups::create(&state, caller.context(), &a, id, body, key).await?;
    with_status(s, v)
}

async fn link_group(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<groups::LinkRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    with_status(
        201,
        groups::link(&state, caller.context(), &a, id, body).await?,
    )
}

async fn reorder_groups(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<groups::OrderRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(groups::reorder(&state, caller.context(), &a, id, body).await?)
}

async fn update_group(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, conversation)): Path<(Uuid, Uuid)>,
    Json(body): Json<groups::UpdateGroupRequest>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(groups::update(&state, caller.context(), &a, id, conversation, body).await?)
}

async fn unlink_group(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, conversation)): Path<(Uuid, Uuid)>,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    ok(groups::unlink(&state, caller.context(), &a, id, conversation).await?)
}

async fn join_group(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, conversation)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> Reply {
    let a = writer(&state, &caller, &headers).await?;
    let body: groups::JoinGroupRequest = optional(&body)?;
    let (s, v) = groups::join(&state, caller.context(), &a, id, conversation, body).await?;
    with_status(s, v)
}

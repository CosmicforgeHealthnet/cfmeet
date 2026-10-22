//! Membership routes (nested under `/v1/conversations`).
//!
//! | Route | Access |
//! |---|---|
//! | `GET /{id}/members` | `messages.read` + acting member, or `conversations.manage` (application) |
//! | `POST /{id}/members` | `conversations.manage`; with an acting principal it must be OWNER / ADMIN |
//! | `DELETE /{id}/members/{principalId}` | leaving (acting = member): `messages.send`; removing another: `conversations.manage` (+ OWNER / ADMIN when a principal acts) |
//! | `PATCH /{id}/members/{principalId}` (role MEMBER ↔ ADMIN) | `conversations.manage` (+ OWNER / ADMIN when a principal acts) |
//! | `GET, POST /{id}/bans`, `DELETE /{id}/bans/{principalId}` | `conversations.manage` (+ OWNER / ADMIN when a principal acts) |

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{AddRequest, Administrator, BanRequest, RoleChange};
use crate::{
    conversations::http::{CONVERSATIONS_MANAGE, MESSAGES_READ},
    http::{
        error::ApiError,
        middleware::{acting_header, verify_principal, CustomerCaller},
    },
    state::AppState,
};

pub const MESSAGES_SEND: &str = "messages.send";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/:conversation_id/members", get(list).post(add))
        .route(
            "/:conversation_id/members/:principal_id",
            delete(remove).patch(change_role),
        )
        .route("/:conversation_id/bans", get(list_bans).post(ban))
        .route("/:conversation_id/bans/:principal_id", delete(unban))
        .route(
            "/:conversation_id/invitation/accept",
            post(accept_invitation),
        )
        .route(
            "/:conversation_id/invitation/decline",
            post(decline_invitation),
        )
}

/// Routes nested under `/v1/messaging`.
pub fn customer_routes() -> Router<AppState> {
    Router::new().route("/invitations", get(list_invitations))
}

async fn accept_invitation(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting =
        crate::messages::http::acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::answer_invitation(&state, caller.context(), &acting, id, true).await?,
    ))
}

async fn decline_invitation(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting =
        crate::messages::http::acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::answer_invitation(&state, caller.context(), &acting, id, false).await?,
    ))
}

async fn list_invitations(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let acting =
        crate::messages::http::acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::invitations(&state, caller.context(), &acting).await?,
    ))
}

/// The administrator of a request: the verified acting principal, or the application.
pub async fn administrator(
    state: &AppState,
    caller: &CustomerCaller,
    headers: &HeaderMap,
) -> Result<Administrator, ApiError> {
    let ctx = caller.require_scope(CONVERSATIONS_MANAGE)?;
    Ok(match acting_header(headers)? {
        Some(p) => Administrator::Principal(verify_principal(state, ctx, p).await?),
        None => Administrator::Application,
    })
}

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<super::ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = match acting_header(&headers)? {
        Some(p) => {
            let ctx = caller.require_scope(MESSAGES_READ)?;
            Some(verify_principal(&state, ctx, p).await?)
        }
        None => {
            caller.require_scope(CONVERSATIONS_MANAGE)?;
            None
        }
    };
    Ok(Json(
        super::list(&state, caller.context(), acting.as_ref(), id, q).await?,
    ))
}

async fn add(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<AddRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = administrator(&state, &caller, &headers).await?;
    let (status, view) = super::add(&state, caller.context(), &admin, id, body).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(view),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveQuery {
    #[serde(default)]
    reason: Option<String>,
}

async fn remove(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, principal)): Path<(Uuid, Uuid)>,
    Query(q): Query<RemoveQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_header(&headers)?;
    let admin = if acting == Some(principal) {
        let ctx = caller.require_scope(MESSAGES_SEND)?;
        Administrator::Principal(verify_principal(&state, ctx, principal).await?)
    } else {
        administrator(&state, &caller, &headers).await?
    };
    Ok(Json(
        super::remove(&state, caller.context(), &admin, id, principal, q.reason).await?,
    ))
}

async fn change_role(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, principal)): Path<(Uuid, Uuid)>,
    Json(body): Json<RoleChange>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = administrator(&state, &caller, &headers).await?;
    Ok(Json(
        super::change_role(&state, caller.context(), &admin, id, principal, body).await?,
    ))
}

async fn ban(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<BanRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = administrator(&state, &caller, &headers).await?;
    Ok((
        StatusCode::CREATED,
        Json(super::ban(&state, caller.context(), &admin, id, body).await?),
    ))
}

async fn unban(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, principal)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = administrator(&state, &caller, &headers).await?;
    Ok(Json(
        super::unban(&state, caller.context(), &admin, id, principal).await?,
    ))
}

async fn list_bans(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = administrator(&state, &caller, &headers).await?;
    Ok(Json(
        super::bans(&state, caller.context(), &admin, id).await?,
    ))
}

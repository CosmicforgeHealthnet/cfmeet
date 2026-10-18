//! Realtime routes.
//!
//! Device-direct (`/v1/realtime`, never a Platform Access credential):
//!
//! | Route | Authentication |
//! |---|---|
//! | `POST /v1/realtime/tickets` | `Authorization: CF-Client-Token <token>` (a Messaging Client Access Token) |
//! | `GET /v1/realtime` (WebSocket) | `Sec-WebSocket-Protocol: cfmeet.messaging.v1, cfmeet.ticket.<ticket>` (single use, seconds) |
//!
//! Application backend (`/v1/messaging`, Platform Access credential + acting principal):
//!
//! | Route | Scope |
//! |---|---|
//! | `POST /client-tokens` | `messages.read` |
//! | `POST /client-sessions/{sessionId}/revoke` | `messages.send` |
//! | `GET` / `PUT /presence/settings` | `messages.read` / `messages.send` |
//! | `PUT` / `DELETE /devices/{deviceId}/push-registration` | `messages.send` |

use axum::{
    extract::{ws::WebSocketUpgrade, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use uuid::Uuid;

use super::{
    connection::Connection,
    hub::Admission,
    presence::{self, SettingsRequest},
    push::{self, RegistrationRequest},
    sessions::{self, IssueRequest},
    SUBPROTOCOL, TICKET_PROTOCOL_PREFIX,
};
use crate::{
    conversations::http::MESSAGES_READ,
    http::{
        error::{ApiError, ErrorCode},
        middleware::CustomerCaller,
    },
    membership::http::MESSAGES_SEND,
    messages::http::acting_with,
    state::AppState,
};

/// Device-direct routes (merged into the public router; they authenticate themselves).
pub fn device_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/realtime", get(connect))
        .route("/v1/realtime/tickets", post(ticket))
}

/// Application-backend routes nested under `/v1/messaging`.
pub fn customer_routes() -> Router<AppState> {
    Router::new()
        .route("/client-tokens", post(issue))
        .route("/client-sessions/:session_id/revoke", post(revoke))
        .route("/presence/settings", get(get_presence).put(put_presence))
        .route(
            "/devices/:device_id/push-registration",
            put(register_push).delete(unregister_push),
        )
}

fn client_token(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > 2_100 {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("cf-client-token")
        .then(|| token.trim().to_string())
        .filter(|t| !t.is_empty())
}

async fn ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = zeroize::Zeroizing::new(
        client_token(&headers).ok_or(ApiError::code(ErrorCode::AuthenticationFailed))?,
    );
    let session = sessions::authenticate(&state, &token).await?;
    drop(token);
    if !session.has_scope(MESSAGES_READ) {
        return Err(ApiError::code(ErrorCode::InsufficientScope));
    }
    Ok((
        StatusCode::CREATED,
        Json(sessions::issue_ticket(&state, &session).await?),
    ))
}

fn refuse(status: StatusCode, code: ErrorCode, reason: &'static str) -> Response {
    let mut r = ApiError::reason(code, reason).into_response();
    *r.status_mut() = status;
    r
}

async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let settings = state.realtime.settings.clone();
    if settings.require_tls
        && headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            != Some("https")
    {
        return refuse(
            StatusCode::FORBIDDEN,
            ErrorCode::AuthenticationFailed,
            "TLS_REQUIRED",
        );
    }
    // Cross-site WebSocket hijacking: a browser always sends Origin; only listed origins
    // may connect. Native clients send none.
    if let Some(origin) = headers.get(header::ORIGIN) {
        let allowed = origin
            .to_str()
            .is_ok_and(|o| settings.allowed_origins.iter().any(|a| a == o));
        if !allowed {
            crate::metrics::inc("messaging_realtime_origin_refused_total", &[]);
            return refuse(
                StatusCode::FORBIDDEN,
                ErrorCode::AuthenticationFailed,
                "ORIGIN_NOT_ALLOWED",
            );
        }
    }
    let offered: Vec<String> = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|p| p.trim().to_string()))
        .collect();
    if !offered.iter().any(|p| p == SUBPROTOCOL) {
        return refuse(
            StatusCode::BAD_REQUEST,
            ErrorCode::ValidationError,
            "SUBPROTOCOL_REQUIRED",
        );
    }
    let tickets: Vec<&str> = offered
        .iter()
        .filter_map(|p| p.strip_prefix(TICKET_PROTOCOL_PREFIX))
        .collect();
    let [ticket] = tickets.as_slice() else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            ErrorCode::AuthenticationFailed,
            "TICKET_REQUIRED",
        );
    };
    let upgrade = match upgrade {
        Ok(u) => u,
        Err(_) => {
            return refuse(
                StatusCode::UPGRADE_REQUIRED,
                ErrorCode::ValidationError,
                "WEBSOCKET_REQUIRED",
            )
        }
    };
    let session = match sessions::consume_ticket(&state, ticket).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !session.has_scope(MESSAGES_READ) {
        return ApiError::code(ErrorCode::InsufficientScope).into_response();
    }
    let admission = Admission {
        tenant_id: session.tenant_id,
        environment_id: session.environment_id,
        principal_id: session.principal_id,
        device_id: session.device_id,
        session_id: session.id,
    };
    if let Err(limit) = state.realtime.hub.admissible(&admission) {
        crate::metrics::inc(
            "messaging_realtime_admission_refused_total",
            &[("reason", limit.reason())],
        );
        return refuse(
            if limit == super::hub::LimitExceeded::Draining {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::TOO_MANY_REQUESTS
            },
            ErrorCode::RateLimited,
            limit.reason(),
        );
    }
    upgrade
        .protocols([SUBPROTOCOL])
        .max_message_size(settings.max_frame_bytes)
        .max_frame_size(settings.max_frame_bytes)
        .on_upgrade(move |socket| async move {
            match state.realtime.hub.register(admission) {
                Ok(handle) => Connection::serve(state, session, handle, socket).await,
                Err(limit) => {
                    let mut socket = socket;
                    let _ = socket
                        .send(axum::extract::ws::Message::Close(Some(
                            axum::extract::ws::CloseFrame {
                                code: 1013,
                                reason: limit.reason().into(),
                            },
                        )))
                        .await;
                }
            }
        })
}

async fn issue(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<IssueRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok((
        StatusCode::CREATED,
        Json(sessions::issue_token(&state, caller.context(), &acting, body).await?),
    ))
}

async fn revoke(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(session): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        sessions::revoke_session(&state, caller.context(), &acting, session).await?,
    ))
}

async fn get_presence(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        presence::get_settings(&state, caller.context(), &acting).await?,
    ))
}

async fn put_presence(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<SettingsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        presence::put_settings(&state, caller.context(), &acting, body).await?,
    ))
}

async fn register_push(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(device): Path<Uuid>,
    Json(body): Json<RegistrationRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        push::register(&state, caller.context(), &acting, device, body).await?,
    ))
}

async fn unregister_push(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(device): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        push::unregister(&state, caller.context(), &acting, device).await?,
    ))
}

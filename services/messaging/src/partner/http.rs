//! Partner device routes (build contract §4; security scheme `userToken`). They
//! authenticate themselves with `Authorization: Bearer cfut_…` (verified by Platform
//! Access, never by Messaging) and are merged into the public router, so a customer
//! credential or a client token is never accepted here.
//!
//! | Route | Grant | Flag |
//! |---|---|---|
//! | `POST /v1/messaging/user-sessions` | `msg` | `MESSAGING_PARTNER_SESSION_DEVICES_ENABLED` |
//! | `PUT /v1/messaging/user-push-registrations/{installationId}` | `msg` | `MESSAGING_PARTNER_PUSH_ENABLED` |
//! | `DELETE /v1/messaging/user-push-registrations/{installationId}?platform=` | `msg` | `MESSAGING_PARTNER_PUSH_ENABLED` |

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{post, put},
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize};

use super::{push, sessions};
use crate::{
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

/// Largest request body of these routes.
const MAX_BODY: usize = 16 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/messaging/user-sessions", post(exchange))
        .route(
            "/v1/messaging/user-push-registrations/:installation_id",
            put(register_push).delete(unregister_push),
        )
}

/// The presented user token: exactly one bounded `Authorization: Bearer cfut_…`.
pub fn user_token(headers: &HeaderMap) -> Result<zeroize::Zeroizing<String>, ApiError> {
    let refused = || ApiError::reason(ErrorCode::AuthenticationFailed, "USER_TOKEN_REQUIRED");
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next().ok_or_else(refused)?;
    if values.next().is_some() || value.len() > super::MAX_USER_TOKEN_BYTES + 16 {
        return Err(refused());
    }
    let (scheme, token) = value
        .to_str()
        .ok()
        .and_then(|v| v.split_once(' '))
        .ok_or_else(refused)?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || !token.starts_with("cfut_") {
        return Err(refused());
    }
    Ok(zeroize::Zeroizing::new(token.to_string()))
}

fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, ApiError> {
    if bytes.len() > MAX_BODY {
        return Err(ApiError::code(ErrorCode::PayloadTooLarge));
    }
    serde_json::from_slice(bytes).map_err(|_| ApiError::validation("invalid request body"))
}

async fn exchange(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    // With the flag off nothing is verified (Platform Access is never asked).
    if !state.partner.settings.session_devices_enabled {
        return Err(ApiError::reason(
            ErrorCode::FeatureDisabled,
            "PARTNER_SESSION_DEVICES_DISABLED",
        ));
    }
    let token = user_token(&headers)?;
    let request: sessions::ExchangeRequest = body(&bytes)?;
    let response = sessions::exchange(&state, &token, request).await?;
    let mut r = (StatusCode::CREATED, Json(response)).into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(r)
}

async fn register_push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(installation): Path<String>,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    if !state.partner.settings.push_enabled {
        return Err(ApiError::reason(
            ErrorCode::FeatureDisabled,
            "PARTNER_PUSH_DISABLED",
        ));
    }
    let token = user_token(&headers)?;
    let request: push::RegistrationRequest = body(&bytes)?;
    let response = push::register(&state, &token, &installation, request).await?;
    Ok(Json(response).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnregisterQuery {
    #[serde(default)]
    platform: Option<String>,
}

async fn unregister_push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(installation): Path<String>,
    query: Result<Query<UnregisterQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, ApiError> {
    if !state.partner.settings.push_enabled {
        return Err(ApiError::reason(
            ErrorCode::FeatureDisabled,
            "PARTNER_PUSH_DISABLED",
        ));
    }
    let token = user_token(&headers)?;
    let Query(query) = query.map_err(|_| ApiError::validation("invalid query"))?;
    push::unregister(&state, &token, &installation, query.platform.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

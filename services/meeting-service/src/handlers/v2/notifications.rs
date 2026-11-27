//! Notification Service user APIs U1–U9 (Document 4 §3.1) under `/api/v2/notifications`,
//! relayed like `/api/v2/communications/*`: the Account session is verified here (a
//! registered account whose token was checked against the revocation store), then the
//! request is forwarded to the Notification Service with the server-resolved account
//! ([`crate::services::notification_relay`]). Its status, body (`schemaVersion: 1`),
//! `Retry-After` and `ETag` are returned with `Cache-Control: no-store`; a matching
//! `If-None-Match` on the unread count answers 304.
//!
//! Errors produced here use the Meeting error envelope: 401 `AUTHENTICATION_REQUIRED`
//! (checked first), 400 `VALIDATION_ERROR` (malformed id, query, key or JSON) and 503
//! `SERVICE_UNAVAILABLE` (relay not configured — the default — or the Notification Service
//! unreachable or answering outside its contract). Everything else is the Notification
//! Service's own answer (`notifications/http/v1.openapi.json`).

use axum::{
    body::{Body, Bytes},
    extract::{Path, RawQuery, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{actor::ActorContext, notification_relay::Dispatch},
    state::AppState,
};

/// Request bodies (a preferences document is the largest; routes in `routes/v2.rs`).
pub const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_QUERY_BYTES: usize = 1024;

fn unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::ServiceUnavailable,
        "Notifications are temporarily unavailable",
    )
}

fn invalid(message: &str) -> ApiError {
    ApiError::domain(ErrorCode::ValidationError, message)
}

fn uuid_segment(raw: &str, name: &str) -> ApiResult<String> {
    Uuid::parse_str(raw)
        .map(|id| id.to_string())
        .map_err(|_| invalid(&format!("{name} must be a UUID")))
}

/// Verify the Account session, forward one U-operation, relay the answer.
async fn forward(
    state: AppState,
    actor: ActorContext,
    method: Method,
    path: String,
    query: Option<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<Response> {
    let user = actor.require_registered()?;
    // Same rule as the Communications relay: an unchecked session is never forwarded.
    if !user.revocation_checked {
        return Err(unavailable());
    }
    let relay = state.notifications.as_ref().ok_or_else(unavailable)?;
    let query = query.filter(|q| !q.is_empty());
    if query.as_ref().is_some_and(|q| q.len() > MAX_QUERY_BYTES) {
        return Err(invalid("The query string is too long"));
    }
    let idempotency_key = headers
        .get("idempotency-key")
        .map(|v| {
            v.to_str()
                .ok()
                .filter(|k| {
                    !k.is_empty() && k.len() <= 128 && k.bytes().all(|b| (0x21..=0x7e).contains(&b))
                })
                .map(str::to_owned)
                .ok_or_else(|| invalid("Idempotency-Key must be 1-128 visible ASCII characters"))
        })
        .transpose()?;
    let body = if bytes.is_empty() {
        None
    } else {
        Some(serde_json::from_slice::<Value>(&bytes).map_err(|_| invalid("Invalid JSON request"))?)
    };
    let envelope = Dispatch {
        method: method.as_str().to_owned(),
        path,
        query,
        account: format!("cfmeet-user:{}", user.user_id),
        idempotency_key,
        body,
    };
    let answer = relay.dispatch(&envelope).await.map_err(|_| unavailable())?;
    // 401 / 403 from the Notification Service are service-authentication outcomes
    // (trust bundle, key rotation), never the user's: not relayed.
    if matches!(answer.status, 401 | 403) {
        tracing::error!(
            status = answer.status,
            "Notification Service refused the Meeting relay"
        );
        return Err(unavailable());
    }
    let status = StatusCode::from_u16(answer.status).map_err(|_| unavailable())?;
    let mut response = if status == StatusCode::NO_CONTENT && answer.body.is_empty() {
        status.into_response()
    } else {
        let value: Value = serde_json::from_slice(&answer.body).map_err(|_| unavailable())?;
        if value["schemaVersion"] != 1 {
            return Err(unavailable());
        }
        let not_modified = method == Method::GET
            && status == StatusCode::OK
            && answer.etag.as_deref().is_some_and(|etag| {
                headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
            });
        if not_modified {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            let mut r = Response::new(Body::from(answer.body));
            *r.status_mut() = status;
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            r
        }
    };
    let h = response.headers_mut();
    if let Some(v) = answer
        .retry_after
        .and_then(|v| HeaderValue::from_str(&v).ok())
    {
        h.insert(header::RETRY_AFTER, v);
    }
    if let Some(v) = answer.etag.and_then(|v| HeaderValue::from_str(&v).ok()) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// List my notifications (U1).
///
/// The signed-in user's notification inbox, newest first, with an opaque cursor. Relayed
/// to the Notification Service with the server-resolved account. Items never contain
/// codes, tokens, addresses, push handles, E2EE text or URLs.
#[utoipa::path(
    get, path = "/api/v2/notifications", tag = "Notifications (v2)", operation_id = "v2_notifications_list",
    params(
        ("cursor" = Option<String>, Query, description = "Opaque cursor (nextCursor of the previous page)"),
        ("limit" = Option<i64>, Query, description = "1-100 (default 30)"),
        ("state" = Option<String>, Query, description = "unread or all (default all)"),
        ("category" = Option<String>, Query, description = "Comma-separated notification categories"),
    ),
    responses((status = 200, description = "{items, nextCursor, unreadCount} (Notification Service answer, schemaVersion 1)", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE (relay not configured or Notification Service unavailable)", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn list_my_notifications(
    State(state): State<AppState>,
    actor: ActorContext,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> ApiResult<Response> {
    forward(
        state,
        actor,
        Method::GET,
        String::new(),
        query,
        headers,
        Bytes::new(),
    )
    .await
}

/// My unread notification count (U2).
///
/// `{unreadCount, latestCreatedAt, version}` with an `ETag`; a matching `If-None-Match`
/// answers 304 without a body.
#[utoipa::path(
    get, path = "/api/v2/notifications/unread-count", tag = "Notifications (v2)", operation_id = "v2_notifications_unread_count",
    params(("If-None-Match" = Option<String>, Header, description = "ETag of a previous answer")),
    responses((status = 200, description = "{unreadCount, latestCreatedAt, version} with ETag", body = Object), (status = 304, description = "Not modified"), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn get_my_notification_unread_count(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
) -> ApiResult<Response> {
    forward(
        state,
        actor,
        Method::GET,
        "/unread-count".into(),
        None,
        headers,
        Bytes::new(),
    )
    .await
}

/// Update one of my notifications (U3).
///
/// `{read?, dismissed?, expectedVersion}`; only the caller's own items (anything else is
/// 404). Requires an `Idempotency-Key`.
#[utoipa::path(
    patch, path = "/api/v2/notifications/{notificationId}", tag = "Notifications (v2)", operation_id = "v2_notifications_update",
    params(
        ("notificationId" = String, Path, description = "Notification id"),
        ("Idempotency-Key" = String, Header, description = "Retry key (8-128 visible ASCII characters)"),
    ),
    request_body = Object,
    responses((status = 200, description = "The updated NotificationItem", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "NOT_FOUND", body = Object), (status = 409, description = "VERSION_CONFLICT", body = Object), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn update_my_notification(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(notification_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    actor.require_registered()?;
    let id = uuid_segment(&notification_id, "notificationId")?;
    forward(
        state,
        actor,
        Method::PATCH,
        format!("/{id}"),
        None,
        headers,
        body,
    )
    .await
}

/// Mark my notifications read (U4).
///
/// `{ids (at most 100) | upTo}` → `{updated, unreadCount}`. Requires an `Idempotency-Key`.
#[utoipa::path(
    post, path = "/api/v2/notifications/read", tag = "Notifications (v2)", operation_id = "v2_notifications_mark_read",
    params(("Idempotency-Key" = String, Header, description = "Retry key (8-128 visible ASCII characters)")),
    request_body = Object,
    responses((status = 200, description = "{updated, unreadCount}", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 409, description = "IDEMPOTENCY_CONFLICT", body = Object), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn mark_my_notifications_read(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    forward(
        state,
        actor,
        Method::POST,
        "/read".into(),
        None,
        headers,
        body,
    )
    .await
}

/// Dismiss my notifications (U5).
///
/// `{ids (at most 100) | upTo, category?}` → `{dismissed, unreadCount}`. Requires an
/// `Idempotency-Key`.
#[utoipa::path(
    post, path = "/api/v2/notifications/dismiss", tag = "Notifications (v2)", operation_id = "v2_notifications_dismiss",
    params(("Idempotency-Key" = String, Header, description = "Retry key (8-128 visible ASCII characters)")),
    request_body = Object,
    responses((status = 200, description = "{dismissed, unreadCount}", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 409, description = "IDEMPOTENCY_CONFLICT", body = Object), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn dismiss_my_notifications(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    forward(
        state,
        actor,
        Method::POST,
        "/dismiss".into(),
        None,
        headers,
        body,
    )
    .await
}

/// My notification preferences (U6).
///
/// The preference document with its `version` and the Account time zone (read-only).
#[utoipa::path(
    get, path = "/api/v2/notifications/preferences", tag = "Notifications (v2)", operation_id = "v2_notifications_preferences_get",
    responses((status = 200, description = "NotificationPreferences with version and timeZone", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn get_my_notification_preferences(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
) -> ApiResult<Response> {
    forward(
        state,
        actor,
        Method::GET,
        "/preferences".into(),
        None,
        headers,
        Bytes::new(),
    )
    .await
}

/// Update my notification preferences (U7).
///
/// A full or partial preference document with `expectedVersion` (optimistic concurrency).
/// Requires an `Idempotency-Key`.
#[utoipa::path(
    put, path = "/api/v2/notifications/preferences", tag = "Notifications (v2)", operation_id = "v2_notifications_preferences_update",
    params(("Idempotency-Key" = String, Header, description = "Retry key (8-128 visible ASCII characters)")),
    request_body = Object,
    responses((status = 200, description = "The updated NotificationPreferences", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 409, description = "VERSION_CONFLICT", body = Object), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn update_my_notification_preferences(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    forward(
        state,
        actor,
        Method::PUT,
        "/preferences".into(),
        None,
        headers,
        body,
    )
    .await
}

/// Register my push endpoint for an Account device (U8).
///
/// `{platform: APNS | APNS_VOIP | FCM | WEB_PUSH, handle}` for one of the caller's ACTIVE
/// Account devices → `{pushTargetRef, platform, enabled}`; the handle is never echoed.
/// Requires an `Idempotency-Key`.
#[utoipa::path(
    put, path = "/api/v2/notifications/devices/{accountDeviceId}/push", tag = "Notifications (v2)", operation_id = "v2_notifications_push_register",
    params(
        ("accountDeviceId" = String, Path, description = "Account device id (ADR 0060)"),
        ("Idempotency-Key" = String, Header, description = "Retry key (8-128 visible ASCII characters)"),
    ),
    request_body = Object,
    responses((status = 200, description = "{pushTargetRef, platform, enabled}", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "NOT_FOUND (not the caller's active device)", body = Object), (status = 409, description = "IDEMPOTENCY_CONFLICT", body = Object), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn register_my_push_endpoint(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(account_device_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    actor.require_registered()?;
    let id = uuid_segment(&account_device_id, "accountDeviceId")?;
    forward(
        state,
        actor,
        Method::PUT,
        format!("/devices/{id}/push"),
        None,
        headers,
        body,
    )
    .await
}

/// Remove my push endpoint for an Account device (U9).
///
/// Idempotent; `platform` limits the removal to one push platform.
#[utoipa::path(
    delete, path = "/api/v2/notifications/devices/{accountDeviceId}/push", tag = "Notifications (v2)", operation_id = "v2_notifications_push_remove",
    params(
        ("accountDeviceId" = String, Path, description = "Account device id (ADR 0060)"),
        ("platform" = Option<String>, Query, description = "APNS, APNS_VOIP, FCM or WEB_PUSH"),
    ),
    responses((status = 200, description = "Removed (Notification Service answer)", body = Object), (status = 204, description = "Removed"), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "NOT_FOUND", body = Object), (status = 429, description = "RATE_LIMITED (Retry-After)", body = Object), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn remove_my_push_endpoint(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(account_device_id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> ApiResult<Response> {
    actor.require_registered()?;
    let id = uuid_segment(&account_device_id, "accountDeviceId")?;
    forward(
        state,
        actor,
        Method::DELETE,
        format!("/devices/{id}/push"),
        query,
        headers,
        Bytes::new(),
    )
    .await
}

//! Final closure — organization-scope Messaging (J-10): the Messaging client API relayed in
//! the organization's OWN technical scope (`/api/v2/organizations/{id}/messaging/relay/{path}`)
//! and its MANAGED-realtime session tokens (`/api/v2/organizations/{id}/messaging/session-tokens`).
//!
//! Same allowlist and query rules as the personal relay (`/api/v2/messaging/relay/{path}`);
//! the difference is the scope: conversations, members and messages live in the
//! organization's FIRST_PARTY triple, so they never mix with the member's personal
//! conversations, and a member removed or suspended by the organization (or a suspended
//! organization) is refused here by Organizations and, independently, by Messaging once
//! the offboarding worker suspended the member's organization principal.

use axum::{
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;
use uuid::Uuid;

use super::unified_messaging::{account_session, parse_query, SessionTokenRequest};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{actor::ActorContext, organization_calling::MessagingRequest, organization_policy},
    state::AppState,
};

fn disabled() -> ApiError {
    ApiError::domain(
        ErrorCode::FeatureDisabled,
        "Organization Messaging is not enabled",
    )
}

fn organization_id(id: &str) -> ApiResult<Uuid> {
    // Malformed, unknown and unauthorized organizations get the same answer.
    Uuid::try_parse(id)
        .map_err(|_| ApiError::domain(ErrorCode::OrganizationNotFound, "Organization not found"))
}

fn respond(status: u16, value: Value) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = if value.is_null() {
        status.into_response()
    } else {
        (status, Json(value)).into_response()
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn relay_operation(
    state: AppState,
    actor: ActorContext,
    method: Method,
    (id, path): (String, String),
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    actor.require_registered()?;
    let core = &state.meeting.core;
    let (Some(relay), Some(authority)) = (
        core.organization_calling.as_ref(),
        organization_policy::authoritative(core),
    ) else {
        return Err(disabled());
    };
    let organization = organization_id(&id)?;
    let body = if body.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice::<Value>(&body)
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| {
                    ApiError::domain(ErrorCode::ValidationError, "The body must be a JSON object")
                })?,
        )
    };
    let idempotency_key = match headers.get("idempotency-key") {
        None => None,
        Some(v) => Some(
            v.to_str()
                .ok()
                .filter(|k| {
                    !k.is_empty() && k.len() <= 128 && k.bytes().all(|b| (0x21..=0x7e).contains(&b))
                })
                .map(str::to_string)
                .ok_or_else(|| {
                    ApiError::domain(ErrorCode::ValidationError, "Invalid Idempotency-Key")
                })?,
        ),
    };
    let (status, value) = relay
        .messaging_relay(
            &state.db,
            authority,
            &actor,
            MessagingRequest {
                organization,
                method,
                path: &path,
                query: parse_query(query)?,
                idempotency_key,
                body,
            },
        )
        .await?;
    Ok(respond(status, value))
}

/// Organization Messaging read (allowlisted path under `v1/`, e.g. `v1/conversations`).
///
/// Reads the caller's conversations, messages, members and receipts in the ORGANIZATION'S
/// own Messaging scope, as the caller's principal in that scope. The caller must be an
/// active member of an active organization whose policy allows Messaging
/// (`features.messaging`) and the organization must have its Messaging binding set up.
/// Same allowlist as `GET /api/v2/messaging/relay/{path}`; anything else is
/// ENDPOINT_NOT_FOUND and is not forwarded. Messaging's status and JSON body are returned.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/messaging/relay/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_messaging_get",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 409, description = "PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn messaging_get(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::GET, p, query, headers, Bytes::new()).await
}

/// Organization Messaging create / action (allowlisted path under `v1/`).
///
/// Creates an organization conversation, adds organization members, posts a message or runs
/// another allowlisted Messaging action in the organization's own scope, as the caller's
/// principal there. Requires active membership, an active organization and
/// `features.messaging`. The JSON object body (at most 1 MiB) and, where the operation
/// accepts one, an `Idempotency-Key` are forwarded. Messaging's status and body are returned.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/messaging/relay/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_messaging_post",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 409, description = "PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn messaging_post(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::POST, p, query, headers, body).await
}

/// Organization Messaging replace (allowlisted path under `v1/`, e.g. a read cursor).
#[utoipa::path(
    put, path = "/api/v2/organizations/{id}/messaging/relay/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_messaging_put",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 409, description = "PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn messaging_put(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::PUT, p, query, headers, body).await
}

/// Organization Messaging update (allowlisted path under `v1/`, e.g. edit a message).
#[utoipa::path(
    patch, path = "/api/v2/organizations/{id}/messaging/relay/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_messaging_patch",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 409, description = "PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn messaging_patch(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::PATCH, p, query, headers, body).await
}

/// Organization Messaging delete (allowlisted path under `v1/`, e.g. remove a member).
#[utoipa::path(
    delete, path = "/api/v2/organizations/{id}/messaging/relay/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_messaging_delete",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 409, description = "PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn messaging_delete(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> ApiResult<Response> {
    relay_operation(
        state,
        actor,
        Method::DELETE,
        p,
        query,
        headers,
        Bytes::new(),
    )
    .await
}

/// Issue an organization-scope MANAGED-realtime session-device token.
///
/// Like `POST /api/v2/messaging/session-tokens`, but in the organization's own Messaging
/// scope: the token opens the realtime gateway for the caller's organization
/// conversations only. It is bound to the caller's verified account session (signing out
/// ends it) and requires active membership, an active organization and
/// `features.messaging`. Once the organization removes or suspends the member, renewals
/// are refused and the open connection is closed when Messaging revalidates it.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/messaging/session-tokens", tag = "Organizations (v2)", operation_id = "v2_organization_messaging_session_token",
    params(("id" = String, Path, description = "Organization id")),
    request_body = Object,
    responses((status = 201, description = "Session-device client token (Messaging contract)", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn messaging_session_token(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    actor.require_registered()?;
    let core = &state.meeting.core;
    let (Some(relay), Some(authority)) = (
        core.organization_calling.as_ref(),
        organization_policy::authoritative(core),
    ) else {
        return Err(disabled());
    };
    let organization = organization_id(&id)?;
    let request: SessionTokenRequest = if body.is_empty() {
        SessionTokenRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| {
            ApiError::domain(
                ErrorCode::ValidationError,
                "The body may name only sessionDeviceId and ttlSeconds",
            )
        })?
    };
    let (session, expires) = account_session(&state, &headers)?;
    let not_after = chrono::DateTime::from_timestamp(expires, 0).ok_or_else(|| {
        ApiError::domain(
            ErrorCode::AuthenticationRequired,
            "Invalid or expired token",
        )
    })?;
    let (status, value) = relay
        .messaging_session_token(
            &state.db,
            authority,
            &actor,
            organization,
            session,
            not_after,
            request.session_device_id,
            request.ttl_seconds,
        )
        .await?;
    Ok(respond(status, value))
}

//! Blueprint Section 12 follow-up — per-organization Enterprise Calling relay
//! (`/api/v2/organizations/{id}/calling/{path}`; see
//! `services::organization_calling` and `docs/operations/org-policy-calling-scope-section-12.md`).

use axum::{
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        organization_calling::{self, CallingRequest},
        organization_policy,
    },
    state::AppState,
};

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
        return Err(organization_calling::disabled());
    };
    // Malformed, unknown and unauthorized organizations get the same answer.
    let organization = Uuid::try_parse(&id)
        .map_err(|_| ApiError::domain(ErrorCode::OrganizationNotFound, "Organization not found"))?;
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
        .relay(
            &state.db,
            authority,
            &actor,
            CallingRequest {
                organization,
                method,
                path: &path,
                query: organization_calling::parse_query(query.as_deref())?,
                idempotency_key,
                body,
            },
        )
        .await?;
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = if value.is_null() {
        status.into_response()
    } else {
        (status, Json(value)).into_response()
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Organization Enterprise Calling read (allowlisted path, e.g. `v1/enterprise/extensions`).
///
/// Reads the organization's own Enterprise Calling configuration (extensions, ring groups,
/// queues, IVR flows, business hours, policies, voicemail, recordings, audit, webhooks,
/// usage and limits) in the organization's provisioned Calling scope — never the shared
/// CF Meet scope. The caller must hold `calling.enterprise.manage` in the organization and
/// the organization must not have turned Calling off (the decision
/// `GET /api/v2/me/workspaces/{id}/capabilities` reports). The path must match the
/// allowlist exactly with lowercase canonical UUIDs; anything else is ENDPOINT_NOT_FOUND
/// and is not forwarded. Calling's status and JSON body are returned. Disabled by default
/// (`409 ORGANIZATION_CALLING_DISABLED`).
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/calling/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_calling_get",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Calling Enterprise administration path under v1/")),
    responses((status = 200, description = "Calling contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn calling_get(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::GET, p, query, headers, Bytes::new()).await
}

/// Organization Enterprise Calling create / action (allowlisted path).
///
/// Creates an Enterprise Calling resource (extension, queue, IVR version, webhook…) or runs
/// an action (recording media access, business-hours evaluation, delivery replay) in the
/// organization's provisioned Calling scope, acting for the caller: Calling's enterprise
/// audit records the caller's own principal. Requires `calling.enterprise.manage` in the
/// organization and Calling not turned off by its policy. The JSON object body (at most
/// 1 MiB) and an `Idempotency-Key` are forwarded. Calling's status and JSON body are
/// returned. Disabled by default (`409 ORGANIZATION_CALLING_DISABLED`).
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/calling/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_calling_post",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Calling Enterprise administration path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Calling contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn calling_post(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::POST, p, query, headers, body).await
}

/// Organization Enterprise Calling replace (allowlisted path, e.g. members or schedules).
///
/// Replaces a set in the organization's Enterprise Calling configuration (department,
/// ring-group or queue members, business-hours schedule, IVR activation, voicemail read
/// state) in the organization's provisioned Calling scope, acting for the caller. Requires
/// `calling.enterprise.manage` and Calling not turned off by the organization. The JSON
/// object body (at most 1 MiB) is forwarded; Calling's status and JSON body are returned.
/// Disabled by default (`409 ORGANIZATION_CALLING_DISABLED`).
#[utoipa::path(
    put, path = "/api/v2/organizations/{id}/calling/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_calling_put",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Calling Enterprise administration path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Calling contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn calling_put(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::PUT, p, query, headers, body).await
}

/// Organization Enterprise Calling partial update (allowlisted path).
///
/// Updates an Enterprise Calling resource (policy, department, extension, ring group,
/// queue, IVR flow, forwarding rule, mailbox, recording policy, webhook) in the
/// organization's provisioned Calling scope, acting for the caller; Calling enforces
/// optimistic concurrency. Requires `calling.enterprise.manage` and Calling not turned off
/// by the organization. Calling's status and JSON body are returned. Disabled by default
/// (`409 ORGANIZATION_CALLING_DISABLED`).
#[utoipa::path(
    patch, path = "/api/v2/organizations/{id}/calling/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_calling_patch",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Calling Enterprise administration path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Calling contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn calling_patch(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(p): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay_operation(state, actor, Method::PATCH, p, query, headers, body).await
}

/// Organization Enterprise Calling removal (allowlisted path).
///
/// Disables or deletes an Enterprise Calling resource (a recording or voicemail message, a
/// forwarding rule, a webhook…) in the organization's provisioned Calling scope, acting for
/// the caller; Calling's enterprise audit records the caller's own principal. Requires
/// `calling.enterprise.manage` and Calling not turned off by the organization. No body is
/// sent. Calling's status and JSON body are returned. Disabled by default
/// (`409 ORGANIZATION_CALLING_DISABLED`).
#[utoipa::path(
    delete, path = "/api/v2/organizations/{id}/calling/{path}", tag = "Organizations (v2)", operation_id = "v2_organization_calling_delete",
    params(("id" = String, Path, description = "Organization id"), ("path" = String, Path, description = "An allowlisted Calling Enterprise administration path under v1/")),
    responses((status = 200, description = "Calling contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn calling_delete(
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

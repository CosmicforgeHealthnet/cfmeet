//! Gap audit G3 — an organization MEMBER's own calling presence in the organization's OWN
//! Calling scope (`/api/v2/organizations/{id}/member-calling/...`): register and list
//! their endpoints, revoke one, and obtain a signaling credential for one, acting as the
//! member's organization principal (see
//! `services::organization_calling::OrganizationCalling::member_calling`).
//!
//! Same gate as the member Messaging relay (`/api/v2/organizations/{id}/messaging/relay/...`):
//! an active member of an active organization whose policy allows Calling — no
//! administrative capability. Requires `ORGANIZATION_CALLING_SCOPE=organization`
//! (otherwise `409 ORGANIZATION_CALLING_DISABLED`, like the administration relay).

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        organization_calling::{self, MemberCallingOperation},
        organization_policy,
    },
    state::AppState,
};

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

/// Authentication and the rollout switch, before anything about the request is read.
fn ready(state: &AppState, actor: &ActorContext) -> ApiResult<()> {
    actor.require_registered()?;
    let core = &state.meeting.core;
    if core.organization_calling.is_none() || organization_policy::authoritative(core).is_none() {
        return Err(organization_calling::disabled());
    }
    Ok(())
}

async fn operation(
    state: AppState,
    actor: ActorContext,
    id: &str,
    operation: MemberCallingOperation,
) -> ApiResult<Response> {
    actor.require_registered()?;
    let core = &state.meeting.core;
    let (Some(relay), Some(authority)) = (
        core.organization_calling.as_ref(),
        organization_policy::authoritative(core),
    ) else {
        return Err(organization_calling::disabled());
    };
    let organization = organization_id(id)?;
    let (status, value) = relay
        .member_calling(&state.db, authority, &actor, organization, operation)
        .await?;
    Ok(respond(status, value))
}

/// The JSON object body (at most 8 KiB).
fn object(body: &Bytes) -> ApiResult<Value> {
    if body.len() > 8 * 1024 {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "The body is too large",
        ));
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| {
            ApiError::domain(ErrorCode::ValidationError, "The body must be a JSON object")
        })
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointListQuery {
    #[serde(default)]
    pub include_revoked: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignalingCredentialRequest {
    pub endpoint_id: Uuid,
}

/// List my calling endpoints in the organization's own Calling scope.
///
/// Gap audit G3: the caller's own endpoints (`includeRevoked=true` adds revoked ones) as
/// their organization principal. Requires active membership of an active organization
/// whose policy allows Calling (`features.calling`; no administrative capability) and the
/// organization's Calling binding. Calling's `EndpointList` is returned.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/member-calling/endpoints", tag = "Organizations (v2)", operation_id = "v2_organization_member_calling_endpoints_list",
    params(("id" = String, Path, description = "Organization id"), ("includeRevoked" = Option<bool>, Query, description = "Also list revoked endpoints")),
    responses((status = 200, description = "EndpointList (Calling contract)", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn endpoints_list(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(id): Path<String>,
    query: Result<Query<EndpointListQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Response> {
    ready(&state, &actor)?;
    let Query(query) =
        query.map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query"))?;
    operation(
        state,
        actor,
        &id,
        MemberCallingOperation::ListEndpoints {
            include_revoked: query.include_revoked.unwrap_or(false),
        },
    )
    .await
}

/// Register (or refresh) my calling endpoint in the organization's own Calling scope.
///
/// Gap audit G3: registers one device of the caller (`endpointKey`, `endpointKind`,
/// `capabilities {audio, video}`, optional `pushTargetRef`; never `principalId`: the
/// caller's organization principal is always the one registered for). The same key again
/// refreshes the endpoint (200). Requires active membership of an active organization
/// whose policy allows Calling and the organization's Calling binding. Calling's
/// `Endpoint` is returned.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/member-calling/endpoints", tag = "Organizations (v2)", operation_id = "v2_organization_member_calling_endpoints_register",
    params(("id" = String, Path, description = "Organization id")),
    request_body = Object,
    responses((status = 200, description = "Endpoint refreshed (Calling contract)", body = Object), (status = 201, description = "Endpoint registered (Calling contract)", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED / endpoint key held by another principal", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn endpoints_register(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    ready(&state, &actor)?;
    let body = object(&body)?;
    operation(
        state,
        actor,
        &id,
        MemberCallingOperation::RegisterEndpoint(body),
    )
    .await
}

/// Revoke my calling endpoint in the organization's own Calling scope.
///
/// Gap audit G3: revokes one of the caller's own endpoints (idempotent); its live call
/// offers end and its signaling credentials stop working. Another member's or an unknown
/// endpoint is NOT_FOUND. Requires active membership of an active organization whose
/// policy allows Calling. Calling's `Endpoint` is returned.
#[utoipa::path(
    delete, path = "/api/v2/organizations/{id}/member-calling/endpoints/{endpointId}", tag = "Organizations (v2)", operation_id = "v2_organization_member_calling_endpoints_revoke",
    params(("id" = String, Path, description = "Organization id"), ("endpointId" = Uuid, Path, description = "One of the caller's endpoints")),
    responses((status = 200, description = "Endpoint (Calling contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED / NOT_FOUND", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn endpoints_revoke(
    State(state): State<AppState>,
    actor: ActorContext,
    Path((id, endpoint)): Path<(String, String)>,
) -> ApiResult<Response> {
    ready(&state, &actor)?;
    let endpoint = Uuid::try_parse(&endpoint)
        .map_err(|_| ApiError::domain(ErrorCode::EndpointNotFound, "Endpoint not found"))?;
    operation(
        state,
        actor,
        &id,
        MemberCallingOperation::RevokeEndpoint(endpoint),
    )
    .await
}

/// Obtain a signaling credential for my calling endpoint in the organization's scope.
///
/// Gap audit G3: a short-lived signaling credential (`{endpointId}`: one of the caller's
/// ACTIVE endpoints) for Calling's realtime signaling channel (`signalingUrl`) in the
/// organization's own scope: incoming organization calls ring that device, which accepts
/// or declines them and obtains media credentials with it. It stops working when the
/// endpoint is revoked or the organization removes or suspends the member. Requires active
/// membership of an active organization whose policy allows Calling. Calling's
/// `SignalingCredential` is returned (a secret: deliver it to that device only).
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/member-calling/signaling-credentials", tag = "Organizations (v2)", operation_id = "v2_organization_member_calling_signaling_credential",
    params(("id" = String, Path, description = "Organization id")),
    request_body = Object,
    responses((status = 201, description = "SignalingCredential (Calling contract)", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "ORGANIZATION_POLICY_RESTRICTED / MEMBERSHIP_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED / NOT_FOUND", body = ErrorEnvelope), (status = 409, description = "ORGANIZATION_CALLING_DISABLED / PROVISIONING_PENDING / ORGANIZATION_SUSPENDED", body = ErrorEnvelope), (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn signaling_credential(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    ready(&state, &actor)?;
    let request: SignalingCredentialRequest = serde_json::from_slice(&body).map_err(|_| {
        ApiError::domain(
            ErrorCode::ValidationError,
            "The body must be {\"endpointId\": \"<uuid>\"}",
        )
    })?;
    operation(
        state,
        actor,
        &id,
        MemberCallingOperation::SignalingCredential(request.endpoint_id),
    )
    .await
}

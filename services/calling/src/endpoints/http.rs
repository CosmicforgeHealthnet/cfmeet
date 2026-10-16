//! Endpoint registry and signaling-credential routes (Part C §7, §8). Backend operations:
//! platform credential authentication, scope from the authenticated context only, the
//! principal resolved inside that scope.
//!
//! * `POST   /v1/calling/endpoints` — register (or idempotently refresh) an endpoint;
//! * `GET    /v1/calling/endpoints?principalId=…` — a principal's endpoints;
//! * `DELETE /v1/calling/endpoints/{endpoint_id}` — revoke (its live offers are cancelled);
//! * `POST   /v1/calling/signaling-credentials` — mint a short-lived signaling credential
//!   for one ACTIVE endpoint of one principal (handed to the device, which never sees the
//!   API credential).

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::Response,
    routing::{delete, post},
    Json, Router,
};
use chrono::SecondsFormat;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    CallEndpoint, CallEndpointId, EndpointCapabilities, EndpointKey, EndpointKind, NewEndpoint,
};
use crate::{
    calls::{
        app::http::{parse_body, respond, uuid_path},
        command::CallScope,
    },
    domain::PrincipalId,
    error::CallingError,
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, PrincipalResolutionError},
        scopes::names,
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calling/endpoints", post(register).get(list))
        .route("/v1/calling/endpoints/:endpoint_id", delete(revoke))
        .route(
            "/v1/calling/signaling-credentials",
            post(mint_signaling_credential),
        )
        .layer(DefaultBodyLimit::max(8 * 1024))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EndpointView {
    endpoint_id: Uuid,
    principal_id: Uuid,
    endpoint_key: String,
    endpoint_kind: &'static str,
    capabilities: EndpointCapabilities,
    status: &'static str,
    presence: &'static str,
    last_seen_at: Option<String>,
    push_target_ref: Option<Uuid>,
    created_at: String,
    revoked_at: Option<String>,
}

fn ts(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

impl From<&CallEndpoint> for EndpointView {
    fn from(e: &CallEndpoint) -> Self {
        Self {
            endpoint_id: e.id.as_uuid(),
            principal_id: e.principal_id.as_uuid(),
            endpoint_key: e.endpoint_key.clone(),
            endpoint_kind: e.kind.as_str(),
            capabilities: e.capabilities,
            status: e.status.as_str(),
            presence: e.presence.as_str(),
            last_seen_at: e.last_seen_at.map(ts),
            push_target_ref: e.push_target_ref,
            created_at: ts(e.created_at),
            revoked_at: e.revoked_at.map(ts),
        }
    }
}

fn principal_error(err: PrincipalResolutionError) -> CallingError {
    match err {
        PrincipalResolutionError::Unavailable { .. } => CallingError::ServiceUnavailable,
        PrincipalResolutionError::InvalidReference => {
            CallingError::Validation("principalId is invalid.")
        }
        // Inactive principals cannot register endpoints; nothing is revealed.
        PrincipalResolutionError::NotFound | PrincipalResolutionError::Inactive => {
            CallingError::NotFound
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterBody {
    principal_id: Uuid,
    endpoint_key: String,
    endpoint_kind: EndpointKind,
    capabilities: EndpointCapabilitiesBody,
    #[serde(default)]
    push_target_ref: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointCapabilitiesBody {
    audio: bool,
    video: bool,
}

async fn register(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<RegisterBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let body = parse_body(body)?;
    let key = EndpointKey::parse(&body.endpoint_key)
        .ok_or(CallingError::Validation("endpointKey is invalid."))?;
    let principal = state
        .principals
        .resolve(
            &ctx,
            &PrincipalReference::Id(PrincipalId::from_uuid(body.principal_id)),
        )
        .await
        .map_err(principal_error)?;
    let (endpoint, created) = state
        .endpoints
        .register(
            &ctx,
            &principal,
            NewEndpoint {
                key,
                kind: body.endpoint_kind,
                capabilities: EndpointCapabilities {
                    audio: body.capabilities.audio,
                    video: body.capabilities.video,
                },
                push_target_ref: body.push_target_ref,
            },
        )
        .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(respond(status, EndpointView::from(&endpoint), false))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListQuery {
    principal_id: Uuid,
    #[serde(default)]
    include_revoked: Option<bool>,
}

#[derive(Debug, Serialize)]
struct EndpointList {
    data: Vec<EndpointView>,
}

async fn list(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<ListQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_READ)?;
    let Query(query) =
        query.map_err(|_| CallingError::Validation("The query string is invalid."))?;
    let endpoints = state
        .endpoints
        .list(
            CallScope::of_platform(&ctx),
            PrincipalId::from_uuid(query.principal_id),
            query.include_revoked.unwrap_or(false),
        )
        .await?;
    Ok(respond(
        StatusCode::OK,
        EndpointList {
            data: endpoints.iter().map(EndpointView::from).collect(),
        },
        false,
    ))
}

async fn revoke(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(endpoint_id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let endpoint_id = CallEndpointId::from_uuid(uuid_path(&endpoint_id)?);
    let (endpoint, newly) = state
        .endpoints
        .revoke(CallScope::of_platform(&ctx), endpoint_id)
        .await?;
    if newly {
        // A revoked endpoint is never rung again: its live offers end now.
        state.app.cancel_endpoint_invitations(endpoint_id).await?;
    }
    Ok(respond(
        StatusCode::OK,
        EndpointView::from(&endpoint),
        false,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignalingCredentialBody {
    principal_id: Uuid,
    endpoint_id: Uuid,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignalingCredentialView {
    endpoint_id: Uuid,
    principal_id: Uuid,
    token: String,
    expires_at: String,
    signaling_url: &'static str,
}

async fn mint_signaling_credential(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<SignalingCredentialBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let body = parse_body(body)?;
    let scope = CallScope::of_platform(&ctx);
    let principal = state
        .principals
        .resolve(
            &ctx,
            &PrincipalReference::Id(PrincipalId::from_uuid(body.principal_id)),
        )
        .await
        .map_err(principal_error)?;
    let endpoint = state
        .endpoints
        .active_endpoint(
            scope,
            principal.id(),
            CallEndpointId::from_uuid(body.endpoint_id),
        )
        .await?;
    let minted = state
        .signaling
        .keys()
        .mint(
            scope,
            principal.id(),
            endpoint.id,
            ctx.service_account_id(),
            ctx.credential_id(),
            state.clock.now(),
        )
        .map_err(|_| CallingError::Internal)?;
    tracing::info!(endpoint_id = %endpoint.id, "signaling credential minted");
    Ok(respond(
        StatusCode::CREATED,
        SignalingCredentialView {
            endpoint_id: endpoint.id.as_uuid(),
            principal_id: principal.id().as_uuid(),
            token: minted.token.expose_secret().to_string(),
            expires_at: minted.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            signaling_url: "/v1/calling/signaling",
        },
        false,
    ))
}

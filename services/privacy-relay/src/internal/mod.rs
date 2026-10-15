//! Internal HTTP API (`/internal/v1/…`, contract `cfmeet.privacy-relay.internal-api`).
//!
//! Every route requires an authenticated internal caller (service token), the receiver-
//! granted permission it names (tokens never self-grant), and — for consumer operations —
//! the signed delegated platform context, which is the ONLY source of tenant /
//! application / environment scope. Bodies refuse unknown members, so a `tenantId`,
//! `applicationId`, `environmentId`, status, version or authorization result in a body is
//! a 400. A foreign or unknown id is the same 404.
//!
//! Commands carry `{ "idempotencyKey": "…", "request": { … } }`; a replay answers the
//! recorded response with `Idempotent-Replayed: true`.

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    commands::IdempotencyKey,
    context::service::{
        ContextQuery, CreateContextRequest, LifecycleRequest, UpdateContextRequest,
    },
    error::{ErrorCode, RelayError},
    middleware::InternalCaller,
    relay::{
        authorization::AuthorizeRequest,
        service::{CreateSessionRequest, SessionQuery},
    },
    scope::{CommandContext, RelayScope},
    service::{CommandOutcome, RelayService},
    state::AppState,
};

pub const IDEMPOTENT_REPLAYED_HEADER: &str = "idempotent-replayed";
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Internal routes (mounted under `/internal/v1` behind service authentication).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/contexts", post(create_context))
        .route("/contexts/query", post(list_contexts))
        .route("/contexts/:context_id", get(get_context))
        .route("/contexts/:context_id/evidence", get(context_evidence))
        .route("/contexts/:context_id/update", post(update_context))
        .route("/contexts/:context_id/suspend", post(suspend_context))
        .route("/contexts/:context_id/resume", post(resume_context))
        .route("/contexts/:context_id/complete", post(complete_context))
        .route("/contexts/:context_id/revoke", post(revoke_context))
        .route("/contexts/:context_id/sessions", post(create_session))
        .route("/sessions/query", post(list_sessions))
        .route("/sessions/:session_id", get(get_session))
        .route("/sessions/:session_id/suspend", post(suspend_session))
        .route("/sessions/:session_id/resume", post(resume_session))
        .route("/sessions/:session_id/revoke", post(revoke_session))
        .route(
            "/sessions/:session_id/relay-number-assignment",
            post(assign_relay_number).get(get_relay_assignment),
        )
        .route(
            "/sessions/:session_id/relay-number-assignment/release",
            post(release_relay_assignment),
        )
        .route("/relay-routing/resolve-inbound", post(resolve_inbound))
        .route(
            "/relay-routing/protect-inbound-caller",
            post(protect_inbound_caller),
        )
        .route("/communication/authorize", post(authorize))
        .route(
            "/contacts/:contact_reference/resolution-grants",
            post(grant_for_execution),
        )
        .route(
            "/contacts/:contact_reference/resolve-for-execution",
            post(resolve_for_execution),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// Command envelope.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommandBody<T> {
    idempotency_key: String,
    request: T,
}

fn body<T: DeserializeOwned>(body: Result<Json<T>, JsonRejection>) -> Result<T, RelayError> {
    body.map(|Json(b)| b)
        .map_err(|_| RelayError::Validation("The request body is invalid."))
}

fn context_id(raw: &str) -> Result<Uuid, RelayError> {
    Uuid::parse_str(raw).map_err(|_| RelayError::Refused(ErrorCode::ContextNotFound))
}

fn session_id(raw: &str) -> Result<Uuid, RelayError> {
    Uuid::parse_str(raw).map_err(|_| RelayError::Refused(ErrorCode::SessionNotFound))
}

fn respond(status: StatusCode, body: Value, replayed: bool) -> Response {
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if replayed {
        headers.insert(IDEMPOTENT_REPLAYED_HEADER, HeaderValue::from_static("true"));
    }
    response
}

fn outcome(o: CommandOutcome) -> Response {
    respond(
        StatusCode::from_u16(o.status).unwrap_or(StatusCode::OK),
        o.body,
        o.replayed,
    )
}

/// Permission + delegated scope of a consumer operation.
fn authorize_caller(
    caller: &InternalCaller,
    permission: InternalPermission,
) -> Result<CommandContext, RelayError> {
    caller.require_permission(permission)?;
    CommandContext::from_caller(caller)
}

fn reader(
    caller: &InternalCaller,
    permission: InternalPermission,
) -> Result<RelayScope, RelayError> {
    Ok(authorize_caller(caller, permission)?.scope())
}

fn relay(state: &AppState) -> &RelayService {
    &state.relay
}

async fn create_context(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<CommandBody<CreateContextRequest>>, JsonRejection>,
) -> Result<Response, RelayError> {
    let cmd = authorize_caller(&caller, InternalPermission::CommunicationContextWrite)?;
    let b = body(raw)?;
    let key = IdempotencyKey::parse(&b.idempotency_key)?;
    Ok(outcome(
        relay(&state).create_context(&cmd, key, b.request).await?,
    ))
}

async fn list_contexts(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<ContextQuery>, JsonRejection>,
) -> Result<Response, RelayError> {
    let scope = reader(&caller, InternalPermission::CommunicationContextRead)?;
    let query = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state).list_contexts(scope, query).await?,
        false,
    ))
}

async fn get_context(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, RelayError> {
    let scope = reader(&caller, InternalPermission::CommunicationContextRead)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state).get_context(scope, context_id(&id)?).await?,
        false,
    ))
}

async fn context_evidence(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, RelayError> {
    let scope = reader(&caller, InternalPermission::CommunicationContextRead)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state)
            .context_evidence(scope, context_id(&id)?)
            .await?,
        false,
    ))
}

async fn update_context(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<CommandBody<UpdateContextRequest>>, JsonRejection>,
) -> Result<Response, RelayError> {
    let cmd = authorize_caller(&caller, InternalPermission::CommunicationContextWrite)?;
    let id = context_id(&id)?;
    let b = body(raw)?;
    let key = IdempotencyKey::parse(&b.idempotency_key)?;
    Ok(outcome(
        relay(&state)
            .update_context(&cmd, key, id, b.request)
            .await?,
    ))
}

macro_rules! lifecycle_handler {
    ($name:ident, $method:ident, $permission:expr, $id:ident) => {
        async fn $name(
            State(state): State<AppState>,
            caller: InternalCaller,
            Path(id): Path<String>,
            raw: Result<Json<CommandBody<LifecycleRequest>>, JsonRejection>,
        ) -> Result<Response, RelayError> {
            let cmd = authorize_caller(&caller, $permission)?;
            let id = $id(&id)?;
            let b = body(raw)?;
            let key = IdempotencyKey::parse(&b.idempotency_key)?;
            Ok(outcome(
                relay(&state).$method(&cmd, key, id, b.request).await?,
            ))
        }
    };
}

lifecycle_handler!(
    suspend_context,
    suspend_context,
    InternalPermission::CommunicationContextWrite,
    context_id
);
lifecycle_handler!(
    resume_context,
    resume_context,
    InternalPermission::CommunicationContextWrite,
    context_id
);
lifecycle_handler!(
    complete_context,
    complete_context,
    InternalPermission::CommunicationContextWrite,
    context_id
);
lifecycle_handler!(
    revoke_context,
    revoke_context,
    InternalPermission::CommunicationContextWrite,
    context_id
);
lifecycle_handler!(
    suspend_session,
    suspend_session,
    InternalPermission::RelaySessionWrite,
    session_id
);
lifecycle_handler!(
    resume_session,
    resume_session,
    InternalPermission::RelaySessionWrite,
    session_id
);
lifecycle_handler!(
    revoke_session,
    revoke_session,
    InternalPermission::RelaySessionWrite,
    session_id
);

async fn create_session(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<CommandBody<CreateSessionRequest>>, JsonRejection>,
) -> Result<Response, RelayError> {
    let cmd = authorize_caller(&caller, InternalPermission::RelaySessionWrite)?;
    let id = context_id(&id)?;
    let b = body(raw)?;
    let key = IdempotencyKey::parse(&b.idempotency_key)?;
    Ok(outcome(
        relay(&state)
            .create_session(&cmd, key, id, b.request)
            .await?,
    ))
}

async fn list_sessions(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<SessionQuery>, JsonRejection>,
) -> Result<Response, RelayError> {
    let scope = reader(&caller, InternalPermission::RelaySessionRead)?;
    let query = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state).list_sessions(scope, query).await?,
        false,
    ))
}

async fn get_session(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, RelayError> {
    let scope = reader(&caller, InternalPermission::RelaySessionRead)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state).get_session(scope, session_id(&id)?).await?,
        false,
    ))
}

async fn authorize(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<AuthorizeRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let cmd = authorize_caller(&caller, InternalPermission::CommunicationAuthorize)?;
    let request = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state).authorize(&cmd, request).await?,
        false,
    ))
}

/// The calling service acting in its signed delegated scope, as a Contact Vault actor.
fn contact_actor(
    caller: &InternalCaller,
    permission: InternalPermission,
) -> Result<crate::contact_vault::ContactActor, RelayError> {
    let cmd = authorize_caller(caller, permission)?;
    Ok(crate::contact_vault::ContactActor::service(
        cmd.scope(),
        cmd.actor_service().clone(),
        Some(cmd.service_account_id()),
        cmd.request_id().clone(),
        cmd.correlation_id().clone(),
    ))
}

fn contact_id(raw: &str) -> Result<Uuid, RelayError> {
    Uuid::parse_str(raw).map_err(|_| RelayError::Refused(ErrorCode::ContactNotFound))
}

async fn grant_for_execution(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<crate::contact_vault::resolution::GrantRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let actor = contact_actor(
        &caller,
        InternalPermission::ProtectedContactGrantForExecution,
    )?;
    let id = contact_id(&id)?;
    let request = body(raw)?;
    Ok(respond(
        StatusCode::CREATED,
        state
            .contacts
            .grant_for_execution(&actor, id, request)
            .await?,
        false,
    ))
}

/// Telephony only. The response carries the canonical number and is never logged.
async fn resolve_for_execution(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<crate::contact_vault::resolution::ResolveRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let actor = contact_actor(
        &caller,
        InternalPermission::ProtectedContactResolveForExecution,
    )?;
    let id = contact_id(&id)?;
    let request = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        state
            .contacts
            .resolve_for_execution(&actor, id, request)
            .await?,
        false,
    ))
}

/// Calling: lend a temporary relay number to a session (idempotent per session). The
/// response carries the assignment id (the session's relay identity binding) — never the
/// number or its provider binding.
async fn assign_relay_number(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<crate::relay_numbers::assignment::AssignRelayNumberRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let cmd = authorize_caller(&caller, InternalPermission::RelayNumberAssign)?;
    let id = session_id(&id)?;
    let request = body(raw)?;
    let (status, body) = relay(&state)
        .assign_relay_number(&cmd, id, request)
        .await
        .inspect_err(|e| {
            crate::metrics::inc(
                "privacy_relay_relay_number_assignment_refusals_total",
                &[("reason", e.code().as_str())],
            )
        })?;
    Ok(respond(
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        body,
        status == 200,
    ))
}

async fn get_relay_assignment(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, RelayError> {
    let cmd = authorize_caller(&caller, InternalPermission::RelayNumberAssign)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state)
            .get_relay_assignment(&cmd, session_id(&id)?)
            .await?,
        false,
    ))
}

/// Restrictive: works whatever the kill switches say.
async fn release_relay_assignment(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, RelayError> {
    let cmd = authorize_caller(&caller, InternalPermission::RelayNumberAssign)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state)
            .release_relay_assignment(&cmd, session_id(&id)?)
            .await?,
        false,
    ))
}

/// Telephony's inbound operations: the permission, and NO delegated context (the relay
/// mapping, not the caller, fixes the tenant / application / environment).
fn inbound_requester(
    caller: &InternalCaller,
    permission: InternalPermission,
) -> Result<crate::relay_numbers::inbound::InboundRequester, RelayError> {
    caller.require_permission(permission)?;
    if caller.context().delegated_platform_context().is_some() {
        return Err(RelayError::Validation(
            "Inbound relay operations take no delegated context; the relay mapping is the scope.",
        ));
    }
    Ok(crate::relay_numbers::inbound::InboundRequester {
        service: caller.context().authenticated_service().clone(),
        request_id: caller.request_id().clone(),
        correlation_id: caller.correlation_id().clone(),
    })
}

/// Telephony only: the current safe mapping of a called relay number (never a
/// destination number), or a generic RELAY_MAPPING_NOT_ACTIVE.
async fn resolve_inbound(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<crate::relay_numbers::inbound::ResolveInboundRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let requester = inbound_requester(&caller, InternalPermission::RelayNumberResolveInbound)?;
    let request = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        relay(&state).resolve_inbound(&requester, request).await?,
        false,
    ))
}

/// Telephony only: protect an inbound caller's presented number in the scope of a
/// RESOLVED inbound resolution; answers its ContactReference and whom it may reach.
async fn protect_inbound_caller(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<crate::contact_vault::inbound::ProtectInboundRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let requester = inbound_requester(&caller, InternalPermission::ProtectedContactProtectInbound)?;
    let request = body(raw)?;
    let mapped = relay(&state)
        .inbound_protection_scope(&requester.service, request.inbound_resolution_id)
        .await?;
    let actor = crate::contact_vault::ContactActor::service(
        mapped.scope,
        requester.service.clone(),
        None,
        requester.request_id.clone(),
        requester.correlation_id.clone(),
    );
    let (created, contact) = state
        .contacts
        .protect_inbound(
            &actor,
            request,
            Some(&mapped.country),
            crate::contact_vault::audit::ExecutionRefs {
                context: Some(mapped.context_id),
                relay_session: Some(mapped.relay_session_id),
                ..Default::default()
            },
        )
        .await?;
    let targets = relay(&state)
        .inbound_caller_targets(&mapped, contact)
        .await?;
    Ok(respond(
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        serde_json::json!({
            "inboundCaller": {
                "contactReference": contact,
                "created": created,
                "contextId": mapped.context_id,
                "relaySessionId": mapped.relay_session_id,
                "allowedTargets": targets,
            }
        }),
        false,
    ))
}

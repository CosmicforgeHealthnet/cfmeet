//! Call creation and control API (Part C §27, §54–§57, §69, §74–§76). All routes run
//! behind platform credential authentication (`routes::app`) and take tenant /
//! application / environment from the authenticated context only; request bodies carry
//! CF Meet concepts only and refuse unknown fields — there is no way to send a state, a
//! scope, an authorization result, a route, a provider, a room, a media identity or a
//! permission.
//!
//! Concurrency: every state-changing command carries `expectedVersion` (one mechanism,
//! no If-Match). Idempotency: `Idempotency-Key` is required for `POST /v1/calls` and
//! `POST …/transfer` and honoured on every other command; a replay returns the recorded
//! response with `Idempotent-Replayed: true`, a different request under the same key is
//! 409 IDEMPOTENCY_CONFLICT.

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

use super::{create::CreateAppCall, media::MediaCredentialActor, InvitationActor, TransferRequest};
use crate::{
    calls::{
        command::CallCommandContext,
        ids::{CallId, CallInvitationId, CallParticipantId, CallSessionId},
        kinds::{CallKind, CommandOperation},
        participants::DisplayName,
        service::{fingerprint, CommandKey, IdempotencyKey},
    },
    domain::PrincipalId,
    endpoints::CallEndpointId,
    error::CallingError,
    platform::{context::PlatformAuthContext, scopes::names},
    state::AppState,
};

pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
pub const IDEMPOTENT_REPLAYED_HEADER: &str = "idempotent-replayed";
const MAX_BODY_BYTES: usize = 16 * 1024;

/// Customer routes of call creation and control.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calls", post(create_call))
        .route("/v1/calls/:call_id/actions", get(available_actions))
        .route("/v1/calls/:call_id/accept", post(accept))
        .route("/v1/calls/:call_id/decline", post(decline))
        .route("/v1/calls/:call_id/cancel", post(cancel))
        .route("/v1/calls/:call_id/end", post(end))
        .route("/v1/calls/:call_id/hold", post(hold))
        .route("/v1/calls/:call_id/resume", post(resume))
        .route("/v1/calls/:call_id/transfer", post(transfer))
        .route(
            "/v1/calls/:call_id/sessions/:session_id/media-credentials",
            post(media_credentials),
        )
        .route(
            "/v1/calls/:call_id/sessions/:session_id/remove",
            post(remove_session),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// JSON response that is never cached.
pub(crate) fn respond<T: Serialize>(status: StatusCode, body: T, replayed: bool) -> Response {
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if replayed {
        headers.insert(IDEMPOTENT_REPLAYED_HEADER, HeaderValue::from_static("true"));
    }
    response
}

pub(crate) fn parse_body<T: DeserializeOwned>(
    body: Result<Json<T>, JsonRejection>,
) -> Result<T, CallingError> {
    body.map(|Json(b)| b)
        .map_err(|_| CallingError::Validation("The request body is invalid."))
}

pub(crate) fn uuid_path(value: &str) -> Result<Uuid, CallingError> {
    Uuid::parse_str(value).map_err(|_| CallingError::NotFound)
}

/// The command's idempotency identity, if an Idempotency-Key was sent.
pub(crate) fn command_key<T: Serialize>(
    headers: &HeaderMap,
    operation: CommandOperation,
    target: Option<Uuid>,
    body: &T,
    required: bool,
) -> Result<Option<CommandKey>, CallingError> {
    let mut values = headers.get_all(IDEMPOTENCY_KEY_HEADER).iter();
    let Some(value) = values.next() else {
        return if required {
            Err(CallingError::Validation(
                "The Idempotency-Key header is required.",
            ))
        } else {
            Ok(None)
        };
    };
    if values.next().is_some() {
        return Err(CallingError::Validation(
            "Idempotency-Key must be sent once.",
        ));
    }
    let key = value
        .to_str()
        .ok()
        .and_then(|v| IdempotencyKey::parse(v).ok())
        .ok_or(CallingError::Validation(
            "Idempotency-Key must be 1-255 printable ASCII characters without spaces.",
        ))?;
    let canonical = serde_json::to_value(body).map_err(|_| CallingError::Internal)?;
    Ok(Some(CommandKey {
        operation,
        key,
        fingerprint: fingerprint(operation, target, &canonical),
    }))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateCallBody {
    kind: CallKind,
    initiator_principal_id: Uuid,
    /// Required for application calls (the initiator's ACTIVE endpoint); absent for a
    /// callback relay (the initiator is called on its protected contact).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initiator_endpoint_id: Option<Uuid>,
    /// The requester asks to be called back on its protected contact and bridged to the
    /// callee (Part E §47). A request, not a route: Privacy Relay and the Routing Engine
    /// still decide whether CALLBACK_RELAY is allowed.
    #[serde(default)]
    callback_requested: bool,
    /// The requester's own verified protected contact (callback relay only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initiator_contact_reference: Option<Uuid>,
    #[serde(default)]
    target_principal_ids: Vec<Uuid>,
    /// A protected telephone contact (opaque ContactReference — never a number): the
    /// call is an app-to-phone call executed by Telephony (Part E §44). Requires
    /// `privacyRelaySessionId` and no principal targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_contact_reference: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initiator_display_name: Option<String>,
    /// Optional Privacy Relay Session: the call is authorized by Privacy Relay
    /// (PRIVACY_RELAY_V1) and fails closed if that authority is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    privacy_relay_session_id: Option<Uuid>,
    /// An enterprise extension number (Part F §54): resolved in the credential's scope
    /// and routed by enterprise configuration (ENTERPRISE_INTERNAL). Requires
    /// `enterprise.calls.use`; excludes every other target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_extension: Option<String>,
}

async fn create_call(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    body: Result<Json<CreateCallBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let body = parse_body(body)?;
    let key = command_key(&headers, CommandOperation::CreateCall, None, &body, true)?
        .ok_or(CallingError::Internal)?;
    if body.target_principal_ids.len() > 64 {
        return Err(CallingError::Validation("targetPrincipalIds is too long."));
    }
    let display = body
        .initiator_display_name
        .as_deref()
        .map(DisplayName::parse)
        .transpose()
        .map_err(|_| CallingError::Validation("initiatorDisplayName is invalid."))?;
    if let Some(extension) = &body.target_extension {
        ctx.require_scope(names::ENTERPRISE_CALLS_USE)?;
        let (true, false, None, None, None, Some(endpoint)) = (
            body.target_principal_ids.is_empty(),
            body.callback_requested,
            body.target_contact_reference,
            body.privacy_relay_session_id,
            body.initiator_contact_reference,
            body.initiator_endpoint_id,
        ) else {
            return Err(CallingError::Validation(
                "targetExtension requires initiatorEndpointId and excludes every other target.",
            ));
        };
        if extension.is_empty() || extension.len() > 32 {
            return Err(CallingError::Validation("targetExtension is invalid."));
        }
        let created = state
            .app
            .create_enterprise_call(
                &ctx,
                crate::calls::app::enterprise::CreateEnterpriseCall {
                    kind: body.kind,
                    initiator: PrincipalId::from_uuid(body.initiator_principal_id),
                    initiator_endpoint: CallEndpointId::from_uuid(endpoint),
                    extension: extension.clone(),
                    initiator_display_name: display,
                    key,
                },
            )
            .await?;
        let status = StatusCode::from_u16(created.status).unwrap_or(StatusCode::CREATED);
        return Ok(respond(status, created.body, created.replayed));
    }
    if body.callback_requested {
        let (true, None, Some(initiator_contact), Some(target_contact), Some(session)) = (
            body.target_principal_ids.is_empty(),
            body.initiator_endpoint_id,
            body.initiator_contact_reference,
            body.target_contact_reference,
            body.privacy_relay_session_id,
        ) else {
            return Err(CallingError::Validation(
                "callbackRequested requires initiatorContactReference, targetContactReference and privacyRelaySessionId, and no endpoint or principal targets.",
            ));
        };
        if body.kind != CallKind::Voice {
            return Err(CallingError::Validation(
                "a callback relay is a VOICE call.",
            ));
        }
        let created = state
            .app
            .create_callback_relay(
                &ctx,
                crate::calls::app::public::CreateCallbackRelay {
                    initiator: PrincipalId::from_uuid(body.initiator_principal_id),
                    initiator_contact,
                    target_contact,
                    privacy_relay_session: session,
                    key,
                },
            )
            .await?;
        let status = StatusCode::from_u16(created.status).unwrap_or(StatusCode::CREATED);
        return Ok(respond(status, created.body, created.replayed));
    }
    if body.initiator_contact_reference.is_some() {
        return Err(CallingError::Validation(
            "initiatorContactReference is only valid with callbackRequested.",
        ));
    }
    let initiator_endpoint = body
        .initiator_endpoint_id
        .ok_or(CallingError::Validation("initiatorEndpointId is required."))?;
    if let Some(contact) = body.target_contact_reference {
        let (true, Some(session)) = (
            body.target_principal_ids.is_empty(),
            body.privacy_relay_session_id,
        ) else {
            return Err(CallingError::Validation(
                "targetContactReference requires privacyRelaySessionId and no targetPrincipalIds.",
            ));
        };
        let created = state
            .app
            .create_protected_contact_call(
                &ctx,
                crate::calls::app::public::CreatePublicCall {
                    kind: body.kind,
                    initiator: PrincipalId::from_uuid(body.initiator_principal_id),
                    initiator_endpoint: CallEndpointId::from_uuid(initiator_endpoint),
                    target_contact: contact,
                    initiator_display_name: display,
                    privacy_relay_session: session,
                    key,
                },
            )
            .await?;
        let status = StatusCode::from_u16(created.status).unwrap_or(StatusCode::CREATED);
        return Ok(respond(status, created.body, created.replayed));
    }
    let created = state
        .app
        .create_app_call(
            &ctx,
            CreateAppCall {
                kind: body.kind,
                initiator: PrincipalId::from_uuid(body.initiator_principal_id),
                initiator_endpoint: CallEndpointId::from_uuid(initiator_endpoint),
                targets: body
                    .target_principal_ids
                    .iter()
                    .map(|p| PrincipalId::from_uuid(*p))
                    .collect(),
                initiator_display_name: display,
                privacy_relay_session: body.privacy_relay_session_id,
                key,
            },
        )
        .await?;
    let status = StatusCode::from_u16(created.status).unwrap_or(StatusCode::CREATED);
    Ok(respond(status, created.body, created.replayed))
}

async fn available_actions(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_READ)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let actions = state.app.available_actions(&ctx, call_id).await?;
    Ok(respond(StatusCode::OK, actions, false))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InvitationCommandBody {
    invitation_id: Uuid,
    endpoint_id: Uuid,
    expected_version: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VersionBody {
    pub expected_version: i64,
}

async fn invitation_command(
    state: AppState,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    call_id: String,
    body: Result<Json<InvitationCommandBody>, JsonRejection>,
    operation: CommandOperation,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let key = command_key(&headers, operation, Some(call_id), &body, false)?;
    let actor = InvitationActor::Backend {
        cmd: CallCommandContext::customer(&ctx),
        endpoint_id: CallEndpointId::from_uuid(body.endpoint_id),
    };
    let outcome = match operation {
        CommandOperation::Accept => {
            state
                .app
                .accept_invitation(
                    actor,
                    Some(CallId::from_uuid(call_id)),
                    CallInvitationId::from_uuid(body.invitation_id),
                    body.expected_version,
                    key.as_ref(),
                )
                .await?
        }
        _ => {
            state
                .app
                .decline_invitation(
                    actor,
                    Some(CallId::from_uuid(call_id)),
                    CallInvitationId::from_uuid(body.invitation_id),
                    body.expected_version,
                    key.as_ref(),
                )
                .await?
        }
    };
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

async fn accept(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<InvitationCommandBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    invitation_command(state, ctx, headers, call_id, body, CommandOperation::Accept).await
}

async fn decline(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<InvitationCommandBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    invitation_command(
        state,
        ctx,
        headers,
        call_id,
        body,
        CommandOperation::Decline,
    )
    .await
}

async fn version_command(
    state: AppState,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    call_id: String,
    body: Result<Json<VersionBody>, JsonRejection>,
    operation: CommandOperation,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let key = command_key(&headers, operation, Some(call_uuid), &body, false)?;
    let cmd = CallCommandContext::customer(&ctx);
    let call_id = CallId::from_uuid(call_uuid);
    let outcome = match operation {
        CommandOperation::Cancel => {
            state
                .app
                .cancel(&cmd, call_id, body.expected_version, key.as_ref())
                .await?
        }
        CommandOperation::End => {
            state
                .app
                .end(&cmd, call_id, body.expected_version, key.as_ref())
                .await?
        }
        CommandOperation::Hold => {
            state
                .app
                .hold(&cmd, call_id, body.expected_version, key.as_ref())
                .await?
        }
        _ => {
            state
                .app
                .resume(&cmd, call_id, body.expected_version, key.as_ref())
                .await?
        }
    };
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

async fn cancel(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::Cancel).await
}

async fn end(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::End).await
}

async fn hold(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::Hold).await
}

async fn resume(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::Resume).await
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransferBody {
    transferring_participant_id: Uuid,
    target_principal_id: Uuid,
    expected_version: i64,
}

async fn transfer(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<TransferBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let key = command_key(
        &headers,
        CommandOperation::Transfer,
        Some(call_uuid),
        &body,
        true,
    )?
    .ok_or(CallingError::Internal)?;
    let outcome = state
        .app
        .transfer(
            &ctx,
            CallId::from_uuid(call_uuid),
            TransferRequest {
                transferring_participant: CallParticipantId::from_uuid(
                    body.transferring_participant_id,
                ),
                target: PrincipalId::from_uuid(body.target_principal_id),
                expected_version: body.expected_version,
                key,
            },
        )
        .await?;
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyBody {}

async fn media_credentials(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, session_id)): Path<(String, String)>,
    body: Result<Json<EmptyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let session_id = CallSessionId::from_uuid(uuid_path(&session_id)?);
    // An absent body is an empty request; any field is refused (no self-granted permission).
    if let Err(rejection) = &body {
        if !matches!(rejection, JsonRejection::MissingJsonContentType(_)) {
            return Err(CallingError::Validation("The request body is invalid."));
        }
    }
    let credential = state
        .app
        .issue_media_credential(
            MediaCredentialActor::Backend(CallCommandContext::customer(&ctx)),
            Some(call_id),
            session_id,
        )
        .await?;
    Ok(respond(StatusCode::OK, credential, false))
}

async fn remove_session(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path((call_id, session_id)): Path<(String, String)>,
    body: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let session_uuid = uuid_path(&session_id)?;
    let body = parse_body(body)?;
    let key = command_key(
        &headers,
        CommandOperation::RemoveSession,
        Some(session_uuid),
        &body,
        false,
    )?;
    let outcome = state
        .app
        .remove_session(
            &CallCommandContext::customer(&ctx),
            CallId::from_uuid(call_uuid),
            CallSessionId::from_uuid(session_uuid),
            body.expected_version,
            key.as_ref(),
        )
        .await?;
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

/// The media provider callback route (provider signature only; no other credential).
pub fn callback_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/internal/provider/media/events",
            post(media_provider_event),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
}

async fn media_provider_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    use crate::media::provider::EventVerificationError;
    // An oversized or unreadable body is refused with the API's error envelope.
    let Ok(body) = body else {
        return CallingError::Validation("The callback is malformed.").into_response();
    };
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    match state.app.handle_provider_event(authorization, &body).await {
        Ok(_) => respond(
            StatusCode::OK,
            serde_json::json!({ "status": "accepted" }),
            false,
        ),
        Err(
            EventVerificationError::MissingSignature | EventVerificationError::InvalidSignature,
        ) => {
            tracing::warn!("provider callback rejected (signature)");
            CallingError::AuthenticationFailed.into_response()
        }
        Err(EventVerificationError::Malformed) => {
            CallingError::Validation("The callback is malformed.").into_response()
        }
    }
}

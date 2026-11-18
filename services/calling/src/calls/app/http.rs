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
    extract::{
        rejection::{JsonRejection, QueryRejection},
        DefaultBodyLimit, Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

use super::{
    create::CreateAppCall, enterprise::TransferTarget, media::MediaCredentialActor,
    InvitationActor, TransferRequest,
};
use crate::{
    calls::{
        command::{CallCommandContext, CallScope},
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
        .route("/v1/calls/:call_id/acknowledge", post(acknowledge))
        .route("/v1/calls/:call_id/accept", post(accept))
        .route("/v1/calls/:call_id/decline", post(decline))
        .route("/v1/calls/:call_id/cancel", post(cancel))
        .route("/v1/calls/:call_id/end", post(end))
        .route("/v1/calls/:call_id/hold", post(hold))
        .route("/v1/calls/:call_id/resume", post(resume))
        .route("/v1/calls/:call_id/transfer", post(transfer))
        .route("/v1/calls/:call_id/consultations", post(start_consultation))
        .route(
            "/v1/calls/:call_id/consultations/:consultation_id/complete",
            post(complete_consultation),
        )
        .route(
            "/v1/calls/:call_id/consultations/:consultation_id/cancel",
            post(cancel_consultation),
        )
        .route("/v1/calls/:call_id/upgrade", post(upgrade))
        .route("/v1/calls/:call_id/screen-share", post(start_screen_share))
        .route(
            "/v1/calls/:call_id/screen-share/stop",
            post(stop_screen_share),
        )
        .route("/v1/calls/:call_id/participants", post(add_participants))
        .route("/v1/calls/:call_id/host", post(transfer_host))
        .route("/v1/calls/:call_id/history", delete(hide_from_history))
        .route(
            "/v1/calls/:call_id/participants/:participant_id/remove",
            post(remove_participant),
        )
        .route(
            "/v1/calls/:call_id/participants/:participant_id/mute",
            post(mute_participant),
        )
        .route(
            "/v1/calls/:call_id/participants/:participant_id/rejoin",
            post(rejoin_call),
        )
        .route("/v1/calls/:call_id/ivr/input", post(ivr_input))
        .route(
            "/v1/calls/:call_id/sessions/:session_id/media-credentials",
            post(media_credentials),
        )
        .route(
            "/v1/calls/:call_id/sessions/:session_id/remove",
            post(remove_session),
        )
        .route(
            "/v1/calls/:call_id/sessions/:session_id/quality-samples",
            post(quality_sample),
        )
        .route("/v1/calls/:call_id/quality", get(quality_summary))
        .route("/v1/blocks", post(create_block).get(list_blocks))
        .route("/v1/blocks/:block_id", delete(release_block))
        .route(
            "/v1/calls/:call_id/abuse-reports",
            post(create_abuse_report),
        )
        .route("/v1/abuse-reports", get(list_abuse_reports))
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
    /// Part IV L: a Digital Communications Number (CF Meet Number Resource id — never the
    /// number) to present as the caller identity of a protected-contact call. Numbering
    /// must hold it ACTIVE for this scope and the initiator must be entitled to present
    /// it; refused otherwise (never another identity substituted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    presented_number_resource_id: Option<Uuid>,
}

/// The bounded request kind of a create-call body (metrics label).
fn request_kind(body: &CreateCallBody) -> &'static str {
    if body.target_extension.is_some() {
        "enterprise"
    } else if body.callback_requested {
        "callback_relay"
    } else if body.target_contact_reference.is_some() {
        "public_outbound"
    } else {
        "application"
    }
}

/// `POST /v1/calls` with the call-request metrics (§84): one request per kind, and every
/// refusal by its bounded public error code (never an identifier).
async fn create_call(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    body: Result<Json<CreateCallBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    let kind = match &body {
        Ok(Json(body)) => request_kind(body),
        Err(_) => "malformed",
    };
    crate::metrics::inc("calling_call_requests_total", &[("kind", kind)]);
    let result = create_call_inner(state, ctx, headers, body).await;
    if let Err(error) = &result {
        let reason = error.code().as_str();
        crate::metrics::inc(
            "calling_call_request_refusals_total",
            &[("kind", kind), ("reason", reason)],
        );
    }
    result
}

async fn create_call_inner(
    state: AppState,
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
    if body.presented_number_resource_id.is_some() && body.target_contact_reference.is_none()
        || body.presented_number_resource_id.is_some() && body.callback_requested
    {
        return Err(CallingError::Validation(
            "presentedNumberResourceId is only valid for a targetContactReference call.",
        ));
    }
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
                    presented_number_resource_id: body.presented_number_resource_id,
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

/// Cancel / end / hold / resume: the version and (C4, end / hold / resume) the acting
/// participant, which must be the host of a group Call.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ControlBody {
    pub expected_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acting_participant_id: Option<Uuid>,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcknowledgeBody {
    invitation_id: Uuid,
    endpoint_id: Uuid,
}

/// The application backend acknowledges receipt of a ring offer for one of its endpoints
/// (C1; the endpoint itself uses POST /v1/calling/invitations/{id}/acknowledge).
async fn acknowledge(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    body: Result<Json<AcknowledgeBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let actor = InvitationActor::Backend {
        cmd: CallCommandContext::customer(&ctx),
        endpoint_id: CallEndpointId::from_uuid(body.endpoint_id),
    };
    let result = state
        .app
        .acknowledge_invitation(
            actor,
            Some(CallId::from_uuid(call_id)),
            CallInvitationId::from_uuid(body.invitation_id),
        )
        .await?;
    Ok(respond(StatusCode::OK, result, false))
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
    body: Result<Json<ControlBody>, JsonRejection>,
    operation: CommandOperation,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let key = command_key(&headers, operation, Some(call_uuid), &body, false)?;
    let cmd = CallCommandContext::customer(&ctx);
    let call_id = CallId::from_uuid(call_uuid);
    // C4: END / HOLD / RESUME for everyone in a group Call are the host's; CANCEL (before
    // anyone answered) names no participant.
    match (operation, body.acting_participant_id) {
        (CommandOperation::Cancel, Some(_)) => {
            return Err(CallingError::Validation(
                "actingParticipantId is not accepted by cancel.",
            ))
        }
        (_, acting) => {
            state
                .app
                .require_host_for_everyone(
                    CallScope::of_platform(&ctx),
                    call_id,
                    acting.map(CallParticipantId::from_uuid),
                )
                .await?
        }
    }
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
    body: Result<Json<ControlBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::Cancel).await
}

async fn end(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<ControlBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::End).await
}

async fn hold(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<ControlBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::Hold).await
}

async fn resume(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<ControlBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    version_command(state, ctx, headers, call_id, body, CommandOperation::Resume).await
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpgradeBody {
    kind: CallKind,
    expected_version: i64,
}

/// `POST /v1/calls/:call_id/upgrade` (C5): VOICE → VIDEO of a live Call (calls.write;
/// Idempotency-Key honoured).
async fn upgrade(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<UpgradeBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let key = command_key(
        &headers,
        CommandOperation::Upgrade,
        Some(call_uuid),
        &body,
        false,
    )?;
    let outcome = state
        .app
        .upgrade(
            &CallCommandContext::customer(&ctx),
            CallId::from_uuid(call_uuid),
            body.kind,
            body.expected_version,
            key.as_ref(),
        )
        .await?;
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddParticipantsBody {
    inviting_participant_id: Uuid,
    target_principal_ids: Vec<Uuid>,
    expected_version: i64,
}

/// `POST /v1/calls/:call_id/participants` (C7): an active participant invites principals
/// into the ongoing Call (calls.write; Idempotency-Key honoured).
async fn add_participants(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<AddParticipantsBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    if body.target_principal_ids.len() > 64 {
        return Err(CallingError::Validation("targetPrincipalIds is too long."));
    }
    let key = command_key(
        &headers,
        CommandOperation::AddParticipants,
        Some(call_uuid),
        &body,
        false,
    )?;
    let outcome = state
        .app
        .invite_participants(
            &ctx,
            CallId::from_uuid(call_uuid),
            super::InviteParticipants {
                inviting_participant: CallParticipantId::from_uuid(body.inviting_participant_id),
                targets: body
                    .target_principal_ids
                    .iter()
                    .map(|p| PrincipalId::from_uuid(*p))
                    .collect(),
                expected_version: body.expected_version,
            },
            key.as_ref(),
        )
        .await?;
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenShareBody {
    participant_id: Uuid,
}

/// `POST /v1/calls/:call_id/screen-share` (C6): the named participant starts presenting
/// (calls.write; Idempotency-Key honoured).
async fn start_screen_share(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<ScreenShareBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    screen_share_command(state, ctx, headers, call_id, body, true).await
}

/// `POST /v1/calls/:call_id/screen-share/stop` (C6): the presenter stops.
async fn stop_screen_share(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<ScreenShareBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    screen_share_command(state, ctx, headers, call_id, body, false).await
}

async fn screen_share_command(
    state: AppState,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    call_id: String,
    body: Result<Json<ScreenShareBody>, JsonRejection>,
    start: bool,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let operation = if start {
        CommandOperation::StartScreenShare
    } else {
        CommandOperation::StopScreenShare
    };
    let key = command_key(&headers, operation, Some(call_uuid), &body, false)?;
    let cmd = CallCommandContext::customer(&ctx);
    let call_id = CallId::from_uuid(call_uuid);
    let participant = CallParticipantId::from_uuid(body.participant_id);
    let outcome = if start {
        state
            .app
            .start_screen_share(&cmd, call_id, participant, key.as_ref())
            .await?
    } else {
        state
            .app
            .stop_screen_share(&cmd, call_id, participant, key.as_ref())
            .await?
    };
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransferBody {
    transferring_participant_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_principal_id: Option<Uuid>,
    /// Part F: an enterprise extension number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_extension: Option<String>,
    /// Part F: a typed enterprise destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_destination: Option<crate::enterprise::destinations::Destination>,
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
    let target = match (
        body.target_principal_id,
        body.target_extension.clone(),
        body.target_destination,
    ) {
        (Some(p), None, None) => TransferTarget::Principal(PrincipalId::from_uuid(p)),
        (None, Some(number), None) => TransferTarget::Extension(number),
        (None, None, Some(d)) => TransferTarget::Destination(d),
        _ => return Err(CallingError::Validation(
            "exactly one of targetPrincipalId, targetExtension or targetDestination is required.",
        )),
    };
    if !matches!(target, TransferTarget::Principal(_)) {
        // Enterprise targets need the enterprise calling scope, never implied by calls.*.
        ctx.require_scope(names::ENTERPRISE_CALLS_USE)?;
    }
    let call_id = CallId::from_uuid(call_uuid);
    let resolved = state
        .app
        .resolve_transfer_target(&ctx, call_id, target)
        .await?;
    let outcome = state
        .app
        .transfer(
            &ctx,
            call_id,
            TransferRequest {
                transferring_participant: CallParticipantId::from_uuid(
                    body.transferring_participant_id,
                ),
                target: resolved.principal,
                expected_version: body.expected_version,
                key,
                enterprise_destination: resolved.destination,
            },
        )
        .await?;
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

fn one_transfer_target(
    principal: Option<Uuid>,
    extension: Option<String>,
    destination: Option<crate::enterprise::destinations::Destination>,
) -> Result<TransferTarget, CallingError> {
    match (principal, extension, destination) {
        (Some(p), None, None) => Ok(TransferTarget::Principal(PrincipalId::from_uuid(p))),
        (None, Some(number), None) => Ok(TransferTarget::Extension(number)),
        (None, None, Some(d)) => Ok(TransferTarget::Destination(d)),
        _ => Err(CallingError::Validation(
            "exactly one of targetPrincipalId, targetExtension or targetDestination is required.",
        )),
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConsultationBody {
    transferring_participant_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_principal_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_destination: Option<crate::enterprise::destinations::Destination>,
    /// The transferring participant's own endpoint the consultation Call is placed from.
    initiator_endpoint_id: Uuid,
    /// The consultation Call's kind (default VOICE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<CallKind>,
    expected_version: i64,
}

/// `POST /v1/calls/:call_id/consultations` (C14): hold the Call and place a consultation
/// Call from the transferring participant to the target (calls.write; Idempotency-Key
/// required; enterprise targets need enterprise.calls.use).
async fn start_consultation(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    headers: HeaderMap,
    Path(call_id): Path<String>,
    body: Result<Json<ConsultationBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let key = command_key(
        &headers,
        CommandOperation::CreateCall,
        Some(call_uuid),
        &body,
        true,
    )?
    .ok_or(CallingError::Internal)?;
    let target = one_transfer_target(
        body.target_principal_id,
        body.target_extension.clone(),
        body.target_destination,
    )?;
    if !matches!(target, TransferTarget::Principal(_)) {
        ctx.require_scope(names::ENTERPRISE_CALLS_USE)?;
    }
    let started = state
        .app
        .start_consultation(
            &ctx,
            CallId::from_uuid(call_uuid),
            super::ConsultationRequest {
                transferring_participant: CallParticipantId::from_uuid(
                    body.transferring_participant_id,
                ),
                target,
                initiator_endpoint: CallEndpointId::from_uuid(body.initiator_endpoint_id),
                kind: body.kind.unwrap_or(CallKind::Voice),
                expected_version: body.expected_version,
                key,
            },
        )
        .await?;
    let status = StatusCode::from_u16(started.status).unwrap_or(StatusCode::CREATED);
    Ok(respond(status, started.body, started.replayed))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteConsultationBody {
    /// The original Call's version.
    expected_version: i64,
}

/// `POST /v1/calls/:call_id/consultations/:consultation_id/complete` (C14): transfer the
/// original Call to the consulted target (calls.write; idempotent per consultation).
async fn complete_consultation(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, consultation_id)): Path<(String, String)>,
    body: Result<Json<CompleteConsultationBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let consultation = uuid_path(&consultation_id)?;
    let body = parse_body(body)?;
    let outcome = state
        .app
        .complete_consultation(
            &ctx,
            CallId::from_uuid(call_uuid),
            consultation,
            body.expected_version,
        )
        .await?;
    Ok(respond(StatusCode::OK, outcome.body, outcome.replayed))
}

/// `POST /v1/calls/:call_id/consultations/:consultation_id/cancel` (C14): end the
/// consultation Call and resume the original (calls.write; idempotent).
async fn cancel_consultation(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, consultation_id)): Path<(String, String)>,
    body: Result<Json<EmptyBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_uuid = uuid_path(&call_id)?;
    let consultation = uuid_path(&consultation_id)?;
    parse_body(body)?;
    let outcome = state
        .app
        .cancel_consultation(&ctx, CallId::from_uuid(call_uuid), consultation)
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransferHostBody {
    acting_participant_id: Uuid,
    host_participant_id: Uuid,
}

/// `POST /v1/calls/:call_id/host` (C4, calls.write): the host hands the role over.
async fn transfer_host(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    body: Result<Json<TransferHostBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let b = parse_body(body)?;
    let out = state
        .app
        .transfer_host(
            &CallCommandContext::customer(&ctx),
            call_id,
            CallParticipantId::from_uuid(b.acting_participant_id),
            CallParticipantId::from_uuid(b.host_participant_id),
        )
        .await?;
    Ok(respond(StatusCode::OK, out, false))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostActionBody {
    acting_participant_id: Uuid,
}

/// `POST /v1/calls/:call_id/participants/:participant_id/remove` (C4, calls.write): the
/// host removes a participant (it cannot rejoin).
async fn remove_participant(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, participant_id)): Path<(String, String)>,
    body: Result<Json<HostActionBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let participant = CallParticipantId::from_uuid(uuid_path(&participant_id)?);
    let b = parse_body(body)?;
    let out = state
        .app
        .remove_participant(
            &CallCommandContext::customer(&ctx),
            call_id,
            CallParticipantId::from_uuid(b.acting_participant_id),
            participant,
        )
        .await?;
    Ok(respond(StatusCode::OK, out, false))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MuteBody {
    acting_participant_id: Uuid,
    muted: bool,
}

/// `POST /v1/calls/:call_id/participants/:participant_id/mute` (C4, calls.write): the host
/// mutes or unmutes a participant's audio.
async fn mute_participant(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, participant_id)): Path<(String, String)>,
    body: Result<Json<MuteBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let participant = CallParticipantId::from_uuid(uuid_path(&participant_id)?);
    let b = parse_body(body)?;
    let out = state
        .app
        .mute_participant(
            &CallCommandContext::customer(&ctx),
            call_id,
            CallParticipantId::from_uuid(b.acting_participant_id),
            participant,
            b.muted,
        )
        .await?;
    Ok(respond(StatusCode::OK, out, false))
}

/// `DELETE /v1/calls/{call_id}/history?principalId=` (calls.write): the principal hides a
/// finished Call from its own history (204, idempotent).
async fn hide_from_history(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let Query(pairs) =
        query.map_err(|_| CallingError::Validation("The query string is invalid."))?;
    let principal = match pairs.as_slice() {
        [(k, v)] if k == "principalId" => Uuid::parse_str(v)
            .map(PrincipalId::from_uuid)
            .map_err(|_| CallingError::Validation("principalId must be a UUID."))?,
        _ => {
            return Err(CallingError::Validation(
                "principalId (and nothing else) is required.",
            ))
        }
    };
    let call_id = Uuid::parse_str(&call_id)
        .map(CallId::from_uuid)
        .map_err(|_| CallingError::NotFound)?;
    state
        .app
        .hide_from_history(CallScope::of_platform(&ctx), principal, call_id)
        .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RejoinBody {
    endpoint_id: Uuid,
}

/// `POST /v1/calls/:call_id/participants/:participant_id/rejoin` (calls.write): a
/// participant who left an ongoing call comes back on one of its endpoints.
async fn rejoin_call(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, participant_id)): Path<(String, String)>,
    body: Result<Json<RejoinBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let participant = CallParticipantId::from_uuid(uuid_path(&participant_id)?);
    let b = parse_body(body)?;
    let out = state
        .app
        .rejoin_call(
            &CallCommandContext::customer(&ctx),
            call_id,
            participant,
            b.endpoint_id,
        )
        .await?;
    let status = if out["rejoined"] == true {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(respond(status, out, false))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QualitySampleBody {
    jitter_ms: f64,
    packet_loss_percent: f64,
    rtt_ms: f64,
    #[serde(default)]
    interval_seconds: Option<i64>,
}

/// `POST /v1/calls/:call_id/sessions/:session_id/quality-samples` (C8, calls.write): one
/// client measurement of the session's media path.
async fn quality_sample(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path((call_id, session_id)): Path<(String, String)>,
    body: Result<Json<QualitySampleBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let session_id = CallSessionId::from_uuid(uuid_path(&session_id)?);
    let b = parse_body(body)?;
    let recorded = state
        .app
        .record_quality_sample(
            &CallCommandContext::customer(&ctx),
            call_id,
            session_id,
            super::quality::QualitySample {
                jitter_ms: b.jitter_ms,
                packet_loss_percent: b.packet_loss_percent,
                rtt_ms: b.rtt_ms,
                interval_seconds: b.interval_seconds,
            },
        )
        .await?;
    Ok(respond(StatusCode::CREATED, recorded, false))
}

/// `GET /v1/calls/:call_id/quality` (C8, calls.read): per-participant quality aggregates.
async fn quality_summary(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_READ)?;
    let call_id = CallId::from_uuid(uuid_path(&call_id)?);
    let summary = state
        .app
        .quality_summary(CallScope::of_platform(&ctx), call_id)
        .await?;
    Ok(respond(StatusCode::OK, summary, false))
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IvrInputBody {
    endpoint_id: Uuid,
    /// One key (0-9, * or #). Never logged or stored.
    digit: String,
}

impl std::fmt::Display for IvrInputBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IvrInputBody(<redacted>)")
    }
}

/// Part F §57: the RTC caller's keypad input to the IVR its Call waits in. Only the
/// Call's initiator endpoint may press keys; the key is used once and never stored.
async fn ivr_input(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    body: Result<Json<IvrInputBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    ctx.require_scope(names::ENTERPRISE_CALLS_USE)?;
    let call_id = uuid_path(&call_id)?;
    let body = parse_body(body)?;
    let accepted = state
        .app
        .ivr_keypad(
            CallScope::of_platform(&ctx),
            call_id,
            body.endpoint_id,
            &body.digit,
        )
        .await?;
    Ok(respond(
        StatusCode::OK,
        serde_json::json!({ "callId": call_id, "accepted": accepted }),
        false,
    ))
}

// ---------------------------------------------------------------------------
// Part H §70 block rules and §69 abuse reports.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBlockBody {
    #[serde(default)]
    principal_id: Option<Uuid>,
    #[serde(default)]
    blocked_principal_id: Option<Uuid>,
    #[serde(default)]
    blocked_contact_reference_id: Option<Uuid>,
    reason_code: String,
}

/// `POST /v1/blocks`: a principal (calls.write) or the application
/// (enterprise.config.write) blocks a principal or an opaque contact. Idempotent per
/// active pair (200 with the existing rule).
async fn create_block(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    body: Result<Json<CreateBlockBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    // Authorization before validation: a credential with neither blocking scope learns
    // nothing about the body (the blocker-specific scope is checked again below).
    ctx.require_scope(names::CALLS_WRITE)
        .or_else(|_| ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE))?;
    let b = parse_body(body)?;
    let (view, created) = state
        .app
        .create_block_rule(
            &ctx,
            super::blocks::CreateBlockRule {
                principal_id: b.principal_id,
                blocked_principal_id: b.blocked_principal_id,
                blocked_contact_reference_id: b.blocked_contact_reference_id,
                reason_code: b.reason_code,
            },
        )
        .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(respond(status, view, false))
}

/// `GET /v1/blocks?principalId=`: the principal's rules, or the application's own
/// rules without `principalId` (calls.read).
async fn list_blocks(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    let q = crate::enterprise::http::QueryParams::parse(query, &["principalId"])?;
    let principal = q.uuid("principalId")?;
    let items = state.app.list_block_rules(&ctx, principal).await?;
    Ok(respond(
        StatusCode::OK,
        serde_json::json!({ "items": items }),
        false,
    ))
}

/// `DELETE /v1/blocks/:block_id`: releases the rule (same scope rule as creation).
async fn release_block(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(block_id): Path<String>,
) -> Result<Response, CallingError> {
    let id = uuid_path(&block_id)?;
    let view = state.app.release_block_rule(&ctx, id).await?;
    Ok(respond(StatusCode::OK, view, false))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AbuseReportBody {
    principal_id: Uuid,
    category: String,
    #[serde(default)]
    comment_code: Option<String>,
    /// C5: the participant being reported (required to name one in a call of more than
    /// two people; must be another participant of that call).
    #[serde(default)]
    reported_principal_id: Option<Uuid>,
}

/// `POST /v1/calls/:call_id/abuse-reports` (calls.write): a participant reports the
/// Call. Idempotent per (call, reporter): 200 with the existing report.
async fn create_abuse_report(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Path(call_id): Path<String>,
    body: Result<Json<AbuseReportBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    let call_id = uuid_path(&call_id)?;
    let b = parse_body(body)?;
    let (view, created) = state
        .app
        .create_abuse_report(
            &ctx,
            call_id,
            super::abuse_reports::CreateAbuseReport {
                principal_id: b.principal_id,
                category: b.category,
                comment_code: b.comment_code,
                reported_principal_id: b.reported_principal_id,
            },
        )
        .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(respond(status, view, false))
}

/// `GET /v1/abuse-reports?principalId=` (calls.read): the reports a principal filed.
async fn list_abuse_reports(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, CallingError> {
    let q = crate::enterprise::http::QueryParams::parse(query, &["principalId"])?;
    let principal = q
        .uuid("principalId")?
        .ok_or(CallingError::Validation("principalId is required."))?;
    let items = state.app.list_abuse_reports(&ctx, principal).await?;
    Ok(respond(
        StatusCode::OK,
        serde_json::json!({ "items": items }),
        false,
    ))
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

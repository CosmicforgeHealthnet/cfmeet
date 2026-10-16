//! Routing grants (Workstream K), service-authenticated (`/internal/v1`):
//!
//! * `POST /routing/inbound-resolutions` (`number-route.resolve`; Telephony for VOICE,
//!   the SMS Gateway for SMS) — the routing of a received call / message with an inbound
//!   grant;
//! * `POST /routing/caller-identity-grants` (`number-caller-identity.authorize`, Calling,
//!   signed delegated customer context) — a single-use CALLER_IDENTITY grant for Telephony;
//! * `POST /routing/sms-sender-grants` (`number-sms-sender.authorize`, SMS Gateway,
//!   delegated context) — a single-use SMS_SENDER grant;
//! * `POST /routing/grant-verifications` (`number-grant.verify`) — the grant's audience
//!   verifies (and, for an outbound grant, consumes) it against CURRENT state.
//!
//! Without a configured grant key every route answers 503 (fail closed). Grants are bearer
//! statements handed to the requesting service only; no customer route returns one.

use axum::{extract::State, routing::post, Json, Router};
use cf_service_auth::{number_grant::NumberGrantKind, InternalPermission, ServiceId};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
    numbers::ctx,
};
use crate::{
    grants::{DelegatedScope, InboundResolution, IssuedGrant, RoutingGrants},
    numbers::acquisitions::RouteFamily,
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/routing/inbound-resolutions", post(resolve_inbound))
        .route("/routing/caller-identity-grants", post(caller_identity))
        .route("/routing/sms-sender-grants", post(sms_sender))
        .route("/routing/grant-verifications", post(verify))
        .route(
            "/routing/number-assignments/:number_resource_id",
            axum::routing::get(current_assignment),
        )
}

/// `GET /routing/number-assignments/{numberResourceId}` (`number-caller-identity.authorize`,
/// signed delegated context): the delegated scope's current assignment and routed targets,
/// so Calling can apply its enterprise membership policy BEFORE requesting a
/// caller-identity grant. Another scope's number is NOT_FOUND.
async fn current_assignment(
    State(s): State<AppState>,
    caller: InternalCaller,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberCallerIdentityAuthorize)?;
    let g = grants(&s)?;
    let delegated = caller
        .context()
        .delegated_platform_context()
        .ok_or(ApiError::Code(ErrorCode::InternalPermissionDenied))?;
    let scope = DelegatedScope {
        tenant_id: delegated.tenant_id(),
        application_id: delegated.application_id(),
        environment_id: delegated.application_environment_id(),
        principal_id: delegated.initiating_principal().map(|p| p.principal_id),
    };
    let c = g.current_assignment(scope, id).await?;
    let a = &c.assignment;
    // A family routed nowhere (NONE) has no target.
    let target = |kind: String, r: Option<Uuid>| match r {
        Some(r) if kind != "NONE" => json!({ "kind": kind, "ref": r }),
        _ => Value::Null,
    };
    Ok(Json(json!({
        "numberResourceId": a.number_resource_id,
        "assignmentId": a.id,
        "assignmentStatus": a.status,
        "ownershipClass": a.ownership_class,
        "ownerKind": a.owner_kind,
        "ownerRef": a.owner_ref,
        "voice": c.targets.as_ref().map(|t| target(t.voice.0.as_str().to_string(), t.voice.1)).unwrap_or(Value::Null),
        "sms": c.targets.as_ref().map(|t| target(t.sms.0.as_str().to_string(), t.sms.1)).unwrap_or(Value::Null),
        "routingRevision": c.routing_revision,
        "numberActive": c.number_active,
        "outbound": { "voice": c.voice_outbound, "sms": c.sms_outbound },
    })))
}

fn grants(s: &AppState) -> Result<&RoutingGrants, ApiError> {
    s.grants.as_ref().ok_or_else(|| {
        tracing::warn!("routing grant requested but no grant key is configured");
        ApiError::Code(ErrorCode::ServiceUnavailable)
    })
}

fn caller_id(caller: &InternalCaller) -> ServiceId {
    caller.context().authenticated_service().clone()
}

fn grant_view(g: &IssuedGrant) -> Value {
    json!({
        "grant": g.expose_for_delivery(),
        "grantId": g.record.id,
        "grantKind": g.record.grant_kind,
        "audience": g.record.audience,
        "numberResourceId": g.record.number_resource_id,
        "routingRevision": g.record.routing_revision,
        "issuedAt": g.record.issued_at,
        "expiresAt": g.record.expires_at,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboundBody {
    pub family: RouteFamily,
    /// The received (dialled) number, E.164.
    pub e164: String,
}

async fn resolve_inbound(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<InboundBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberRouteResolve)?;
    let g = grants(&s)?;
    let out = g
        .resolve_inbound(&ctx(), &caller_id(&caller), body.family, &body.e164)
        .await?;
    let scope = |a: &crate::numbers::assignments::AssignmentRow| {
        json!({
            "tenantId": a.tenant_id,
            "applicationId": a.application_id,
            "environmentId": a.environment_id,
        })
    };
    Ok(Json(match out {
        InboundResolution::Routed {
            assignment,
            target,
            routing_revision,
            grant,
        } => json!({
            "outcome": "ROUTED",
            "family": body.family,
            "numberResourceId": assignment.number_resource_id,
            "assignmentId": assignment.id,
            "scope": scope(&assignment),
            "ownershipClass": assignment.ownership_class,
            "target": { "kind": target.0, "ref": target.1 },
            "routingRevision": routing_revision,
            "grant": grant_view(&grant),
            "reason": null,
        }),
        InboundResolution::NotRouted {
            assignment,
            routing_revision,
        } => json!({
            "outcome": "NOT_ROUTED",
            "family": body.family,
            "numberResourceId": assignment.number_resource_id,
            "assignmentId": assignment.id,
            "scope": scope(&assignment),
            "ownershipClass": assignment.ownership_class,
            "target": null,
            "routingRevision": routing_revision,
            "grant": null,
            "reason": null,
        }),
        InboundResolution::Unavailable { reason } => json!({
            "outcome": "UNAVAILABLE",
            "family": body.family,
            "numberResourceId": null,
            "assignmentId": null,
            "scope": null,
            "ownershipClass": null,
            "target": null,
            "routingRevision": null,
            "grant": null,
            "reason": reason,
        }),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboundBody {
    pub number_resource_id: Uuid,
    /// The one operation (call / message) the grant authorizes.
    pub operation_ref: String,
}

async fn outbound(
    s: &AppState,
    caller: &InternalCaller,
    permission: InternalPermission,
    kind: NumberGrantKind,
    body: OutboundBody,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(permission)?;
    let g = grants(s)?;
    // The customer scope comes only from the signed delegated context.
    let delegated = caller
        .context()
        .delegated_platform_context()
        .ok_or(ApiError::Code(ErrorCode::InternalPermissionDenied))?;
    let scope = DelegatedScope {
        tenant_id: delegated.tenant_id(),
        application_id: delegated.application_id(),
        environment_id: delegated.application_environment_id(),
        principal_id: delegated.initiating_principal().map(|p| p.principal_id),
    };
    let grant = g
        .authorize_outbound(
            &ctx(),
            &caller_id(caller),
            kind,
            scope,
            body.number_resource_id,
            &body.operation_ref,
        )
        .await?;
    Ok(Json(grant_view(&grant)))
}

async fn caller_identity(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<OutboundBody>,
) -> Result<Json<Value>, ApiError> {
    outbound(
        &s,
        &caller,
        InternalPermission::NumberCallerIdentityAuthorize,
        NumberGrantKind::CallerIdentity,
        body,
    )
    .await
}

async fn sms_sender(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<OutboundBody>,
) -> Result<Json<Value>, ApiError> {
    outbound(
        &s,
        &caller,
        InternalPermission::NumberSmsSenderAuthorize,
        NumberGrantKind::SmsSender,
        body,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifyBody {
    pub grant: String,
    #[serde(default)]
    pub consume: bool,
    #[serde(default)]
    pub operation_ref: Option<String>,
}

async fn verify(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<VerifyBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberGrantVerify)?;
    let g = grants(&s)?;
    if body.grant.len() > 4096 {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    let v = g
        .verify(
            &caller_id(&caller),
            &body.grant,
            body.consume,
            body.operation_ref.as_deref(),
        )
        .await?;
    let Some(verified) = v.verified.as_ref().filter(|_| v.valid()) else {
        // A refusal names only the reason (and the grant id when it was recorded).
        return Ok(Json(json!({
            "valid": false,
            "reason": v.reason,
            "grantId": v.grant.as_ref().map(|g| g.id),
        })));
    };
    let record = v.grant.as_ref().expect("a valid grant is recorded");
    Ok(Json(json!({
        "valid": true,
        "reason": null,
        "grantId": record.id,
        "grantKind": verified.kind.as_str(),
        "numberResourceId": verified.number_resource_id,
        "assignmentId": verified.assignment_id,
        "routingRevision": verified.routing_revision,
        "scope": {
            "tenantId": verified.scope.tenant_id,
            "applicationId": verified.scope.application_id,
            "environmentId": verified.scope.environment_id,
        },
        "principalId": verified.principal_id,
        "target": verified.target.as_ref().map(|t| json!({ "kind": t.kind, "ref": t.target_ref })),
        "operationRef": verified.operation_ref,
        "e164": v.e164,
        "consumed": v.consumed,
        "expiresAt": record.expires_at,
    })))
}

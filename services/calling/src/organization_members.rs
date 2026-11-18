//! Gap audit G3 — first-party organization MEMBERS in their organization's own Calling
//! scope, over internal service authentication (Meeting Service only).
//!
//! Organization Enterprise Calling used to be administration only: a member had no
//! endpoint and no signaling in the organization's FIRST_PARTY scope, and removing a member
//! left the extensions routed to them active. The Meeting Service (the account authority
//! that authenticated the human and asked the Organizations Service whether they are an
//! active member allowed to use Calling) now acts for the member here:
//!
//! * `calling-organization-member.act` — the member's OWN calling presence:
//!   * `GET  /internal/v1/organization-members/endpoints[?includeRevoked=true]`
//!   * `POST /internal/v1/organization-members/endpoints` — register / refresh one endpoint
//!   * `POST /internal/v1/organization-members/endpoints/revoke` — `{endpointId}`
//!   * `POST /internal/v1/organization-members/signaling-credentials` — `{endpointId}`
//!
//!   The signed delegated context must name the organization's Calling credential (id
//!   only; re-verified LIVE with Platform Access exactly like a forwarded request, so a
//!   revoked or suspended binding stops at once) and the member's NATIVE_CF_USER principal,
//!   which must resolve ACTIVE inside that credential's scope; the tenant must be a
//!   FIRST_PARTY organization tenant (`org-<32 hex>`). Every operation acts on THAT
//!   principal only: the body never names a principal, and another principal's endpoint
//!   is answered 404. The domain code is the customer routes' own (`EndpointService`,
//!   `SignalingKeys`); the signaling credential is an organization member credential (see
//!   `signaling::credentials`), re-checked on every use like any other.
//! * `calling-organization-member.offboard` — `POST /internal/v1/organization-members/
//!   extension-status` `{principalId, status: ACTIVE | SUSPENDED}` in the delegated
//!   organization scope: SUSPENDED disables the ACTIVE extensions routed to that principal,
//!   ACTIVE restores exactly the ones disabled that way
//!   ([`EnterpriseService::apply_member_extension_status`]). Idempotent; answers the counts.
//!
//! Neither path widens the organization credential's scopes: the authority is the
//! receiver-granted permission, the live credential chain and the member's principal.
//!
//! [`EnterpriseService::apply_member_extension_status`]:
//!     crate::enterprise::EnterpriseService::apply_member_extension_status

use axum::{
    extract::{rejection::JsonRejection, rejection::QueryRejection, Query, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::{DelegatedPlatformContext, InternalPermission};
use chrono::SecondsFormat;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    calls::{
        app::http::{parse_body, respond},
        command::CallScope,
    },
    domain::{PrincipalId, PrincipalKind, RequestContext, TenantClassification},
    endpoints::{
        http::EndpointView, CallEndpointId, EndpointCapabilities, EndpointKey, EndpointKind,
        NewEndpoint,
    },
    error::CallingError,
    internal_auth::middleware::InternalCaller,
    middleware::platform_auth::{required_delegation, ForwardKind},
    platform::{
        authentication::{is_organization_tenant_slug, AuthenticationError},
        context::PlatformAuthContext,
        limits::RateDecision,
        principals::{PrincipalReference, PrincipalResolutionError, ResolvedPrincipal},
    },
    state::AppState,
};

/// Request bodies are small, closed objects.
const BODY_LIMIT: usize = 8 * 1024;

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organization-members/endpoints",
            get(list_endpoints).post(register_endpoint),
        )
        .route(
            "/organization-members/endpoints/revoke",
            post(revoke_endpoint),
        )
        .route(
            "/organization-members/signaling-credentials",
            post(mint_signaling_credential),
        )
        .route(
            "/organization-members/extension-status",
            post(extension_status),
        )
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
}

/// The authenticated organization member: the live organization credential context and
/// the member's ACTIVE NATIVE_CF_USER principal in it.
struct Member {
    ctx: PlatformAuthContext,
    principal: ResolvedPrincipal,
}

fn delegated(caller: &InternalCaller) -> Result<DelegatedPlatformContext, CallingError> {
    caller
        .context()
        .delegated_platform_context()
        .cloned()
        .ok_or_else(|| {
            tracing::info!("organization member request without a delegated context");
            CallingError::InternalPermissionDenied
        })
}

/// The permission alone (checked before the body is read, so an unauthorized caller
/// learns nothing about validation).
fn may_act(caller: &InternalCaller) -> Result<(), CallingError> {
    caller.require_permission(InternalPermission::CallingOrganizationMemberAct)
}

/// The member context; the caller passed [`may_act`].
async fn member(state: &AppState, caller: &InternalCaller) -> Result<Member, CallingError> {
    let d = delegated(caller)?;
    let acting = required_delegation(ForwardKind::Organization, &d)
        .ok()
        .flatten()
        .ok_or_else(|| {
            tracing::info!("organization member request without credential or member principal");
            CallingError::InternalPermissionDenied
        })?;
    let request = RequestContext {
        request_id: caller.context().request_id().clone(),
        correlation_id: caller.context().correlation_id().clone(),
    };
    let ctx = match state
        .authenticator
        .authenticate_delegated(&d, request)
        .await
    {
        Ok(ctx) => ctx,
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "organization member request refused: Platform Access unavailable");
            return Err(CallingError::ServiceUnavailable);
        }
        Err(AuthenticationError::Rejected { reason, .. })
        | Err(AuthenticationError::Throttled { reason, .. }) => {
            tracing::info!(
                reason = reason.as_str(),
                "organization member request refused: organization credential rejected"
            );
            return Err(CallingError::AuthenticationFailed);
        }
    };
    if ctx.is_safety_only()
        || ctx.tenant_classification() != TenantClassification::FirstParty
        || !ctx.is_organization_tenant()
    {
        tracing::info!("organization member request outside an active organization scope");
        return Err(CallingError::AuthenticationFailed);
    }
    let principal = match state
        .principals
        .resolve(
            &ctx,
            &PrincipalReference::Id(PrincipalId::from_uuid(acting)),
        )
        .await
    {
        Ok(p) if p.kind() == PrincipalKind::NativeCfUser => p,
        Ok(_)
        | Err(PrincipalResolutionError::NotFound)
        | Err(PrincipalResolutionError::Inactive)
        | Err(PrincipalResolutionError::InvalidReference) => {
            // A suspended (offboarded) member's principal is inactive: refused here.
            tracing::info!("organization member principal not active in the organization scope");
            return Err(CallingError::AuthenticationFailed);
        }
        Err(PrincipalResolutionError::Unavailable { summary }) => {
            tracing::error!(%summary, "principal directory unavailable; organization member request refused");
            return Err(CallingError::ServiceUnavailable);
        }
    };
    match state.rate_limiter.check(&ctx).await {
        Ok(RateDecision::Allowed) => {}
        Ok(RateDecision::Limited { retry_after_secs }) => {
            return Err(CallingError::RateLimited { retry_after_secs });
        }
        Err(_) => return Err(CallingError::ServiceUnavailable),
    }
    Ok(Member { ctx, principal })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListQuery {
    #[serde(default)]
    include_revoked: Option<bool>,
}

#[derive(Serialize)]
struct EndpointList {
    data: Vec<EndpointView>,
}

async fn list_endpoints(
    State(state): State<AppState>,
    caller: InternalCaller,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, CallingError> {
    may_act(&caller)?;
    let Query(query) =
        query.map_err(|_| CallingError::Validation("The query string is invalid."))?;
    let m = member(&state, &caller).await?;
    let endpoints = state
        .endpoints
        .list(
            CallScope::of_platform(&m.ctx),
            m.principal.id(),
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilitiesBody {
    audio: bool,
    video: bool,
}

/// `POST /v1/calling/endpoints` without `principalId`: always the member's own principal.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterBody {
    endpoint_key: String,
    endpoint_kind: EndpointKind,
    capabilities: CapabilitiesBody,
    #[serde(default)]
    push_target_ref: Option<Uuid>,
}

async fn register_endpoint(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<RegisterBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    may_act(&caller)?;
    let body = parse_body(body)?;
    let m = member(&state, &caller).await?;
    let key = EndpointKey::parse(&body.endpoint_key)
        .ok_or(CallingError::Validation("endpointKey is invalid."))?;
    let (endpoint, created) = state
        .endpoints
        .register(
            &m.ctx,
            &m.principal,
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
    Ok(respond(
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        EndpointView::from(&endpoint),
        false,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointBody {
    endpoint_id: Uuid,
}

async fn revoke_endpoint(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<EndpointBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    may_act(&caller)?;
    let id = CallEndpointId::from_uuid(parse_body(body)?.endpoint_id);
    let m = member(&state, &caller).await?;
    let scope = CallScope::of_platform(&m.ctx);
    // Another principal's (or an unknown) endpoint is indistinguishable: 404. The owner
    // never changes, so the read cannot race the revoke.
    match state.endpoints.get(scope, id).await {
        Ok(e) if e.principal_id == m.principal.id() => {}
        Ok(_) => return Err(CallingError::NotFound),
        Err(e) => return Err(e.into()),
    }
    let (endpoint, newly) = state.endpoints.revoke(scope, id).await?;
    if newly {
        state.app.cancel_endpoint_invitations(id).await?;
    }
    Ok(respond(
        StatusCode::OK,
        EndpointView::from(&endpoint),
        false,
    ))
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
    caller: InternalCaller,
    body: Result<Json<EndpointBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    may_act(&caller)?;
    let body = parse_body(body)?;
    let m = member(&state, &caller).await?;
    let scope = CallScope::of_platform(&m.ctx);
    let endpoint = state
        .endpoints
        .active_endpoint(
            scope,
            m.principal.id(),
            CallEndpointId::from_uuid(body.endpoint_id),
        )
        .await?;
    let minted = state
        .signaling
        .keys()
        .mint_organization_member(
            scope,
            m.principal.id(),
            endpoint.id,
            m.ctx.service_account_id(),
            m.ctx.credential_id(),
            state.clock.now(),
        )
        .map_err(|_| CallingError::Internal)?;
    tracing::info!(endpoint_id = %endpoint.id, "organization member signaling credential minted");
    Ok(respond(
        StatusCode::CREATED,
        SignalingCredentialView {
            endpoint_id: endpoint.id.as_uuid(),
            principal_id: m.principal.id().as_uuid(),
            token: minted.token.expose_secret().to_string(),
            expires_at: minted.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            signaling_url: "/v1/calling/signaling",
        },
        false,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum MemberStatus {
    Active,
    Suspended,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExtensionStatusBody {
    principal_id: Uuid,
    status: MemberStatus,
}

/// Whether `scope`'s tenant may be governed here: `Some(true)` for a FIRST_PARTY
/// organization tenant, `Some(false)` for any other projected tenant, `None` when the
/// tenant is not projected (then Calling holds nothing of it).
async fn organization_tenant(
    state: &AppState,
    scope: CallScope,
) -> Result<Option<bool>, CallingError> {
    let row = state
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT classification, slug FROM platform_tenants WHERE id = $1",
            [scope.tenant_id().as_uuid().into()],
        ))
        .await
        .map_err(|_| CallingError::ServiceUnavailable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let classification: String = row
        .try_get("", "classification")
        .map_err(|_| CallingError::Internal)?;
    let slug: String = row
        .try_get("", "slug")
        .map_err(|_| CallingError::Internal)?;
    Ok(Some(
        classification == "FIRST_PARTY" && is_organization_tenant_slug(&slug),
    ))
}

async fn extension_status(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<ExtensionStatusBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    caller.require_permission(InternalPermission::CallingOrganizationMemberOffboard)?;
    let body = parse_body(body)?;
    let d = delegated(&caller)?;
    if body.principal_id.is_nil() {
        return Err(CallingError::Validation("principalId is invalid."));
    }
    let scope = CallScope::of_delegated(&d);
    let change = match organization_tenant(&state, scope).await? {
        Some(false) => {
            tracing::warn!("member extension status refused outside an organization tenant");
            return Err(CallingError::InternalPermissionDenied);
        }
        // Never projected: Calling holds no extension of this scope.
        None => Default::default(),
        Some(true) => {
            state
                .enterprise
                .apply_member_extension_status(
                    scope,
                    body.principal_id,
                    body.status == MemberStatus::Active,
                    caller.context().correlation_id().as_str(),
                )
                .await?
        }
    };
    tracing::info!(
        disabled = change.disabled,
        restored = change.restored,
        not_restored = change.not_restored,
        "organization member extension status applied"
    );
    let mut value = serde_json::to_value(change).map_err(|_| CallingError::Internal)?;
    value["schemaVersion"] = json!(1);
    Ok(respond(StatusCode::OK, value, false))
}

//! The customer API of the Developer Platform (`/v1/**`, contract
//! `cfmeet.developer-platform.http-api`), reachable ONLY through the API Gateway.
//!
//! Every request carries a gateway service token (`developer-request.forward`) and
//! exactly one of:
//!
//! * `X-CF-User-Session` — the CF Meet human session the gateway transported verbatim;
//!   the Meeting Service verifies it (session bridge). Never a machine credential.
//! * a signed delegated context with the gateway-authenticated credential id — a machine
//!   caller, re-validated by Platform Access; its scopes are evaluated by Platform Access
//!   per operation.
//!
//! Disabled (404) unless `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`.

use axum::{
    extract::{FromRequestParts, Path, Query, Request, State},
    http::{request::Parts, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde_json::Value;
use uuid::Uuid;

use super::error::{ApiError, ErrorCode};
use crate::{
    control::{
        self, activity, commercial, entitlements, idempotency, members, organizations,
        pagination::{Page, PageQuery},
        projects, Caller, MachineCaller, Principal,
    },
    domain::{rbac::Action, scoped, CausationId, RequestContext},
    platform_access::ContextOutcome,
    session::SessionOutcome,
    state::AppState,
};

pub const USER_SESSION_HEADER: &str = "x-cf-user-session";

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(crate::webhooks::http::customer_routes())
        .route("/organizations", get(list_orgs).post(create_org))
        .route(
            "/organizations/:organization_id",
            get(get_org).patch(update_org),
        )
        .route("/organizations/:organization_id/close", post(close_org))
        .route("/organizations/:organization_id/members", get(list_members))
        .route(
            "/organizations/:organization_id/members/:member_id",
            axum::routing::patch(change_role).delete(remove_member),
        )
        .route(
            "/organizations/:organization_id/ownership-transfer",
            post(transfer),
        )
        .route(
            "/organizations/:organization_id/invitations",
            get(list_invitations).post(invite),
        )
        .route(
            "/organizations/:organization_id/invitations/:invitation_id/revoke",
            post(revoke_invitation),
        )
        .route("/invitations/accept", post(accept_invitation))
        .route(
            "/organizations/:organization_id/projects",
            get(list_projects).post(create_project),
        )
        .route(
            "/organizations/:organization_id/activity",
            get(activity_list),
        )
        .route(
            "/organizations/:organization_id/commercial",
            get(org_commercial),
        )
        .route(
            "/organizations/:organization_id/plan-assignments",
            get(plan_history),
        )
        .route(
            "/organizations/:organization_id/billing-account",
            axum::routing::put(put_billing),
        )
        .route("/commercial-plans", get(list_plans))
        .route(
            "/projects/:project_id",
            get(get_project).patch(update_project),
        )
        .route("/projects/:project_id/archive", post(archive_project))
        .route("/projects/:project_id/suspend", post(suspend_project))
        .route("/projects/:project_id/resume", post(resume_project))
        .route("/projects/:project_id/scopes", get(project_scopes))
        .route(
            "/projects/:project_id/environments",
            get(list_environments).post(create_environment),
        )
        .route("/environments/:environment_id", get(get_environment))
        .route(
            "/environments/:environment_id/suspend",
            post(suspend_environment),
        )
        .route(
            "/environments/:environment_id/resume",
            post(resume_environment),
        )
        .route(
            "/environments/:environment_id/entitlements",
            get(environment_entitlements),
        )
}

/// Authenticates the gateway hop and resolves the caller (human or machine).
pub async fn require_gateway_caller(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let fallback = RequestContext::generate();
    if !state.flags.gateway_api {
        return scoped(fallback, async {
            ApiError(ErrorCode::NotFound).into_response()
        })
        .await;
    }
    let ctx = match state
        .service_auth
        .verifier()
        .verify_headers(request.headers())
    {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::warn!(
                reason = e.failure.as_str(),
                "developer api: service authentication rejected"
            );
            return scoped(fallback, async {
                ApiError(ErrorCode::InternalAuthenticationFailed).into_response()
            })
            .await;
        }
    };
    let context = RequestContext {
        request_id: ctx.request_id().clone(),
        correlation_id: ctx.correlation_id().clone(),
        causation_id: Some(CausationId::of_request(ctx.request_id())),
    };
    let session = request
        .headers()
        .get(USER_SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let result = scoped(context.clone(), async {
        if ctx
            .require_permission(InternalPermission::DeveloperRequestForward)
            .is_err()
        {
            return Err(ApiError(ErrorCode::InternalPermissionDenied));
        }
        let principal = match (session, ctx.delegated_platform_context().cloned()) {
            (Some(_), Some(_)) => return Err(ApiError(ErrorCode::AuthenticationRequired)),
            (Some(token), None) => {
                match state.sessions.verify(&token, &context.correlation_id).await {
                    Ok(SessionOutcome::Verified(user)) => Principal::Human(user),
                    Ok(SessionOutcome::Rejected) => {
                        return Err(ApiError(ErrorCode::AuthenticationRequired))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "session authority unavailable");
                        return Err(ApiError(ErrorCode::ServiceUnavailable));
                    }
                }
            }
            (None, Some(delegated)) => {
                let Some(credential_id) = delegated.initiating_credential_id() else {
                    return Err(ApiError(ErrorCode::AuthenticationRequired));
                };
                match state
                    .platform_access
                    .trusted_context(&delegated, &context.correlation_id)
                    .await
                {
                    Ok(ContextOutcome::Confirmed(c))
                        if c.matches(&delegated) && c.authority_mode == "FULL" =>
                    {
                        // The credential itself must still authenticate NOW (revoked,
                        // expired or superseded since the gateway's check: refused).
                        match live_credential(
                            &state,
                            &delegated,
                            credential_id,
                            &context.correlation_id,
                        )
                        .await
                        {
                            Ok(true) => {}
                            Ok(false) => return Err(ApiError(ErrorCode::AuthenticationRequired)),
                            Err(e) => {
                                tracing::warn!(error = %e, "identity authority unavailable");
                                return Err(ApiError(ErrorCode::ServiceUnavailable));
                            }
                        }
                        Principal::Machine(MachineCaller {
                            context: c,
                            delegated,
                            credential_id,
                        })
                    }
                    Ok(_) => return Err(ApiError(ErrorCode::AuthenticationRequired)),
                    Err(e) => {
                        tracing::warn!(error = %e, "identity authority unavailable");
                        return Err(ApiError(ErrorCode::ServiceUnavailable));
                    }
                }
            }
            (None, None) => return Err(ApiError(ErrorCode::AuthenticationRequired)),
        };
        Ok(principal)
    })
    .await;
    match result {
        Ok(principal) => {
            request.extensions_mut().insert(Caller {
                principal,
                request: context.clone(),
            });
            let mut response = scoped(context.clone(), next.run(request)).await;
            if let Ok(v) = axum::http::HeaderValue::from_str(context.request_id.as_str()) {
                response.headers_mut().insert("x-request-id", v);
            }
            response
        }
        Err(e) => scoped(context, async { e.into_response() }).await,
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Caller>()
            .cloned()
            .ok_or(ApiError(ErrorCode::AuthenticationRequired))
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn page(q: &PageQuery) -> ApiResult<Page> {
    Page::from_query(q)
}

fn idem_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// `(status, body)` as a response.
fn reply((status, body): (StatusCode, Value)) -> Response {
    (status, Json(body)).into_response()
}

fn body_digest<T: serde::Serialize>(scope: &str, value: &T) -> String {
    idempotency::digest(&[
        scope.as_bytes(),
        &serde_json::to_vec(value).unwrap_or_default(),
    ])
}

async fn list_orgs(
    State(s): State<AppState>,
    c: Caller,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(organizations::list(&s, &c, &page(&q)?).await?))
}

async fn create_org(
    State(s): State<AppState>,
    c: Caller,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let parsed: organizations::CreateOrganization =
        serde_json::from_value(body.clone()).map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let scope = format!("{}:organizations.create", c.actor_ref());
    let digest = body_digest(&scope, &body);
    let key = idem_key(&headers);
    let out = idempotency::run(
        &s.db,
        &scope,
        key.as_deref(),
        &digest,
        idempotency::keep,
        || organizations::create(&s, &c, parsed),
    )
    .await?;
    Ok(reply(out))
}

async fn get_org(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(organizations::get(&s, &c, id).await?))
}

async fn update_org(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Json(body): Json<organizations::UpdateOrganization>,
) -> ApiResult<Json<Value>> {
    Ok(Json(organizations::update(&s, &c, id, body).await?))
}

async fn close_org(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(organizations::close(&s, &c, id).await?))
}

async fn list_members(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(members::list(&s, &c, id, &page(&q)?).await?))
}

async fn change_role(
    State(s): State<AppState>,
    c: Caller,
    Path((id, member)): Path<(Uuid, Uuid)>,
    Json(body): Json<members::ChangeRole>,
) -> ApiResult<Json<Value>> {
    Ok(Json(members::change_role(&s, &c, id, member, body).await?))
}

async fn remove_member(
    State(s): State<AppState>,
    c: Caller,
    Path((id, member)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    members::remove(&s, &c, id, member).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn transfer(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Json(body): Json<members::TransferOwnership>,
) -> ApiResult<Json<Value>> {
    Ok(Json(members::transfer_ownership(&s, &c, id, body).await?))
}

async fn list_invitations(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        members::list_invitations(&s, &c, id, &page(&q)?).await?,
    ))
}

async fn invite(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let parsed: members::CreateInvitation =
        serde_json::from_value(body.clone()).map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let scope = format!("{}:{id}:invitations.create", c.actor_ref());
    let digest = body_digest(&scope, &body);
    let key = idem_key(&headers);
    let out = idempotency::run(
        &s.db,
        &scope,
        key.as_deref(),
        &digest,
        members::redact_token,
        || members::invite(&s, &c, id, parsed),
    )
    .await?;
    Ok(reply(out))
}

async fn revoke_invitation(
    State(s): State<AppState>,
    c: Caller,
    Path((id, invitation)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        members::revoke_invitation(&s, &c, id, invitation).await?,
    ))
}

async fn accept_invitation(
    State(s): State<AppState>,
    c: Caller,
    Json(body): Json<members::AcceptInvitation>,
) -> ApiResult<Json<Value>> {
    Ok(Json(members::accept(&s, &c, body).await?))
}

async fn list_projects(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::list(&s, &c, id, &page(&q)?).await?))
}

async fn create_project(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let parsed: projects::CreateProject =
        serde_json::from_value(body.clone()).map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let scope = format!("{}:{id}:projects.create", c.actor_ref());
    let digest = body_digest(&scope, &body);
    let key = idem_key(&headers);
    let out = idempotency::run(
        &s.db,
        &scope,
        key.as_deref(),
        &digest,
        idempotency::keep,
        || projects::create(&s, &c, id, parsed),
    )
    .await?;
    Ok(reply(out))
}

async fn get_project(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::get(&s, &c, id).await?))
}

async fn update_project(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Json(body): Json<projects::UpdateProject>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::update(&s, &c, id, body).await?))
}

async fn archive_project(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::set_status(&s, &c, id, "ARCHIVED").await?))
}

async fn suspend_project(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::set_status(&s, &c, id, "SUSPENDED").await?))
}

async fn resume_project(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::set_status(&s, &c, id, "ACTIVE").await?))
}

async fn project_scopes(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::scopes(&s, &c, id).await?))
}

async fn list_environments(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        projects::list_environments(&s, &c, id, &page(&q)?).await?,
    ))
}

async fn create_environment(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let parsed: projects::CreateEnvironment =
        serde_json::from_value(body.clone()).map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let scope = format!("{}:{id}:environments.create", c.actor_ref());
    let digest = body_digest(&scope, &body);
    let key = idem_key(&headers);
    let out = idempotency::run(
        &s.db,
        &scope,
        key.as_deref(),
        &digest,
        idempotency::keep,
        || projects::create_environment(&s, &c, id, parsed),
    )
    .await?;
    Ok(reply(out))
}

async fn get_environment(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(projects::get_environment(&s, &c, id).await?))
}

async fn suspend_environment(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        projects::set_environment_status(&s, &c, id, true).await?,
    ))
}

async fn resume_environment(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        projects::set_environment_status(&s, &c, id, false).await?,
    ))
}

async fn environment_entitlements(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let tenant: Uuid =
        s.db.query_one(control::stmt(
            "SELECT tenant_id FROM devplat_environment_profiles WHERE environment_id = $1",
            vec![id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?
        .try_get("", "tenant_id")?;
    control::authorize(&s, &c, tenant, Action::LimitsRead).await?;
    Ok(Json(entitlements::effective(&s.db, id, None).await?))
}

async fn activity_list(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    control::authorize(&s, &c, id, Action::ActivityRead).await?;
    Ok(Json(activity::list(&s.db, id, &page(&q)?).await?))
}

async fn org_commercial(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(commercial::organization_commercial(&s, &c, id).await?))
}

async fn plan_history(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        commercial::assignment_history(&s, &c, id, &page(&q)?).await?,
    ))
}

async fn put_billing(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Json(body): Json<commercial::PutBillingAccount>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        commercial::put_billing_account(&s, &c, id, body).await?,
    ))
}

async fn list_plans(State(s): State<AppState>, c: Caller) -> ApiResult<Json<Value>> {
    c.human()?;
    Ok(Json(commercial::list_plans(&s.db).await?))
}

use sea_orm::ConnectionTrait;

/// Whether the gateway-forwarded credential still authenticates (Platform Access
/// `/authorization/evaluate` with no required scope checks the credential's live state).
pub(crate) async fn live_credential(
    state: &AppState,
    delegated: &cf_service_auth::DelegatedPlatformContext,
    credential_id: uuid::Uuid,
    correlation: &cf_service_auth::CorrelationId,
) -> Result<bool, crate::platform_access::AuthorityUnavailable> {
    state
        .platform_access
        .credential_allows(delegated, correlation, credential_id, &[])
        .await
}

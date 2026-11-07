//! Organization endpoints.
//!
//! * Legacy organization directory (`legacy` / `shadow` mode): meeting-service's own tables,
//!   the membership authority for ORGANIZATION_ONLY.
//! * Blueprint Section 4 (`authoritative` mode, after the gated cutover): the same legacy
//!   routes become authenticated facades over the Organizations Service (never a second
//!   writer), and the first-party organization routes relay to it with the verified acting
//!   user. In any other mode the first-party routes answer `FEATURE_DISABLED`.

use axum::{
    body::Bytes,
    extract::{RawQuery, State},
    http::{header::CACHE_CONTROL, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{
    domain::{OrganizationMemberStatus, OrganizationStatus},
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    models::{organization_memberships, organizations},
    services::{
        actor::{ActorContext, RegisteredActor},
        meeting_core::{permission_denied, validation},
        organizations_authority::{
            legacy_role, relayed_error, unavailable, ActingUser, AuthorityError, AuthorityMember,
            Call, OrganizationSnapshot, Reply, SharedOrganizationsAuthority,
        },
    },
    state::AppState,
};

fn org(o: &organizations::Model) -> OrganizationResponse {
    OrganizationResponse {
        id: o.id,
        name: o.name.clone(),
        slug: o.slug.clone(),
        status: o.status,
        created_at: o.created_at,
    }
}

fn member(m: &organization_memberships::Model) -> OrganizationMemberResponse {
    OrganizationMemberResponse {
        id: m.id,
        organization_id: m.organization_id,
        user_id: m.user_id,
        role: m.role,
        status: m.status,
        created_at: m.created_at,
        updated_at: m.updated_at,
        organization_role: None,
        version: None,
    }
}

// ---------------------------------------------------------------------------------------
// Organizations Service plumbing
// ---------------------------------------------------------------------------------------

/// The Organizations integration when it is authoritative.
fn authoritative(state: &AppState) -> Option<&SharedOrganizationsAuthority> {
    state
        .meeting
        .core
        .organization_authority
        .as_ref()
        .filter(|a| a.is_authoritative())
}

fn require_authoritative(state: &AppState) -> ApiResult<&SharedOrganizationsAuthority> {
    authoritative(state).ok_or_else(|| {
        ApiError::domain(
            ErrorCode::FeatureDisabled,
            "First-party organization management is not enabled",
        )
    })
}

fn authority_error(e: AuthorityError) -> ApiError {
    match e {
        AuthorityError::Refused(reply) => relayed_error(&reply),
        AuthorityError::Unavailable(_) => unavailable(),
    }
}

/// The client's `Idempotency-Key` (visible ASCII, at most 128 characters).
fn idempotency_key(headers: &HeaderMap) -> ApiResult<Option<String>> {
    headers
        .get("idempotency-key")
        .map(|v| {
            v.to_str()
                .ok()
                .filter(|s| {
                    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
                })
                .map(str::to_string)
                .ok_or_else(|| validation("Idempotency-Key must be 1-128 visible ASCII characters"))
        })
        .transpose()
}

/// A JSON object body, or `None` for an empty body.
fn json_object(body: &Bytes) -> ApiResult<Option<Value>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        _ => Err(validation("The request body must be a JSON object")),
    }
}

/// Query pairs restricted to `allowed` keys (anything else is a validation error).
fn query_pairs(raw: Option<&str>, allowed: &[&str]) -> ApiResult<Vec<(String, String)>> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(vec![]);
    };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    for (k, v) in &pairs {
        if !allowed.contains(&k.as_str()) || v.len() > 256 {
            return Err(validation(format!("Unsupported query parameter `{k}`")));
        }
    }
    Ok(pairs)
}

/// A relayed success answer, unchanged (`Cache-Control: no-store`).
fn relayed_success(reply: Reply) -> Response {
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK);
    let mut response = if status == StatusCode::NO_CONTENT {
        status.into_response()
    } else {
        (status, Json(reply.body)).into_response()
    };
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(v) = reply
        .replayed
        .as_deref()
        .and_then(|v| HeaderValue::from_str(v).ok())
    {
        response.headers_mut().insert("idempotent-replayed", v);
    }
    response
}

/// The caller of a first-party organization route: a registered account whose session
/// revocation status was checked.
fn relay_actor(actor: &ActorContext) -> ApiResult<&RegisteredActor> {
    let user = actor.require_registered()?;
    if !user.revocation_checked {
        return Err(ApiError::domain(
            ErrorCode::RevocationCheckUnavailable,
            "Session revocation status is temporarily unavailable; retry later",
        ));
    }
    Ok(user)
}

struct Relay<'a> {
    method: Method,
    path: String,
    headers: &'a HeaderMap,
    body: Option<&'a Bytes>,
    raw_query: Option<&'a str>,
    allowed_query: &'a [&'a str],
}

/// Relays one first-party organization operation for the verified caller (acting user
/// and verified email from the account authority; only `Idempotency-Key` is forwarded
/// from the request). An organization document in the answer refreshes the local
/// read-only projection.
async fn relay(state: &AppState, actor: &ActorContext, r: Relay<'_>) -> ApiResult<Response> {
    let authority = require_authoritative(state)?;
    let user = relay_actor(actor)?;
    let query = query_pairs(r.raw_query, r.allowed_query)?;
    let body = match r.body {
        Some(b) => json_object(b)?,
        None => None,
    };
    let body = match (body, &r.method) {
        (Some(b), _) => Some(b),
        (None, &Method::POST) | (None, &Method::PATCH) => Some(json!({})),
        (None, _) => None,
    };
    let acting = ActingUser::of(user);
    let mut call = Call::new(r.method, r.path)
        .actor(&acting)
        .query(query)
        .idempotency_key(idempotency_key(r.headers)?);
    call.body = body;
    let reply = authority.send(&call).await.map_err(|_| unavailable())?;
    if !reply.is_success() {
        return Err(relayed_error(&reply));
    }
    authority.project_document(&state.db, &reply.body).await;
    Ok(relayed_success(reply))
}

fn legacy_member(m: &AuthorityMember) -> ApiResult<OrganizationMemberResponse> {
    Ok(OrganizationMemberResponse {
        id: m.id,
        organization_id: m.organization_id,
        user_id: m.user_id,
        role: legacy_role(&m.role),
        status: OrganizationMemberStatus::parse(&m.status).ok_or_else(unavailable)?,
        created_at: m.created_at,
        updated_at: m.updated_at,
        organization_role: Some(m.role.clone()),
        version: m.version,
    })
}

fn legacy_organization(s: &OrganizationSnapshot) -> OrganizationResponse {
    OrganizationResponse {
        id: s.id,
        name: s.name.clone(),
        slug: s.slug.clone(),
        status: if s.lifecycle_status == "ACTIVE" {
            OrganizationStatus::Active
        } else {
            OrganizationStatus::Suspended
        },
        created_at: s.created_at,
    }
}

/// A platform-administrator facade write relayed to `/internal/v1/admin/organizations…`.
async fn admin_facade(
    state: &AppState,
    authority: &SharedOrganizationsAuthority,
    actor: &ActorContext,
    method: Method,
    path: String,
    body: Value,
    headers: &HeaderMap,
) -> ApiResult<OrganizationResponse> {
    let user = actor.require_registered()?;
    if !user.platform_admin {
        return Err(permission_denied("Platform administrator role required"));
    }
    let acting = ActingUser::of(user);
    let reply = authority
        .send(
            &Call::new(method, path)
                .actor(&acting)
                .body(body)
                .idempotency_key(idempotency_key(headers)?),
        )
        .await
        .map_err(|_| unavailable())?;
    if !reply.is_success() {
        return Err(relayed_error(&reply));
    }
    let snapshot = OrganizationSnapshot::from_json(&reply.body).ok_or_else(unavailable)?;
    authority.project(&state.db, &snapshot).await?;
    Ok(legacy_organization(&snapshot))
}

// ---------------------------------------------------------------------------------------
// Legacy routes (facades once Organizations is authoritative)
// ---------------------------------------------------------------------------------------

/// Create an organization (platform administrators).
///
/// Creates an ACTIVE organization with a unique `slug` (lowercase letters, digits and `-`) and a
/// name of 1–200 characters, optionally making `ownerUserId` its first OWNER. Requires a platform
/// administrator bearer token. Once the Organizations Service is authoritative this is relayed to
/// its operator create (creation source PLATFORM_ADMIN, the administrator recorded as creator)
/// and the response keeps this shape.
#[utoipa::path(
    post, path = "/api/v2/admin/organizations", tag = "Organizations (v2)", operation_id = "v2_create_organization",
    request_body = CreateOrganizationRequest,
    responses(
        (status = 201, description = "Created", body = OrganizationResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "SLUG_TAKEN (authoritative mode)", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE (Organizations unreachable)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<CreateOrganizationRequest>,
) -> ApiResult<(StatusCode, Json<OrganizationResponse>)> {
    if let Some(authority) = authoritative(&state) {
        let mut body = json!({ "name": req.name, "slug": req.slug });
        if let Some(owner) = req.owner_user_id {
            body["ownerUserId"] = json!(owner);
        }
        let o = admin_facade(
            &state,
            authority,
            &actor,
            Method::POST,
            "/internal/v1/admin/organizations".into(),
            body,
            &headers,
        )
        .await?;
        return Ok((StatusCode::CREATED, Json(o)));
    }
    let o = state
        .meeting
        .organizations
        .create(&actor, &req.name, &req.slug, req.owner_user_id)
        .await?;
    Ok((StatusCode::CREATED, Json(org(&o))))
}

/// Suspend or reactivate an organization (platform administrators).
///
/// Sets the organization's status to ACTIVE or SUSPENDED. While an organization is suspended, its
/// own OWNERs and ADMINs can no longer list or manage its members. Requires a platform
/// administrator bearer token. Once the Organizations Service is authoritative this is relayed to
/// its operator lifecycle change and the response keeps this shape.
#[utoipa::path(
    patch, path = "/api/v2/admin/organizations/{id}", tag = "Organizations (v2)", operation_id = "v2_update_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = UpdateOrganizationRequest,
    responses(
        (status = 200, description = "Updated", body = OrganizationResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "MEETING_NOT_FOUND / ORGANIZATION_NOT_FOUND (authoritative mode)", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE (Organizations unreachable)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<UpdateOrganizationRequest>,
) -> ApiResult<Json<OrganizationResponse>> {
    if let Some(authority) = authoritative(&state) {
        let o = admin_facade(
            &state,
            authority,
            &actor,
            Method::PATCH,
            format!("/internal/v1/admin/organizations/{id}"),
            json!({ "status": req.status.as_str() }),
            &headers,
        )
        .await?;
        return Ok(Json(o));
    }
    let o = state
        .meeting
        .organizations
        .set_status(&actor, id, req.status)
        .await?;
    Ok(Json(org(&o)))
}

/// List members (platform admins, organization OWNER/ADMIN).
///
/// Returns every membership of the organization, whatever its status, oldest first. Allowed for
/// platform administrators and for active OWNER or ADMIN members of an active organization. Once
/// the Organizations Service is authoritative, the list comes from it (removed memberships are
/// omitted; members holding `organization.members.read` may list) and each item also carries
/// `organizationRole` and `version`.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/members", tag = "Organizations (v2)", operation_id = "v2_list_organization_members",
    params(("id" = Uuid, Path, description = "Organization id")),
    responses(
        (status = 200, description = "Members", body = [OrganizationMemberResponse]),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "MEETING_NOT_FOUND / ORGANIZATION_NOT_FOUND (authoritative mode)", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE (Organizations unreachable)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_organization_members(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<Vec<OrganizationMemberResponse>>> {
    if let Some(authority) = authoritative(&state) {
        let user = actor.require_registered()?;
        let acting = ActingUser::of(user);
        // Platform administrators read the authoritative list as a service read; everyone
        // else reads as themselves (Organizations checks organization.members.read).
        let as_actor = (!user.platform_admin).then_some(&acting);
        let members = authority
            .members(id, as_actor)
            .await
            .map_err(authority_error)?;
        let mut list = members
            .iter()
            .map(legacy_member)
            .collect::<ApiResult<Vec<_>>>()?;
        list.sort_by_key(|m| (m.created_at, m.id));
        return Ok(Json(list));
    }
    let list = state.meeting.organizations.list_members(&actor, id).await?;
    Ok(Json(list.iter().map(member).collect()))
}

/// Add or change a membership (platform admins, organization OWNER/ADMIN within limits).
///
/// Creates or updates the user's role and status in the organization; status REMOVED ends an
/// existing membership. Platform administrators and OWNERs may set any role, while ADMINs may only
/// manage MEMBER memberships. Except for platform administrators, callers cannot change their own
/// membership. Once the Organizations Service is authoritative, an existing member's change is
/// relayed to it (its role rules apply) and a user who is not yet a member gets
/// `409 INVITATION_REQUIRED`: new members join by invitation.
#[utoipa::path(
    put, path = "/api/v2/organizations/{id}/members/{userId}", tag = "Organizations (v2)", operation_id = "v2_upsert_organization_member",
    params(("id" = Uuid, Path, description = "Organization id"), ("userId" = Uuid, Path, description = "User id")),
    request_body = UpsertOrganizationMemberRequest,
    responses(
        (status = 200, description = "Membership", body = OrganizationMemberResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "INVITATION_REQUIRED / LAST_OWNER (authoritative mode)", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE (Organizations unreachable)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn upsert_organization_member(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, user_id)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<UpsertOrganizationMemberRequest>,
) -> ApiResult<Json<OrganizationMemberResponse>> {
    if let Some(authority) = authoritative(&state) {
        let user = actor.require_registered()?;
        let acting = ActingUser::of(user);
        let members = authority
            .members(id, Some(&acting))
            .await
            .map_err(authority_error)?;
        let Some(existing) = members
            .iter()
            .find(|m| m.user_id == user_id && m.status != "REMOVED")
        else {
            return Err(ApiError::domain(
                ErrorCode::InvitationRequired,
                "New members join an organization by invitation; send an invitation instead",
            ));
        };
        let version = existing.version.ok_or_else(unavailable)?;
        let mut body = json!({ "expectedVersion": version });
        // MANAGER and GUEST read as MEMBER here: re-sending MEMBER keeps their role.
        if legacy_role(&existing.role) != req.role {
            body["role"] = json!(req.role.as_str());
        }
        if existing.status != req.status.as_str() {
            body["status"] = json!(req.status.as_str());
        }
        if body.as_object().is_some_and(|o| o.len() == 1) {
            return Ok(Json(legacy_member(existing)?));
        }
        let reply = authority
            .send(
                &Call::new(
                    Method::PATCH,
                    format!("/internal/v1/organizations/{id}/members/{}", existing.id),
                )
                .actor(&acting)
                .body(body),
            )
            .await
            .map_err(|_| unavailable())?;
        if !reply.is_success() {
            return Err(relayed_error(&reply));
        }
        let changed = AuthorityMember::from_json(&reply.body).ok_or_else(unavailable)?;
        return Ok(Json(legacy_member(&changed)?));
    }
    let m = state
        .meeting
        .organizations
        .upsert_member(&actor, id, user_id, req.role, req.status)
        .await?;
    Ok(Json(member(&m)))
}

/// Organizations the caller verifiably belongs to.
///
/// Returns the organizations in which the signed-in user has an active, directory-verified
/// membership, each with the user's role. Requires a user bearer token. Once the Organizations
/// Service is authoritative the list comes from it (active, non-GUEST memberships of active
/// organizations) and each item also carries `organizationRole`.
#[utoipa::path(
    get, path = "/api/v2/me/organizations", tag = "Organizations (v2)", operation_id = "v2_my_organizations",
    responses(
        (status = 200, description = "Organizations", body = [MyOrganizationResponse]),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE (Organizations unreachable)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn my_organizations(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<Vec<MyOrganizationResponse>>> {
    if let Some(authority) = authoritative(&state) {
        let user = actor.require_registered()?;
        let acting = ActingUser::of(user);
        let items: Vec<_> = authority
            .user_organizations(&acting)
            .await
            .map_err(authority_error)?
            .into_iter()
            .filter(|i| i.grants_meeting_membership())
            .collect();
        let ids: Vec<Uuid> = items.iter().map(|i| i.organization_id).collect();
        let rows = state
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id, created_at FROM organizations WHERE id = ANY($1)",
                [ids.into()],
            ))
            .await?;
        let mut created = std::collections::HashMap::new();
        for r in rows {
            let id: Uuid = r.try_get("", "id")?;
            let at: chrono::DateTime<chrono::Utc> = r.try_get("", "created_at")?;
            created.insert(id, at);
        }
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let created_at = match created.get(&item.organization_id) {
                Some(at) => {
                    // Keep the projection's name and status current (best effort).
                    let _ = state
                        .db
                        .execute(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "UPDATE organizations SET name = $2, status = 'ACTIVE' \
                             WHERE id = $1 AND (name <> $2 OR status <> 'ACTIVE')",
                            [
                                item.organization_id.into(),
                                item.name.chars().take(200).collect::<String>().into(),
                            ],
                        ))
                        .await;
                    *at
                }
                None => {
                    authority
                        .ensure_projection(&state.db, item.organization_id)
                        .await?
                        .created_at
                }
            };
            out.push(MyOrganizationResponse {
                organization: OrganizationResponse {
                    id: item.organization_id,
                    name: item.name.clone(),
                    slug: item.slug.clone(),
                    status: OrganizationStatus::Active,
                    created_at,
                },
                role: legacy_role(&item.role),
                organization_role: Some(item.role.clone()),
            });
        }
        return Ok(Json(out));
    }
    let list = state.meeting.organizations.mine(&actor).await?;
    Ok(Json(
        list.iter()
            .map(|(o, role)| MyOrganizationResponse {
                organization: org(o),
                role: *role,
                organization_role: None,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------------------
// First-party organization routes (authoritative mode only)
// ---------------------------------------------------------------------------------------

/// Create a business organization (the caller becomes OWNER).
///
/// Creates a first-party business organization in the Organizations Service with the signed-in
/// user as its OWNER (creation source SELF_SERVICE; at most 5 per user per 24 hours). This is the
/// CF Meet business organization, not a developer-platform API workspace. Send an
/// `Idempotency-Key` header to make retries safe. Returns FEATURE_DISABLED until the Organizations
/// Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations", tag = "Organizations (v2)", operation_id = "v2_create_business_organization",
    request_body = CreateBusinessOrganizationRequest,
    responses(
        (status = 201, description = "Created", body = BusinessOrganization),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "SLUG_TAKEN / IDEMPOTENCY_CONFLICT", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_business_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: "/internal/v1/organizations".into(),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Get a business organization.
///
/// Returns the organization with its profile, member count and the caller's own membership
/// (`viewer`: role, status and capabilities). Organizations the caller does not belong to answer
/// ORGANIZATION_NOT_FOUND, exactly like unknown ones. Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}", tag = "Organizations (v2)", operation_id = "v2_get_business_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    responses(
        (status = 200, description = "Organization", body = BusinessOrganization),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "MEMBERSHIP_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_business_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::GET,
            path: format!("/internal/v1/organizations/{id}"),
            headers: &headers,
            body: None,
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Update a business organization's name, slug or profile.
///
/// Changes the name, slug and profile fields given (an omitted profile field is unchanged and
/// `null` clears it). `expectedVersion` must be the organization's current `version`, otherwise
/// VERSION_CONFLICT. Requires the `organization.profile.write` capability (OWNER, ADMIN). Returns
/// FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    patch, path = "/api/v2/organizations/{id}", tag = "Organizations (v2)", operation_id = "v2_update_business_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = UpdateBusinessOrganizationRequest,
    responses(
        (status = 200, description = "Updated organization", body = BusinessOrganization),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / SLUG_TAKEN / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_business_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::PATCH,
            path: format!("/internal/v1/organizations/{id}"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Change a member's role or status.
///
/// Changes the role (OWNER, ADMIN, MANAGER, MEMBER, GUEST) or status (ACTIVE, SUSPENDED, REMOVED)
/// of one membership. The path's member segment is the membership `id` from the member list.
/// `expectedVersion` must be the membership's current `version`. The Organizations Service
/// enforces the role rules: OWNERs manage anyone, ADMINs manage MANAGER, MEMBER and GUEST
/// memberships, nobody raises their own role, and the last OWNER cannot be removed or demoted
/// (LAST_OWNER). Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    patch, path = "/api/v2/organizations/{id}/members/{userId}", tag = "Organizations (v2)", operation_id = "v2_update_organization_membership",
    params(("id" = Uuid, Path, description = "Organization id"), ("userId" = Uuid, Path, description = "Membership id (the `id` of a member list item)")),
    request_body = UpdateBusinessMembershipRequest,
    responses(
        (status = 200, description = "Updated membership", body = BusinessOrganizationMembership),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / LAST_OWNER", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_organization_membership(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, membership_id)): V2Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::PATCH,
            path: format!("/internal/v1/organizations/{id}/members/{membership_id}"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Leave an organization.
///
/// Ends the signed-in user's own membership. The last OWNER cannot leave (LAST_OWNER): transfer
/// ownership first. Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/members/me/leave", tag = "Organizations (v2)", operation_id = "v2_leave_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body(content = Object, content_type = "application/json", description = "Optional empty JSON object"),
    responses(
        (status = 200, description = "The ended membership", body = BusinessOrganizationMembership),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "LAST_OWNER", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn leave_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/members/me/leave"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Transfer organization ownership.
///
/// Atomically makes the membership `toMembershipId` an OWNER and the calling OWNER an ADMIN.
/// `expectedVersion` must be the organization's current `version`. Requires the
/// `organization.ownership.transfer` capability (OWNER). Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/ownership-transfer", tag = "Organizations (v2)", operation_id = "v2_transfer_organization_ownership",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = OrganizationOwnershipTransferRequest,
    responses(
        (status = 200, description = "Transfer result", body = Object),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn transfer_organization_ownership(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/ownership-transfer"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Invite someone to an organization by email.
///
/// Creates a PENDING invitation for `email` with `role` and returns it with its one-time `token`
/// (shown only in this response; only the token's hash is stored). Only an account whose verified
/// email equals the invited address can accept it. Requires the `organization.members.invite`
/// capability (OWNER, ADMIN, MANAGER). Send an `Idempotency-Key` header to make retries safe.
/// Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/invitations", tag = "Organizations (v2)", operation_id = "v2_create_organization_invitation",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = CreateOrganizationInvitationRequest,
    responses(
        (status = 201, description = "Invitation with its one-time token", body = OrganizationInvitation),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "ORGANIZATION_SUSPENDED / IDEMPOTENCY_CONFLICT", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_organization_invitation(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/invitations"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// List an organization's invitations.
///
/// Returns the organization's invitations (masked invitee email, never a token), optionally
/// filtered by `status`. Requires the `organization.members.invite` capability. Returns
/// FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/invitations", tag = "Organizations (v2)", operation_id = "v2_list_organization_invitations",
    params(
        ("id" = Uuid, Path, description = "Organization id"),
        ("status" = Option<String>, Query, description = "PENDING, ACCEPTED, DECLINED, REVOKED or EXPIRED"),
    ),
    responses(
        (status = 200, description = "items: invitations", body = Object),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_organization_invitations(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::GET,
            path: format!("/internal/v1/organizations/{id}/invitations"),
            headers: &headers,
            body: None,
            raw_query: query.as_deref(),
            allowed_query: &["status"],
        },
    )
    .await
}

/// Revoke an organization invitation.
///
/// Revokes a PENDING invitation so its token can no longer be accepted (idempotent: revoking a
/// revoked invitation succeeds). Requires the `organization.members.invite` capability. Returns
/// FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/invitations/{invitationId}/revoke", tag = "Organizations (v2)", operation_id = "v2_revoke_organization_invitation",
    params(("id" = Uuid, Path, description = "Organization id"), ("invitationId" = Uuid, Path, description = "Invitation id")),
    request_body(content = Object, content_type = "application/json", description = "Optional empty JSON object"),
    responses(
        (status = 200, description = "Revoked invitation", body = OrganizationInvitation),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / INVITATION_INVALID / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_organization_invitation(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, invitation_id)): V2Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/invitations/{invitation_id}/revoke"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Accept an organization invitation.
///
/// Joins the organization with the invitation's role. The signed-in account must have a verified
/// email equal to the invited address (the account's verified email is sent by the server, never
/// taken from the request). An expired invitation answers INVITE_EXPIRED; an unknown, used or
/// revoked token answers INVITATION_INVALID. Returns FEATURE_DISABLED until the Organizations
/// Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organization-invitations/accept", tag = "Organizations (v2)", operation_id = "v2_accept_organization_invitation",
    request_body = OrganizationInvitationTokenRequest,
    responses(
        (status = 200, description = "Membership created by the invitation", body = Object),
        (status = 403, description = "PERMISSION_DENIED (verified email does not match)", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 410, description = "INVITATION_INVALID / INVITE_EXPIRED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn accept_organization_invitation(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: "/internal/v1/organization-invitations/accept".into(),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Decline an organization invitation.
///
/// Declines a PENDING invitation addressed to the signed-in user so it can no longer be accepted.
/// Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organization-invitations/decline", tag = "Organizations (v2)", operation_id = "v2_decline_organization_invitation",
    request_body = OrganizationInvitationTokenRequest,
    responses(
        (status = 200, description = "Declined invitation", body = Object),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 410, description = "INVITATION_INVALID / INVITE_EXPIRED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn decline_organization_invitation(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: "/internal/v1/organization-invitations/decline".into(),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

async fn lifecycle(
    state: AppState,
    actor: ActorContext,
    headers: HeaderMap,
    id: Uuid,
    body: Bytes,
    action: &str,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/{action}"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Suspend a business organization (OWNER).
///
/// Suspends an ACTIVE organization: no member keeps any capability except that OWNERs can still
/// read it, so organization meetings, invitations and colleague lists stop. Requires the
/// `organization.lifecycle.manage` capability (OWNER). Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/suspend", tag = "Organizations (v2)", operation_id = "v2_suspend_business_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = OrganizationLifecycleRequest,
    responses(
        (status = 200, description = "Suspended organization", body = BusinessOrganization),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn suspend_business_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    lifecycle(state, actor, headers, id, body, "suspend").await
}

/// Reactivate a business organization (OWNER).
///
/// Reactivates an organization its OWNERs suspended (a suspension imposed by an operator can only
/// be lifted by an operator). Requires the `organization.lifecycle.manage` capability (OWNER).
/// Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/reactivate", tag = "Organizations (v2)", operation_id = "v2_reactivate_business_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = OrganizationLifecycleRequest,
    responses(
        (status = 200, description = "Reactivated organization", body = BusinessOrganization),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn reactivate_business_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    lifecycle(state, actor, headers, id, body, "reactivate").await
}

/// Archive a business organization (OWNER).
///
/// Archives an organization its OWNERs suspended; archiving is final for members (only OWNERs can
/// still read it) and existing organization meetings lose organization access. Requires the
/// `organization.lifecycle.manage` capability (OWNER). Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/archive", tag = "Organizations (v2)", operation_id = "v2_archive_business_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = OrganizationLifecycleRequest,
    responses(
        (status = 200, description = "Archived organization", body = BusinessOrganization),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / ORGANIZATION_SUSPENDED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn archive_business_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    lifecycle(state, actor, headers, id, body, "archive").await
}

/// An organization's audit history.
///
/// Returns the organization's audit events, newest first (`before` = the `seq` of the oldest
/// event already seen; `limit` 1–200, default 50): who changed what, when, and through which
/// service. Requires the `organization.audit.read` capability (OWNER, ADMIN). Returns
/// FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/audit-events", tag = "Organizations (v2)", operation_id = "v2_list_organization_audit_events",
    params(
        ("id" = Uuid, Path, description = "Organization id"),
        ("before" = Option<i64>, Query, description = "Return events older than this sequence number"),
        ("limit" = Option<i64>, Query, description = "1–200 (default 50)"),
    ),
    responses(
        (status = 200, description = "items: audit events", body = Object),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_organization_audit_events(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::GET,
            path: format!("/internal/v1/organizations/{id}/audit-events"),
            headers: &headers,
            body: None,
            raw_query: query.as_deref(),
            allowed_query: &["before", "limit"],
        },
    )
    .await
}

/// An organization's technical provisioning state.
///
/// Returns the truthful provisioning state of the organization's technical scope (NOT_REQUIRED,
/// PENDING, IN_PROGRESS, READY or FAILED), the current step and, on failure, the failure class.
/// Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/provisioning", tag = "Organizations (v2)", operation_id = "v2_get_organization_provisioning",
    params(("id" = Uuid, Path, description = "Organization id")),
    responses(
        (status = 200, description = "Provisioning state", body = Object),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_organization_provisioning(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::GET,
            path: format!("/internal/v1/organizations/{id}/provisioning"),
            headers: &headers,
            body: None,
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// List all business organizations (platform administrators).
///
/// Returns every first-party organization in id order (`cursor` = the previous page's
/// `nextCursor`; `limit` 1–200, default 50), optionally filtered by `status` (ACTIVE, SUSPENDED or
/// ARCHIVED). Requires a platform administrator bearer token. Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/admin/organizations", tag = "Organizations (v2)", operation_id = "v2_admin_list_organizations",
    params(
        ("cursor" = Option<Uuid>, Query, description = "The previous page's nextCursor"),
        ("limit" = Option<i64>, Query, description = "1–200 (default 50)"),
        ("status" = Option<String>, Query, description = "ACTIVE, SUSPENDED or ARCHIVED"),
    ),
    responses(
        (status = 200, description = "Organizations", body = BusinessOrganizationPage),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_list_organizations(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    require_authoritative(&state)?;
    if !relay_actor(&actor)?.platform_admin {
        return Err(permission_denied("Platform administrator role required"));
    }
    relay(
        &state,
        &actor,
        Relay {
            method: Method::GET,
            path: "/internal/v1/admin/organizations".into(),
            headers: &headers,
            body: None,
            raw_query: query.as_deref(),
            allowed_query: &["cursor", "limit", "status"],
        },
    )
    .await
}

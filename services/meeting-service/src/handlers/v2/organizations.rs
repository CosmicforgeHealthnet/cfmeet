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
use sea_orm::EntityTrait;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{
    domain::{OrganizationMemberStatus, OrganizationStatus},
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    models::{organization_memberships, organizations, users},
    services::{
        actor::{ActorContext, RegisteredActor},
        display_identity,
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
        display_name: None,
        username: None,
        email: None,
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
    relay_reply(state, actor, r).await.map(relayed_success)
}

/// [`relay`] returning the authoritative answer for post-processing.
async fn relay_reply(state: &AppState, actor: &ActorContext, r: Relay<'_>) -> ApiResult<Reply> {
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
    Ok(reply)
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
        display_name: None,
        username: None,
        email: None,
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

/// ORG-006 member-list paging: `limit` (1–200, default 50 when only `cursor` is given) and
/// `cursor` (the previous page's `X-Next-Cursor`). `None` when neither is given: the whole
/// list, exactly as before paging existed. Other query parameters are ignored, as before.
fn member_page(raw: Option<&str>) -> ApiResult<Option<(Option<(i64, Uuid)>, usize)>> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    let (mut cursor, mut limit) = (None, None);
    for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
        match k.as_ref() {
            "cursor" => {
                cursor = Some(
                    v.split_once('.')
                        .and_then(|(t, id)| {
                            Some((t.parse::<i64>().ok()?, Uuid::parse_str(id).ok()?))
                        })
                        .ok_or_else(|| {
                            validation("cursor must be a previous page's X-Next-Cursor")
                        })?,
                )
            }
            "limit" => {
                limit = Some(
                    v.parse::<usize>()
                        .ok()
                        .filter(|l| (1..=200).contains(l))
                        .ok_or_else(|| validation("limit must be between 1 and 200"))?,
                )
            }
            _ => {}
        }
    }
    if cursor.is_none() && limit.is_none() {
        return Ok(None);
    }
    Ok(Some((cursor, limit.unwrap_or(50))))
}

/// The opaque member-list position of `m` (oldest-first order: creation time, then id).
fn member_cursor(m: &OrganizationMemberResponse) -> (i64, Uuid) {
    (m.created_at.timestamp_micros(), m.id)
}

/// List members (platform admins, organization OWNER/ADMIN).
///
/// Returns the organization's memberships, oldest first, each with the member's display identity
/// (`displayName` and `username` from the account authority; never an email address). Allowed for
/// platform administrators and for active OWNER or ADMIN members of an active organization. Once
/// the Organizations Service is authoritative, the list comes from it (removed memberships are
/// omitted; members holding `organization.members.read` may list, following the organization's
/// member-directory policy) and each item also carries `organizationRole` and `version`.
///
/// Paging (ORG-006): without `limit` or `cursor` every membership is returned, as before. With
/// `limit` (1–200) and/or `cursor`, one page is returned (50 items when only `cursor` is given)
/// and, while more remain, the `X-Next-Cursor` response header holds the opaque cursor of the
/// next page. The body stays a JSON array in both cases.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/members", tag = "Organizations (v2)", operation_id = "v2_list_organization_members",
    params(
        ("id" = Uuid, Path, description = "Organization id"),
        ("limit" = Option<i64>, Query, description = "Page size 1–200; omit both `limit` and `cursor` for the whole list"),
        ("cursor" = Option<String>, Query, description = "The previous page's `X-Next-Cursor` header (opaque)"),
    ),
    responses(
        (status = 200, description = "Members", body = [OrganizationMemberResponse],
            headers(("X-Next-Cursor" = String, description = "Paged requests only: the cursor of the next page; absent on the last page"))),
        (status = 400, description = "VALIDATION_ERROR (limit or cursor)", body = ErrorEnvelope),
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
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let page = member_page(query.as_deref())?;
    let (mut list, emails_visible) = if let Some(authority) = authoritative(&state) {
        let user = actor.require_registered()?;
        let acting = ActingUser::of(user);
        // Platform administrators read the authoritative list as a service read; everyone
        // else reads as themselves (Organizations checks organization.members.read).
        let as_actor = (!user.platform_admin).then_some(&acting);
        let members = authority
            .members(id, as_actor)
            .await
            .map_err(authority_error)?;
        let visible = user.platform_admin || may_see_member_emails(&members, user.user_id);
        let list = members
            .iter()
            .map(legacy_member)
            .collect::<ApiResult<Vec<_>>>()?;
        (list, visible)
    } else {
        // The legacy directory admits only platform administrators and ACTIVE OWNERs /
        // ADMINs of an active organization to this list, i.e. exactly the email policy.
        let list = state.meeting.organizations.list_members(&actor, id).await?;
        (list.iter().map(member).collect(), true)
    };
    list.sort_by_key(member_cursor);
    let mut next = None;
    if let Some((cursor, limit)) = page {
        let start = cursor.map_or(0, |c| list.partition_point(|m| member_cursor(m) <= c));
        let end = start.saturating_add(limit).min(list.len());
        if end < list.len() {
            next = list.get(end - 1).map(|m| {
                let (t, id) = member_cursor(m);
                format!("{t}.{id}")
            });
        }
        list = list.drain(start..end).collect();
    }
    let known = display_identity::lookup(&state.db, list.iter().map(|m| m.user_id)).await;
    for m in &mut list {
        if let Some(identity) = known.get(&m.user_id) {
            m.display_name = Some(identity.display_name.clone());
            m.username = Some(identity.username.clone());
            if emails_visible {
                m.email = Some(identity.email.clone());
            }
        }
    }
    let mut response = Json(list).into_response();
    if let Some(v) = next.and_then(|c| HeaderValue::from_str(&c).ok()) {
        response.headers_mut().insert("x-next-cursor", v);
    }
    Ok(response)
}

/// The member-directory email policy: an ACTIVE OWNER or ADMIN of the organization sees
/// member emails; every other member (MANAGER, MEMBER, GUEST, suspended) does not.
pub(crate) fn may_see_member_emails(members: &[AuthorityMember], caller: Uuid) -> bool {
    members.iter().any(|m| {
        m.user_id == caller && m.status == "ACTIVE" && matches!(m.role.as_str(), "OWNER" | "ADMIN")
    })
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
        let items = authority.my_organizations(&state.db, &acting).await?;
        let mut out = Vec::with_capacity(items.len());
        for (item, created_at) in items {
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
/// (`viewer`: role, status and capabilities), plus `createdByDisplayName` (the creator's display
/// name from the account authority, when known). Organizations the caller does not belong to
/// answer ORGANIZATION_NOT_FOUND, exactly like unknown ones. Returns FEATURE_DISABLED until the
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
    let mut reply = relay_reply(
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
    .await?;
    display_identity::enrich_creator(&state.db, &mut reply.body).await;
    Ok(relayed_success(reply))
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
        (status = 200, description = "The organization after the transfer", body = BusinessOrganization),
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

/// Invite someone to an organization by email or CF Meet ID.
///
/// The invitee is given as `email`, or as `identifierType` (`EMAIL` / `CF_MEET_ID`) with
/// `identifier`. A CF Meet ID is resolved server-side (Communications Hub discovery rules:
/// discoverable, visible, not blocked, active) for callers who may invite; anything else is
/// `404 INVITEE_NOT_FOUND`, and the invitee's email is never returned (the hint reads
/// `CF Meet ID <id>`). An account that is already a member gets `409 ALREADY_MEMBER`; a
/// second invitation for the same address replaces the pending one (resend).
///
/// Creates a PENDING invitation for `email` with `role` and returns it with its one-time `token`
/// (shown only in this response; only the token's hash is stored). Only an account whose verified
/// email equals the invited address can accept it. Requires the `organization.members.invite`
/// capability (OWNER, ADMIN, MANAGER). Send an `Idempotency-Key` header to make retries safe.
/// With `sendEmail: true` the invitation link is also emailed to the invitee when notification
/// email is enabled on the deployment (`emailDelivery` reports the outcome; an idempotent replay
/// re-issues the token and emails the new link). Returns FEATURE_DISABLED until the Organizations
/// Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/invitations", tag = "Organizations (v2)", operation_id = "v2_create_organization_invitation",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = CreateOrganizationInvitationRequest,
    responses(
        (status = 201, description = "Invitation with its one-time token", body = OrganizationInvitation),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / INVITEE_NOT_FOUND (CF_MEET_ID) / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "ALREADY_MEMBER / ORGANIZATION_SUSPENDED / IDEMPOTENCY_CONFLICT", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED (invitation emails per hour, CF Meet ID lookups)", body = ErrorEnvelope),
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
    use crate::services::notifications::{self as notify, EmailDelivery};
    // Blueprint Section 11: `sendEmail` belongs to this relay, never to the Organizations
    // contract (which refuses unknown fields); it is removed before the signed call.
    let mut document = json_object(&body)?;
    let send_email = match document
        .as_mut()
        .and_then(|d| d.as_object_mut())
        .and_then(|o| o.remove("sendEmail"))
    {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => b,
        Some(_) => return Err(validation("sendEmail must be a boolean")),
    };
    let addressed = address_invitee(&state, &actor, &headers, id, document.as_mut()).await?;
    let invitee = document
        .as_ref()
        .and_then(|d| d["email"].as_str())
        .map(str::to_owned);
    let forwarded = match &document {
        Some(d) => Bytes::from(serde_json::to_vec(d).map_err(|_| unavailable())?),
        None => body.clone(),
    };
    let ctx = notify::NotificationContext::from_state(&state);
    let mut budget_ok = true;
    if send_email && ctx.config.email_enabled {
        require_authoritative(&state)?;
        let user = relay_actor(&actor)?;
        match notify::invitation_email_budget(&state, user.user_id).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(ApiError::domain(
                    ErrorCode::RateLimited,
                    "Too many invitation emails; try again later or share the link",
                ))
            }
            Err(()) => budget_ok = false,
        }
    }
    let mut reply = relay_reply(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/invitations"),
            headers: &headers,
            body: Some(&forwarded),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await?;
    if send_email {
        let token = reply.body["token"].as_str().map(str::to_owned);
        let delivery = match (ctx.config.email_enabled, budget_ok, token, invitee) {
            (false, _, _, _) => EmailDelivery::Disabled,
            (true, true, Some(token), Some(to)) => {
                let expires_at = reply.body["expiresAt"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.with_timezone(&chrono::Utc));
                let inviter = actor
                    .registered()
                    .map(|r| r.display_name.clone())
                    .unwrap_or_default();
                notify::send_organization_invitation(
                    &ctx,
                    &inviter,
                    &to,
                    id,
                    reply.body["role"].as_str().unwrap_or("MEMBER"),
                    &token,
                    expires_at,
                )
                .await
            }
            // No token in the answer (for example a replay of an invitation that is no
            // longer pending): nothing valid to send.
            _ => EmailDelivery::Failed,
        };
        if let Some(o) = reply.body.as_object_mut() {
            o.insert("emailDelivery".into(), json!(delivery));
        }
    }
    if let (Some(addressed), Some(o)) = (addressed, reply.body.as_object_mut()) {
        o.insert("identifierType".into(), json!(addressed.kind()));
        if let Addressed::CfMeetId(cf_meet_id) = addressed {
            o.insert("identifier".into(), json!(cf_meet_id));
            // Never the resolved address, in any form.
            o.remove("email");
        }
    }
    Ok(relayed_success(reply))
}

/// How an invitation was addressed (`identifierType`).
enum Addressed {
    Email,
    CfMeetId(String),
}

impl Addressed {
    fn kind(&self) -> &'static str {
        match self {
            Addressed::Email => "EMAIL",
            Addressed::CfMeetId(_) => "CF_MEET_ID",
        }
    }
}

const INVITING_ROLES: [&str; 3] = ["OWNER", "ADMIN", "MANAGER"];

fn invitee_not_found() -> ApiError {
    ApiError::domain(
        ErrorCode::InviteeNotFound,
        "No account can be invited with this CF Meet ID",
    )
}

fn already_member() -> ApiError {
    ApiError::domain(
        ErrorCode::AlreadyMember,
        "This account is already a member of the organization",
    )
}

/// Normalizes the invitee of `POST /api/v2/organizations/{id}/invitations` into the
/// Organizations contract (`email`): either `email` as before, or `identifierType` +
/// `identifier` (`EMAIL` / `CF_MEET_ID`). A CF Meet ID is resolved through the
/// Communications Hub's discovery rules and only for callers who may invite (ACTIVE
/// OWNER, ADMIN or MANAGER), so the route is never an account-discovery tool; the
/// resolved address goes to Organizations only, with the hint `CF Meet ID <id>`. An
/// account that already holds a membership is refused with `409 ALREADY_MEMBER`.
async fn address_invitee(
    state: &AppState,
    actor: &ActorContext,
    headers: &HeaderMap,
    organization: Uuid,
    document: Option<&mut Value>,
) -> ApiResult<Option<Addressed>> {
    let Some(obj) = document.and_then(Value::as_object_mut) else {
        return Ok(None);
    };
    // The display hint is the server's (CF Meet ID invitations), never a client's.
    if obj.remove("inviteeHint").is_some() {
        return Err(validation("inviteeHint is not a request field"));
    }
    let (kind, identifier) = match (obj.remove("identifierType"), obj.remove("identifier")) {
        (None, None) => {
            if let Some(email) = obj.get("email").and_then(Value::as_str).map(str::to_owned) {
                refuse_member_email(state, actor, organization, &email).await?;
            }
            return Ok(None);
        }
        (Some(Value::String(k)), Some(Value::String(i))) => (k, i.trim().to_string()),
        _ => {
            return Err(validation(
                "identifierType and identifier must be given together, as strings",
            ))
        }
    };
    if obj.contains_key("email") {
        return Err(validation(
            "Send either email or identifierType with identifier",
        ));
    }
    match kind.as_str() {
        "EMAIL" => {
            let email = identifier.to_ascii_lowercase();
            refuse_member_email(state, actor, organization, &email).await?;
            obj.insert("email".into(), json!(email));
            Ok(Some(Addressed::Email))
        }
        "CF_MEET_ID" => {
            if identifier.is_empty() || identifier.len() > 64 || identifier.contains('@') {
                return Err(invitee_not_found());
            }
            let authority = require_authoritative(state)?;
            let user = relay_actor(actor)?;
            let acting = ActingUser::of(user);
            let members = authority
                .members(organization, Some(&acting))
                .await
                .map_err(authority_error)?;
            let may_invite = members.iter().any(|m| {
                m.user_id == user.user_id
                    && m.status == "ACTIVE"
                    && INVITING_ROLES.contains(&m.role.as_str())
            });
            if !may_invite {
                return Err(permission_denied(
                    "Your role in this organization cannot invite members",
                ));
            }
            let invitee = crate::services::communications::resolve_organization_invitee(
                state,
                user,
                headers,
                &identifier,
            )
            .await?
            .ok_or_else(invitee_not_found)?;
            if invitee.is_self
                || members
                    .iter()
                    .any(|m| m.user_id == invitee.user_id && m.status != "REMOVED")
            {
                return Err(already_member());
            }
            let account = users::Entity::find_by_id(invitee.user_id)
                .one(&state.db)
                .await?
                .filter(|u| {
                    u.role != users::UserRole::Guest
                        && matches!(
                            u.status,
                            users::UserStatus::Active | users::UserStatus::PendingVerification
                        )
                })
                .ok_or_else(invitee_not_found)?;
            obj.insert(
                "email".into(),
                json!(account.email.trim().to_ascii_lowercase()),
            );
            obj.insert(
                "inviteeHint".into(),
                json!(format!("CF Meet ID {identifier}")),
            );
            Ok(Some(Addressed::CfMeetId(identifier)))
        }
        _ => Err(validation("identifierType must be EMAIL or CF_MEET_ID")),
    }
}

/// `409 ALREADY_MEMBER` when the account registered with `email` holds a membership of
/// the organization (best effort: anything unavailable leaves the decision to the
/// Organizations Service).
async fn refuse_member_email(
    state: &AppState,
    actor: &ActorContext,
    organization: Uuid,
    email: &str,
) -> ApiResult<()> {
    use sea_orm::{sea_query::Expr, QueryFilter};
    let (Some(authority), Ok(user)) = (authoritative(state), actor.require_registered()) else {
        return Ok(());
    };
    let normalized = email.trim().to_ascii_lowercase();
    let Ok(Some(account)) = users::Entity::find()
        .filter(Expr::cust_with_values("lower(email) = $1", [normalized]))
        .one(&state.db)
        .await
    else {
        return Ok(());
    };
    let acting = ActingUser::of(user);
    let Ok(members) = authority.members(organization, Some(&acting)).await else {
        return Ok(());
    };
    if members
        .iter()
        .any(|m| m.user_id == account.id && m.status != "REMOVED")
    {
        return Err(already_member());
    }
    Ok(())
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
        (status = 200, description = "Invitations", body = OrganizationInvitationList),
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
        (status = 200, description = "Membership created by the invitation", body = BusinessOrganizationMembership),
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
        (status = 200, description = "Declined invitation", body = OrganizationInvitation),
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
        (status = 200, description = "Audit events, newest first", body = OrganizationAuditEventPage),
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
        (status = 200, description = "Provisioning state", body = OrganizationProvisioning),
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

// ---------------------------------------------------------------------------------------
// Teams and policies (Blueprint Section 6 catalogue, Section 17 web administration;
// authoritative mode only)
// ---------------------------------------------------------------------------------------

/// A relayed team-member answer with each member's display identity added.
async fn relay_team_members(
    state: &AppState,
    actor: &ActorContext,
    r: Relay<'_>,
) -> ApiResult<Response> {
    let mut reply = relay_reply(state, actor, r).await?;
    display_identity::enrich_members(&state.db, &mut reply.body).await;
    Ok(relayed_success(reply))
}

/// List an organization's teams.
///
/// Returns the organization's teams in id order (`cursor` = the previous page's `nextCursor`;
/// `limit` 1–200, default 50), optionally filtered by `status` (ACTIVE or ARCHIVED). Requires an
/// active membership allowed to read the member directory (`organization.members.read` under the
/// organization policy). Organizations the caller does not belong to answer
/// ORGANIZATION_NOT_FOUND, exactly like unknown ones. Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/teams", tag = "Organizations (v2)", operation_id = "v2_list_organization_teams",
    params(
        ("id" = Uuid, Path, description = "Organization id"),
        ("status" = Option<String>, Query, description = "ACTIVE or ARCHIVED"),
        ("cursor" = Option<Uuid>, Query, description = "The previous page's nextCursor"),
        ("limit" = Option<i64>, Query, description = "1–200 (default 50)"),
    ),
    responses(
        (status = 200, description = "Teams", body = OrganizationTeamPage),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / ORGANIZATION_POLICY_RESTRICTED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_organization_teams(
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
            path: format!("/internal/v1/organizations/{id}/teams"),
            headers: &headers,
            body: None,
            raw_query: query.as_deref(),
            allowed_query: &["status", "cursor", "limit"],
        },
    )
    .await
}

/// Create a team.
///
/// Creates an ACTIVE team (`name` 1–120 characters, unique among the organization's ACTIVE teams;
/// optional `description` and `parentTeamId`, nesting bounded). Requires the
/// `organization.teams.manage` capability (OWNER, ADMIN; MANAGERs only when the organization
/// policy lets them create teams, and they become the new team's TEAM_MANAGER). Send an
/// `Idempotency-Key` header to make retries safe. Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/teams", tag = "Organizations (v2)", operation_id = "v2_create_organization_team",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = CreateOrganizationTeamRequest,
    responses(
        (status = 201, description = "Created team", body = OrganizationTeam),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / ORGANIZATION_POLICY_RESTRICTED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND (parent) / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "TEAM_NAME_TAKEN / TEAM_ARCHIVED (parent) / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED / IDEMPOTENCY_CONFLICT", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_organization_team(
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
            path: format!("/internal/v1/organizations/{id}/teams"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Get a team.
///
/// Returns one team of the organization with its ACTIVE member count. A team of another
/// organization answers TEAM_NOT_FOUND. Requires the same access as the team list. Returns
/// FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/teams/{teamId}", tag = "Organizations (v2)", operation_id = "v2_get_organization_team",
    params(("id" = Uuid, Path, description = "Organization id"), ("teamId" = Uuid, Path, description = "Team id")),
    responses(
        (status = 200, description = "Team", body = OrganizationTeam),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / ORGANIZATION_POLICY_RESTRICTED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_organization_team(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, team_id)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::GET,
            path: format!("/internal/v1/organizations/{id}/teams/{team_id}"),
            headers: &headers,
            body: None,
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Update a team.
///
/// Changes the team's `name`, `description` (`null` clears it) or `parentTeamId` (`null`
/// detaches it). `expectedVersion` must be the team's current `version`, otherwise
/// VERSION_CONFLICT. Requires `organization.teams.manage` (OWNER, ADMIN, or a MANAGER who is
/// TEAM_MANAGER of this team). Returns FEATURE_DISABLED until the Organizations Service is
/// authoritative.
#[utoipa::path(
    patch, path = "/api/v2/organizations/{id}/teams/{teamId}", tag = "Organizations (v2)", operation_id = "v2_update_organization_team",
    params(("id" = Uuid, Path, description = "Organization id"), ("teamId" = Uuid, Path, description = "Team id")),
    request_body = UpdateOrganizationTeamRequest,
    responses(
        (status = 200, description = "Updated team", body = OrganizationTeam),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / ORGANIZATION_POLICY_RESTRICTED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / TEAM_NAME_TAKEN / TEAM_ARCHIVED / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_organization_team(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, team_id)): V2Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::PATCH,
            path: format!("/internal/v1/organizations/{id}/teams/{team_id}"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Archive a team.
///
/// Archives the team (idempotent) and ends every ACTIVE team membership; refused while the team
/// has ACTIVE child teams. Optional `expectedVersion`. Requires `organization.teams.manage` as an
/// OWNER or ADMIN. Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/teams/{teamId}/archive", tag = "Organizations (v2)", operation_id = "v2_archive_organization_team",
    params(("id" = Uuid, Path, description = "Organization id"), ("teamId" = Uuid, Path, description = "Team id")),
    request_body = ArchiveOrganizationTeamRequest,
    responses(
        (status = 200, description = "Archived team", body = OrganizationTeam),
        (status = 400, description = "VALIDATION_ERROR (active child teams)", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn archive_organization_team(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, team_id)): V2Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    relay(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/teams/{team_id}/archive"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// List a team's members.
///
/// Returns the team's ACTIVE memberships in id order (`cursor` = the previous page's
/// `nextCursor`; `limit` 1–200, default 50), each with the member's display identity
/// (`displayName`, `username`; never an email address). Requires the same access as the team
/// list. Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/teams/{teamId}/members", tag = "Organizations (v2)", operation_id = "v2_list_organization_team_members",
    params(
        ("id" = Uuid, Path, description = "Organization id"),
        ("teamId" = Uuid, Path, description = "Team id"),
        ("cursor" = Option<Uuid>, Query, description = "The previous page's nextCursor"),
        ("limit" = Option<i64>, Query, description = "1–200 (default 50)"),
    ),
    responses(
        (status = 200, description = "Team members", body = OrganizationTeamMemberPage),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / ORGANIZATION_POLICY_RESTRICTED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_organization_team_members(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, team_id)): V2Path<(Uuid, Uuid)>,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    relay_team_members(
        &state,
        &actor,
        Relay {
            method: Method::GET,
            path: format!("/internal/v1/organizations/{id}/teams/{team_id}/members"),
            headers: &headers,
            body: None,
            raw_query: query.as_deref(),
            allowed_query: &["cursor", "limit"],
        },
    )
    .await
}

/// Add a member to a team.
///
/// Adds an ACTIVE organization member (exactly one of `membershipId` and `userId`) with `role`
/// TEAM_MANAGER (organization MANAGERs, ADMINs and OWNERs only) or TEAM_MEMBER. OWNERs and ADMINs
/// holding `organization.teams.manage` add either role; a MANAGER who is TEAM_MANAGER of the team
/// adds TEAM_MEMBERs only. Re-adding an ACTIVE member answers 200 with the existing membership.
/// Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    post, path = "/api/v2/organizations/{id}/teams/{teamId}/members", tag = "Organizations (v2)", operation_id = "v2_add_organization_team_member",
    params(("id" = Uuid, Path, description = "Organization id"), ("teamId" = Uuid, Path, description = "Team id")),
    request_body = AddOrganizationTeamMemberRequest,
    responses(
        (status = 200, description = "Already a member", body = OrganizationTeamMember),
        (status = 201, description = "Added team member", body = OrganizationTeamMember),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / ORGANIZATION_POLICY_RESTRICTED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "TEAM_ARCHIVED / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn add_organization_team_member(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, team_id)): V2Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    relay_team_members(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!("/internal/v1/organizations/{id}/teams/{team_id}/members"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Change a team member's role.
///
/// Sets `role` (TEAM_MANAGER or TEAM_MEMBER) of one team membership (the path's member segment is
/// the team membership `id`). `expectedVersion` must be its current `version`. OWNERs and ADMINs
/// assign either role; a TEAM_MANAGER can only step down; nobody raises their own team role.
/// Returns FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    patch, path = "/api/v2/organizations/{id}/teams/{teamId}/members/{memberId}", tag = "Organizations (v2)", operation_id = "v2_update_organization_team_member",
    params(
        ("id" = Uuid, Path, description = "Organization id"),
        ("teamId" = Uuid, Path, description = "Team id"),
        ("memberId" = Uuid, Path, description = "Team membership id (the `id` of a team member list item)"),
    ),
    request_body = UpdateOrganizationTeamMemberRequest,
    responses(
        (status = 200, description = "Updated team member", body = OrganizationTeamMember),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / TEAM_ARCHIVED / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_organization_team_member(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, team_id, member_id)): V2Path<(Uuid, Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    relay_team_members(
        &state,
        &actor,
        Relay {
            method: Method::PATCH,
            path: format!("/internal/v1/organizations/{id}/teams/{team_id}/members/{member_id}"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Remove a member from a team.
///
/// Ends one team membership (idempotent: removing a removed membership answers it again).
/// OWNERs and ADMINs remove anyone, a TEAM_MANAGER removes TEAM_MEMBERs, and everyone can leave a
/// team themselves. The organization membership is unchanged. Returns FEATURE_DISABLED until the
/// Organizations Service is authoritative.
#[utoipa::path(
    delete, path = "/api/v2/organizations/{id}/teams/{teamId}/members/{memberId}", tag = "Organizations (v2)", operation_id = "v2_remove_organization_team_member",
    params(
        ("id" = Uuid, Path, description = "Organization id"),
        ("teamId" = Uuid, Path, description = "Team id"),
        ("memberId" = Uuid, Path, description = "Team membership id (the `id` of a team member list item)"),
    ),
    responses(
        (status = 200, description = "The ended team membership", body = OrganizationTeamMember),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_organization_team_member(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Path((id, team_id, member_id)): V2Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Response> {
    relay_team_members(
        &state,
        &actor,
        Relay {
            method: Method::POST,
            path: format!(
                "/internal/v1/organizations/{id}/teams/{team_id}/members/{member_id}/remove"
            ),
            headers: &headers,
            body: None,
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Get the organization policy.
///
/// Returns the organization's policy document (member directory visibility, guests, meeting
/// hosting, enabled features, retention hint, delegated administration) with its `revision` (0
/// while the defaults apply). Requires an active membership (`organization.read`). Returns
/// FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/policies", tag = "Organizations (v2)", operation_id = "v2_get_organization_policy",
    params(("id" = Uuid, Path, description = "Organization id")),
    responses(
        (status = 200, description = "Organization policy", body = OrganizationPolicy),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_organization_policy(
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
            path: format!("/internal/v1/organizations/{id}/policies"),
            headers: &headers,
            body: None,
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

/// Change the organization policy (compare-and-swap).
///
/// Merges the partial `policy` document into the current one (unknown keys and out-of-range
/// values are VALIDATION_ERROR) and stores it as `revision + 1`. `expectedRevision` must be the
/// current `revision`, otherwise VERSION_CONFLICT. A policy only removes capabilities and never
/// touches OWNER administration. Requires `organization.policies.manage` (OWNER; ADMINs unless
/// the policy reserves it to OWNERs); `delegatedAdministration` is OWNER-only. Returns
/// FEATURE_DISABLED until the Organizations Service is authoritative.
#[utoipa::path(
    patch, path = "/api/v2/organizations/{id}/policies", tag = "Organizations (v2)", operation_id = "v2_update_organization_policy",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = UpdateOrganizationPolicyRequest,
    responses(
        (status = 200, description = "Updated organization policy", body = OrganizationPolicy),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / ORGANIZATION_POLICY_RESTRICTED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "VERSION_CONFLICT / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_organization_policy(
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
            path: format!("/internal/v1/organizations/{id}/policies"),
            headers: &headers,
            body: Some(&body),
            raw_query: None,
            allowed_query: &[],
        },
    )
    .await
}

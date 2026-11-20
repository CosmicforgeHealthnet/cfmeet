//! Memberships: listing, role / status changes (last-owner and no-self-escalation rules),
//! self-leave and atomic ownership transfer. Every change locks the organization row
//! first (`SELECT … FOR UPDATE`), so concurrent changes serialize and the last-owner
//! invariant is checked against committed state.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    admin_bridge::{self, command, BridgeCommand, Outcome, Risk, Subject},
    authorize, membership_not_found,
    middleware::Caller,
    page_limit, parse, respond,
};
use crate::{
    db,
    domain::{can_assign, can_manage_target, Capability, LifecycleStatus, MembershipStatus, Role},
    error::{ApiError, ApiResult, ErrorCode},
    state::AppState,
    store::{self, Event, MembershipRow, OpContext, OrgRow, MEMBERSHIP_COLUMNS, ORG_COLUMNS},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub cursor: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /internal/v1/organizations/{id}/members` — non-removed memberships (id order).
/// With an actor it requires `organization.members.read`; a service read (no actor) gets
/// the authoritative list.
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let limit = page_limit(q.limit)?;
    match caller.acting_user {
        Some(user) => {
            authorize(&state.db, id, user, Capability::MembersRead, false).await?;
        }
        None => {
            store::org(&state.db, id, false)
                .await?
                .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
        }
    }
    page(&state, id, q.cursor, limit).await
}

/// One page of an organization's non-removed memberships (id order). Callers check
/// access (and that the organization exists) first.
pub(super) async fn page(
    state: &AppState,
    id: Uuid,
    cursor: Option<Uuid>,
    limit: i64,
) -> ApiResult<Response> {
    let rows: Vec<MembershipRow> = db::all(
        &state.db,
        &format!(
            "SELECT {MEMBERSHIP_COLUMNS} FROM organization_memberships WHERE organization_id = $1 AND status <> 'REMOVED' AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3"
        ),
        vec![id.into(), cursor.into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let items: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(MembershipRow::to_json)
        .collect();
    let next = if more {
        rows.get(limit as usize - 1).map(|m| m.id)
    } else {
        None
    };
    Ok(respond(
        StatusCode::OK,
        json!({ "items": items, "nextCursor": next }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateBody {
    pub expected_version: i64,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Writes `after` for `before`, its history row and the membership events (one aggregate
/// version per event, restrictive changes first).
#[allow(clippy::too_many_arguments)]
async fn change_membership(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    before: &MembershipRow,
    role: Role,
    status: MembershipStatus,
    history_kind: Option<&str>,
) -> Result<MembershipRow, ApiError> {
    let role_changed = role != before.role();
    let status_changed = status != before.status();
    let bumps = role_changed as i64 + status_changed as i64;
    let after: MembershipRow = db::one(
        txn,
        &format!(
            "UPDATE organization_memberships SET role = $2, status = $3, removed_at = CASE WHEN $3 = 'REMOVED' THEN $4 ELSE NULL END, version = version + $5, updated_at = $4 WHERE id = $1 RETURNING {MEMBERSHIP_COLUMNS}"
        ),
        vec![before.id.into(), role.as_str().into(), status.as_str().into(), now.into(), bumps.into()],
    )
    .await?
    .ok_or_else(membership_not_found)?;
    let kind = history_kind.unwrap_or(match (role_changed, status_changed) {
        (true, true) => "ROLE_AND_STATUS_CHANGED",
        (true, false) => "ROLE_CHANGED",
        _ => "STATUS_CHANGED",
    });
    store::history(txn, ctx, now, Some(before), &after, kind).await?;
    if status_changed && status == MembershipStatus::Removed {
        // A removed member leaves every team (Section 6: only ACTIVE members are in teams).
        super::teams::end_team_memberships(txn, ctx, now, after.id).await?;
    }
    if role_changed || status_changed {
        // Invitations are only as good as their inviter: a removed, suspended or demoted
        // member's pending invitations they could no longer issue are revoked.
        super::invitations::revoke_unbacked(
            txn,
            ctx,
            now,
            after.organization_id,
            after.user_id,
            after.role(),
            after.status() == MembershipStatus::Active,
        )
        .await?;
    }
    let mut version = before.version;
    let status_event = |v: i64| -> Option<Event<'static>> {
        if !status_changed {
            return None;
        }
        let (event_type, payload) = match status {
            MembershipStatus::Active => (
                "membership.activated",
                json!({ "userId": after.user_id, "role": after.role, "membershipVersion": v }),
            ),
            MembershipStatus::Suspended => (
                "membership.suspended",
                json!({ "userId": after.user_id, "membershipVersion": v }),
            ),
            MembershipStatus::Removed => (
                "membership.removed",
                json!({ "userId": after.user_id, "membershipVersion": v }),
            ),
        };
        Some(Event {
            organization_id: after.organization_id,
            aggregate_type: "membership",
            aggregate_id: after.id,
            aggregate_version: v,
            event_type,
            payload,
        })
    };
    let role_event = |v: i64| -> Option<Event<'static>> {
        role_changed.then(|| Event {
            organization_id: after.organization_id,
            aggregate_type: "membership",
            aggregate_id: after.id,
            aggregate_version: v,
            event_type: "membership.role_changed",
            payload: json!({ "userId": after.user_id, "fromRole": before.role, "toRole": after.role, "membershipVersion": v }),
        })
    };
    // Restrictive status changes (suspension, removal) are applied by consumers before any
    // role change; an activation after it.
    let restrictive_first = status != MembershipStatus::Active;
    let order: [bool; 2] = if restrictive_first {
        [true, false]
    } else {
        [false, true]
    };
    for is_status in order {
        let next = version + 1;
        let event = if is_status {
            status_event(next)
        } else {
            role_event(next)
        };
        if let Some(e) = event {
            store::emit(txn, ctx, now, e).await?;
            version = next;
        }
    }
    Ok(after)
}

/// `PATCH /internal/v1/organizations/{id}/members/{membershipId}`.
pub async fn update(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, membership_id)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (UpdateBody, Value) = parse(&body)?;
    let new_role = req
        .role
        .as_deref()
        .map(|r| {
            Role::parse(r).ok_or_else(|| {
                ApiError::validation("role must be OWNER, ADMIN, MANAGER, MEMBER or GUEST")
            })
        })
        .transpose()?;
    let new_status = req
        .status
        .as_deref()
        .map(|s| {
            MembershipStatus::parse(s)
                .ok_or_else(|| ApiError::validation("status must be ACTIVE, SUSPENDED or REMOVED"))
        })
        .transpose()?;
    if new_role.is_none() && new_status.is_none() {
        return Err(ApiError::validation("role or status is required"));
    }
    let txn = state.db.begin().await?;
    let access = authorize(&txn, id, user, Capability::MembersManage, true).await?;
    let target = store::membership(&txn, id, membership_id, true)
        .await?
        .filter(|m| m.status() != MembershipStatus::Removed)
        .ok_or_else(membership_not_found)?;
    if target.version != req.expected_version {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    let actor_role = access.membership.role();
    let (cur_role, cur_status) = (target.role(), target.status());
    let role = new_role.unwrap_or(cur_role);
    let status = new_status.unwrap_or(cur_status);
    if role == cur_role && status == cur_status {
        return Ok(respond(StatusCode::OK, target.to_json()));
    }
    if target.id == access.membership.id {
        // No self-escalation; one's own status changes only by leaving.
        if role.rank() > cur_role.rank() || status != cur_status {
            return Err(ApiError::new(ErrorCode::PermissionDenied));
        }
    } else {
        if !can_manage_target(actor_role, cur_role) {
            return Err(ApiError::new(ErrorCode::PermissionDenied));
        }
        if role != cur_role && !can_assign(actor_role, role) {
            return Err(ApiError::new(ErrorCode::PermissionDenied));
        }
    }
    let loses_ownership = cur_role == Role::Owner
        && cur_status == MembershipStatus::Active
        && (role != Role::Owner || status != MembershipStatus::Active);
    if loses_ownership && store::other_active_owners(&txn, id, target.id).await? == 0 {
        return Err(ApiError::new(ErrorCode::LastOwner));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let after = change_membership(&txn, &ctx, now, &target, role, status, None).await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "membership.updated",
        "membership",
        Some(after.id),
        json!({ "userId": after.user_id, "fromRole": target.role, "toRole": after.role, "fromStatus": target.status, "toStatus": after.status }),
    )
    .await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, after.to_json()))
}

/// `POST /internal/v1/organizations/{id}/members/me/leave` — refused for the last active
/// OWNER.
pub async fn leave(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let txn = state.db.begin().await?;
    store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let mine = store::current_membership(&txn, id, user, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if mine.role() == Role::Owner
        && mine.status() == MembershipStatus::Active
        && store::other_active_owners(&txn, id, mine.id).await? == 0
    {
        return Err(ApiError::new(ErrorCode::LastOwner));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let after = change_membership(
        &txn,
        &ctx,
        now,
        &mine,
        mine.role(),
        MembershipStatus::Removed,
        Some("LEFT"),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "membership.left",
        "membership",
        Some(after.id),
        json!({ "userId": user, "role": after.role }),
    )
    .await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, after.to_json()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferBody {
    pub to_membership_id: Uuid,
    pub expected_version: i64,
}

/// `POST /internal/v1/organizations/{id}/ownership-transfer` — atomically: the target
/// becomes OWNER and the acting OWNER becomes ADMIN; `expectedVersion` is the
/// organization's version (bumped by the transfer, so concurrent transfers conflict).
pub async fn transfer_ownership(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (TransferBody, Value) = parse(&body)?;
    let txn = state.db.begin().await?;
    let access = authorize(&txn, id, user, Capability::OwnershipTransfer, true).await?;
    if access.org.version != req.expected_version {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    if req.to_membership_id == access.membership.id {
        return Err(ApiError::validation(
            "ownership cannot be transferred to oneself",
        ));
    }
    let target = store::membership(&txn, id, req.to_membership_id, true)
        .await?
        .filter(|m| m.status() != MembershipStatus::Removed)
        .ok_or_else(membership_not_found)?;
    if target.status() != MembershipStatus::Active {
        return Err(ApiError::validation(
            "the new owner's membership must be active",
        ));
    }
    if target.role() == Role::Owner {
        return Err(ApiError::validation(
            "the target membership is already an owner",
        ));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let new_owner = change_membership(
        &txn,
        &ctx,
        now,
        &target,
        Role::Owner,
        MembershipStatus::Active,
        Some("OWNERSHIP_TRANSFER"),
    )
    .await?;
    let me = change_membership(
        &txn,
        &ctx,
        now,
        &access.membership,
        Role::Admin,
        MembershipStatus::Active,
        Some("OWNERSHIP_TRANSFER"),
    )
    .await?;
    let org: OrgRow = db::one(
        &txn,
        &format!("UPDATE organizations SET version = version + 1, updated_at = $2 WHERE id = $1 RETURNING {ORG_COLUMNS}"),
        vec![id.into(), now.into()],
    )
    .await?
    .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    store::emit(
        &txn,
        &ctx,
        now,
        Event {
            organization_id: id,
            aggregate_type: "organization",
            aggregate_id: id,
            aggregate_version: org.version,
            event_type: "ownership.transferred",
            payload: json!({ "fromUserId": user, "toUserId": new_owner.user_id }),
        },
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "ownership.transferred",
        "organization",
        Some(id),
        json!({ "fromUserId": user, "toUserId": new_owner.user_id, "toMembershipId": new_owner.id }),
    )
    .await?;
    let doc = store::organization_json(&txn, &org, Some(&me)).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppointOwnerBody {
    pub user_id: Uuid,
    /// Why the operator intervenes (recorded in the audit event; 1–500 characters).
    pub reason: String,
    /// The version of the user's current (non-removed) membership, 0 when they have none.
    /// Required from the admin bridge; checked when given.
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// The admin bridge's answer for a membership: its status and version.
pub fn membership_outcome(m: &MembershipRow) -> Outcome {
    Outcome::new(
        m.status.clone(),
        Some(m.version),
        json!({
            "organizationId": m.organization_id,
            "membershipId": m.id,
            "userId": m.user_id,
            "role": m.role,
            "status": m.status,
            "version": m.version,
            "updatedAt": store::rfc3339(m.updated_at),
            "removedAt": m.removed_at.map(store::rfc3339),
        }),
    )
}

/// `POST /internal/v1/admin/organizations/{id}/owners` — operator recovery of an
/// organization that lost its owner (for example the only owner's account was
/// deactivated): makes `userId` an ACTIVE OWNER. An existing membership is promoted and
/// re-activated; otherwise one is created (`joined_via = OPERATOR`). The operator's reason
/// is audited. Requires `organization.administer`. Archived organizations are refused
/// (archival is terminal). Idempotent: an ACTIVE OWNER is returned unchanged.
///
/// The admin bridge (R3, two people) also sends `Idempotency-Key` and `expectedVersion`
/// (the user's current membership version, 0 for none) and gets the bridge answer.
pub async fn admin_appoint_owner(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let bridge = BridgeCommand::of(&caller, &headers, Risk::R3)?;
    let (req, _): (AppointOwnerBody, Value) = parse(&body)?;
    let reason = req.reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 || reason.chars().any(char::is_control) {
        return Err(ApiError::validation(
            "reason must be 1-500 characters without control characters",
        ));
    }
    let expected = match &bridge {
        Some(_) => Some(admin_bridge::expected_version(req.expected_version, 0)?),
        None => req.expected_version,
    };
    let txn = state.db.begin().await?;
    if let Some(b) = &bridge {
        if admin_bridge::replayed(
            &txn,
            b,
            command::OWNERSHIP_RECOVER,
            Subject::Organization,
            Some(id),
        )
        .await?
        .is_some()
        {
            store::org(&txn, id, false)
                .await?
                .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
            let current = store::current_membership(&txn, id, req.user_id, false).await?;
            txn.commit().await?;
            return Ok(match current {
                Some(m) => membership_outcome(&m),
                None => Outcome::new(
                    "REMOVED",
                    None,
                    json!({ "organizationId": id, "userId": req.user_id }),
                ),
            }
            .response());
        }
    }
    let org = store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if org.status() == LifecycleStatus::Archived {
        return Err(ApiError::new(ErrorCode::OrganizationArchived));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_operator();
    let existing = store::current_membership(&txn, id, req.user_id, true).await?;
    if let Some(v) = expected {
        admin_bridge::check_version(existing.as_ref().map(|m| m.version).unwrap_or(0), v)?;
    }
    let changed = match existing {
        Some(m) if m.role() == Role::Owner && m.status() == MembershipStatus::Active => None,
        Some(m) => {
            let previous = json!({ "role": m.role, "status": m.status });
            let after = change_membership(
                &txn,
                &ctx,
                now,
                &m,
                Role::Owner,
                MembershipStatus::Active,
                None,
            )
            .await?;
            Some((after, previous))
        }
        None => {
            let m: MembershipRow = db::one(
                &txn,
                &format!(
                    "INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_by, created_at, updated_at, version) \
                     VALUES ($1, $2, $3, 'OWNER', 'ACTIVE', 'OPERATOR', $4, $5, $5, 1) RETURNING {MEMBERSHIP_COLUMNS}"
                ),
                vec![Uuid::new_v4().into(), id.into(), req.user_id.into(), ctx.user_id.into(), now.into()],
            )
            .await?
            .ok_or(ApiError::new(ErrorCode::ServiceUnavailable))?;
            store::history(&txn, &ctx, now, None, &m, "CREATED").await?;
            store::emit_membership_activated(&txn, &ctx, now, &m).await?;
            Some((m, Value::Null))
        }
    };
    let after = match changed {
        Some((after, previous)) => {
            store::audit(
                &txn,
                &ctx,
                now,
                Some(id),
                "membership.owner_appointed",
                "membership",
                Some(after.id),
                json!({ "userId": after.user_id, "previous": previous, "reason": reason }),
            )
            .await?;
            after
        }
        // Already an ACTIVE OWNER: nothing written (but a bridge command is recorded).
        None => store::current_membership(&txn, id, req.user_id, false)
            .await?
            .ok_or_else(membership_not_found)?,
    };
    if let Some(b) = &bridge {
        let outcome = membership_outcome(&after);
        admin_bridge::record(
            &txn,
            b,
            command::OWNERSHIP_RECOVER,
            Subject::Organization,
            id,
            Some(id),
            &outcome,
            now,
        )
        .await?;
        txn.commit().await?;
        return Ok(outcome.response());
    }
    txn.commit().await?;
    Ok(respond(StatusCode::OK, after.to_json()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminMemberChangeBody {
    pub expected_version: Option<i64>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// `POST /internal/v1/admin/organizations/{id}/members/{membershipId}/change`
/// `{expectedVersion, role?, status?}` — the admin bridge's staff change of a membership
/// (R3, two people; bridge only). Any role or status (`REMOVED` ends the membership and its
/// team memberships); the last active OWNER is kept (409 `LAST_OWNER`); a removed
/// membership is not found; an archived organization is refused. Same history, events and
/// audit (`membership.updated`, actor OPERATOR) as an owner's change.
pub async fn admin_change_member(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path((id, membership_id)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let b = BridgeCommand::required(&caller, &headers, Risk::R3)?;
    let (req, _): (AdminMemberChangeBody, Value) = parse(&body)?;
    let expected = admin_bridge::expected_version(req.expected_version, 1)?;
    let new_role = req
        .role
        .as_deref()
        .map(|r| {
            Role::parse(r).ok_or_else(|| {
                ApiError::validation("role must be OWNER, ADMIN, MANAGER, MEMBER or GUEST")
            })
        })
        .transpose()?;
    let new_status = req
        .status
        .as_deref()
        .map(|s| {
            MembershipStatus::parse(s)
                .ok_or_else(|| ApiError::validation("status must be ACTIVE, SUSPENDED or REMOVED"))
        })
        .transpose()?;
    if new_role.is_none() && new_status.is_none() {
        return Err(ApiError::validation("role or status is required"));
    }
    let txn = state.db.begin().await?;
    if admin_bridge::replayed(
        &txn,
        &b,
        command::MEMBER_CHANGE,
        Subject::Membership,
        Some(membership_id),
    )
    .await?
    .is_some()
    {
        let m = store::membership(&txn, id, membership_id, false)
            .await?
            .ok_or_else(membership_not_found)?;
        txn.commit().await?;
        return Ok(membership_outcome(&m).response());
    }
    let org = store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if org.status() == LifecycleStatus::Archived {
        return Err(ApiError::new(ErrorCode::OrganizationArchived));
    }
    let target = store::membership(&txn, id, membership_id, true)
        .await?
        .filter(|m| m.status() != MembershipStatus::Removed)
        .ok_or_else(membership_not_found)?;
    admin_bridge::check_version(target.version, expected)?;
    let (cur_role, cur_status) = (target.role(), target.status());
    let role = new_role.unwrap_or(cur_role);
    let status = new_status.unwrap_or(cur_status);
    let now = db::now(&txn).await?;
    let after = if role == cur_role && status == cur_status {
        target
    } else {
        let loses_ownership = cur_role == Role::Owner
            && cur_status == MembershipStatus::Active
            && (role != Role::Owner || status != MembershipStatus::Active);
        if loses_ownership && store::other_active_owners(&txn, id, target.id).await? == 0 {
            return Err(ApiError::new(ErrorCode::LastOwner));
        }
        let ctx = caller.as_operator();
        let after = change_membership(&txn, &ctx, now, &target, role, status, None).await?;
        store::audit(
            &txn,
            &ctx,
            now,
            Some(id),
            "membership.updated",
            "membership",
            Some(after.id),
            json!({ "userId": after.user_id, "fromRole": target.role, "toRole": after.role, "fromStatus": target.status, "toStatus": after.status }),
        )
        .await?;
        after
    };
    let outcome = membership_outcome(&after);
    admin_bridge::record(
        &txn,
        &b,
        command::MEMBER_CHANGE,
        Subject::Membership,
        after.id,
        Some(id),
        &outcome,
        now,
    )
    .await?;
    txn.commit().await?;
    Ok(outcome.response())
}

//! Memberships: listing, role / status changes (last-owner and no-self-escalation rules),
//! self-leave and atomic ownership transfer. Every change locks the organization row
//! first (`SELECT … FOR UPDATE`), so concurrent changes serialize and the last-owner
//! invariant is checked against committed state.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{authorize, membership_not_found, middleware::Caller, page_limit, parse, respond};
use crate::{
    db,
    domain::{can_assign, can_manage_target, Capability, MembershipStatus, Role},
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
    let rows: Vec<MembershipRow> = db::all(
        &state.db,
        &format!(
            "SELECT {MEMBERSHIP_COLUMNS} FROM organization_memberships WHERE organization_id = $1 AND status <> 'REMOVED' AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3"
        ),
        vec![id.into(), q.cursor.into(), (limit + 1).into()],
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

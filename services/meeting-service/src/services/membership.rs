//! Membership service — persistent meeting membership (spec §B5, §C23).
//!
//! Membership is NOT conference participation. One active membership per
//! space + principal (DB partial unique index). Grants are bounded by the actor's
//! own grant authority; nobody can modify their own membership.

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{MemberStatus, PrincipalType, Role, SpaceStatus},
    dto::v2::{AddMemberRequest, UpdateMemberRequest},
    error::{ApiError, ApiResult, ErrorCode},
    models::{meeting_members, meeting_spaces, users},
    services::{
        actor::{ActorContext, RegisteredActor},
        meeting_core::{
            find_space, is_unique_violation, lock_space, permission_denied, space_role,
            state_conflict, validation, Core,
        },
        meeting_events::{action, audit, AuditActor, AuditRecord},
        meeting_permissions::{check_grant, space_allows, PermissionContext, SpacePermission},
    },
};

fn member_not_found() -> ApiError {
    ApiError::domain(
        ErrorCode::ParticipantNotFound,
        "Member not found in this meeting",
    )
}

pub struct MembershipService {
    core: Core,
}

impl MembershipService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    async fn manager_role<C: sea_orm::ConnectionTrait>(
        &self,
        conn: &C,
        space: &meeting_spaces::Model,
        actor: &RegisteredActor,
        permission: SpacePermission,
    ) -> ApiResult<Role> {
        let role = space_role(conn, space, actor).await?.map(|(r, _)| r);
        let pctx = PermissionContext::for_space(space);
        match role {
            Some(r) if space_allows(r, permission, &pctx) => Ok(r),
            _ => Err(permission_denied(
                "You are not permitted to manage members of this meeting",
            )),
        }
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
    ) -> ApiResult<Vec<meeting_members::Model>> {
        let actor = ctx.require_registered()?;
        let space = find_space(&self.core.db, space_id).await?;
        self.manager_role(&self.core.db, &space, actor, SpacePermission::ViewMembers)
            .await?;
        Ok(meeting_members::Entity::find()
            .filter(meeting_members::Column::MeetingSpaceId.eq(space_id))
            .order_by_asc(meeting_members::Column::CreatedAt)
            .all(&self.core.db)
            .await?)
    }

    pub async fn add(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        req: AddMemberRequest,
    ) -> ApiResult<meeting_members::Model> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        let (principal_type, principal_ref, user_id) = match (&req.user_id, &req.email) {
            (Some(u), None) => (PrincipalType::User, u.to_string(), Some(*u)),
            (None, Some(e)) => {
                let e = e.trim().to_ascii_lowercase();
                if !e.contains('@') || e.len() > 320 {
                    return Err(validation("email is not a valid email address"));
                }
                (PrincipalType::Email, e, None)
            }
            _ => return Err(validation("Provide exactly one of userId or email")),
        };
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        if space.status != SpaceStatus::Active {
            return Err(state_conflict(
                "Archived meeting spaces cannot change membership",
            ));
        }
        let actor_role = self
            .manager_role(&txn, &space, actor, SpacePermission::ManageMembers)
            .await?;
        check_grant(actor_role, req.role, &PermissionContext::for_space(&space))
            .map_err(|d| permission_denied(d.message()))?;
        if let Some(u) = user_id {
            if u == actor.user_id {
                return Err(permission_denied("You cannot change your own membership"));
            }
            if Some(u) == space.owner_user_id {
                return Err(validation(
                    "The owner is not a member; ownership already grants full authority",
                ));
            }
            users::Entity::find_by_id(u)
                .one(&txn)
                .await?
                .filter(|x| {
                    x.status != users::UserStatus::Inactive && x.role != users::UserRole::Guest
                })
                .ok_or_else(|| validation("userId must reference an active registered user"))?;
        }
        let now = Utc::now();
        let member = meeting_members::ActiveModel {
            id: Set(Uuid::new_v4()),
            meeting_space_id: Set(space_id),
            principal_type: Set(principal_type),
            principal_ref: Set(principal_ref),
            user_id: Set(user_id),
            role: Set(req.role),
            status: Set(MemberStatus::Active),
            created_by: Set(Some(actor.user_id)),
            created_at: Set(now),
            updated_at: Set(now),
            revoked_at: Set(None),
            revoked_by: Set(None),
            version: Set(1),
        }
        .insert(&txn)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                ApiError::domain(
                    ErrorCode::ValidationError,
                    "This principal is already an active member",
                )
            } else {
                e.into()
            }
        })?;
        audit(
            &txn,
            &AuditActor::from(ctx),
            AuditRecord {
                meeting_space_id: space_id,
                conference_id: None,
                action: action::MEMBER_ADD,
                target_type: Some("MEMBER"),
                target_ref: Some(member.id.to_string()),
                metadata: json!({ "role": member.role, "principalType": member.principal_type }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(member)
    }

    async fn load_scoped<C: sea_orm::ConnectionTrait>(
        conn: &C,
        space_id: Uuid,
        member_id: Uuid,
    ) -> ApiResult<meeting_members::Model> {
        meeting_members::Entity::find_by_id(member_id)
            .filter(meeting_members::Column::MeetingSpaceId.eq(space_id))
            .one(conn)
            .await?
            .ok_or_else(member_not_found)
    }

    pub async fn update(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        member_id: Uuid,
        req: UpdateMemberRequest,
    ) -> ApiResult<meeting_members::Model> {
        let actor = ctx.require_registered()?;
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        let actor_role = self
            .manager_role(&txn, &space, actor, SpacePermission::ManageMembers)
            .await?;
        let member = Self::load_scoped(&txn, space_id, member_id).await?;
        if member.user_id == Some(actor.user_id) {
            return Err(permission_denied("You cannot change your own membership"));
        }
        if member.status != MemberStatus::Active {
            return Err(state_conflict("Revoked memberships cannot be changed"));
        }
        if member.version != req.version {
            return Err(ApiError::domain(
                ErrorCode::MeetingVersionConflict,
                "Membership was modified",
            ));
        }
        let pctx = PermissionContext::for_space(&space);
        if member.role.rank() >= actor_role.rank() {
            return Err(permission_denied(
                "You can only change members less privileged than you",
            ));
        }
        check_grant(actor_role, req.role, &pctx).map_err(|d| permission_denied(d.message()))?;
        let previous = member.role;
        let mut am: meeting_members::ActiveModel = member.clone().into();
        am.role = Set(req.role);
        am.version = Set(member.version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &AuditActor::from(ctx),
            AuditRecord {
                meeting_space_id: space_id,
                conference_id: None,
                action: action::MEMBER_UPDATE,
                target_type: Some("MEMBER"),
                target_ref: Some(member_id.to_string()),
                metadata: json!({ "from": previous, "to": req.role }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }

    pub async fn revoke(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        member_id: Uuid,
    ) -> ApiResult<meeting_members::Model> {
        let actor = ctx.require_registered()?;
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        let actor_role = self
            .manager_role(&txn, &space, actor, SpacePermission::ManageMembers)
            .await?;
        let member = Self::load_scoped(&txn, space_id, member_id).await?;
        if member.status == MemberStatus::Revoked {
            txn.commit().await?;
            return Ok(member);
        }
        if member.user_id != Some(actor.user_id) && member.role.rank() >= actor_role.rank() {
            return Err(permission_denied(
                "You can only revoke members less privileged than you",
            ));
        }
        let now = Utc::now();
        let mut am: meeting_members::ActiveModel = member.clone().into();
        am.status = Set(MemberStatus::Revoked);
        am.revoked_at = Set(Some(now));
        am.revoked_by = Set(Some(actor.user_id));
        am.version = Set(member.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &AuditActor::from(ctx),
            AuditRecord {
                meeting_space_id: space_id,
                conference_id: None,
                action: action::MEMBER_REVOKE,
                target_type: Some("MEMBER"),
                target_ref: Some(member_id.to_string()),
                metadata: json!({ "role": member.role }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }
}

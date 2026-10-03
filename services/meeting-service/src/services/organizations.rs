//! Organizations — authoritative CF organization membership (A–D closure).
//!
//! `DbOrganizationDirectory` is the production [`OrganizationDirectory`]: membership is
//! resolved server-side from `organization_memberships` on every request, and only an
//! ACTIVE membership of an ACTIVE organization held by a non-deactivated account counts.
//! Clients can never assert organization membership.

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, Set, Statement, TransactionTrait,
};
use uuid::Uuid;

use crate::{
    domain::{OrganizationMemberStatus, OrganizationRole, OrganizationStatus},
    error::{ApiError, ApiResult, ErrorCode},
    models::{organization_memberships, organizations, users},
    services::{
        actor::{ActorContext, OrganizationDirectory, RegisteredActor},
        meeting_core::{is_unique_violation, permission_denied, validation, Core},
    },
};

/// Production organization directory backed by PostgreSQL.
pub struct DbOrganizationDirectory {
    db: DatabaseConnection,
}

impl DbOrganizationDirectory {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl OrganizationDirectory for DbOrganizationDirectory {
    async fn organizations_for_user(&self, user_id: Uuid) -> Vec<Uuid> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"SELECT m.organization_id
                     FROM organization_memberships m
                     JOIN organizations o ON o.id = m.organization_id
                     JOIN users u ON u.id = m.user_id
                    WHERE m.user_id = $1 AND m.status = 'ACTIVE' AND o.status = 'ACTIVE'
                      AND u.status::text <> 'inactive'"#,
                [user_id.into()],
            ))
            .await;
        match rows {
            Ok(rows) => rows
                .iter()
                .filter_map(|r| r.try_get_by_index::<Uuid>(0).ok())
                .collect(),
            // Fail closed: an unreadable directory grants no organization membership.
            Err(e) => {
                tracing::error!("organization directory unavailable: {e}");
                vec![]
            }
        }
    }
}

use sea_orm::ConnectionTrait;

fn org_not_found() -> ApiError {
    ApiError::domain(ErrorCode::MeetingNotFound, "Organization not found")
}

pub fn validate_slug(slug: &str) -> ApiResult<()> {
    let ok = !slug.is_empty()
        && slug.len() <= 100
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(validation("slug must be lowercase letters, digits and '-'"))
    }
}

pub struct OrganizationService {
    core: Core,
}

impl OrganizationService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    fn require_platform_admin(actor: &RegisteredActor) -> ApiResult<()> {
        if actor.platform_admin {
            Ok(())
        } else {
            Err(permission_denied("Platform administrator role required"))
        }
    }

    async fn find_org<C: ConnectionTrait>(conn: &C, id: Uuid) -> ApiResult<organizations::Model> {
        organizations::Entity::find_by_id(id)
            .one(conn)
            .await?
            .ok_or_else(org_not_found)
    }

    async fn current_membership<C: ConnectionTrait>(
        conn: &C,
        org: Uuid,
        user: Uuid,
    ) -> ApiResult<Option<organization_memberships::Model>> {
        Ok(organization_memberships::Entity::find()
            .filter(organization_memberships::Column::OrganizationId.eq(org))
            .filter(organization_memberships::Column::UserId.eq(user))
            .filter(organization_memberships::Column::Status.ne(OrganizationMemberStatus::Removed))
            .one(conn)
            .await?)
    }

    /// Role of the actor as an ACTIVE manager (OWNER/ADMIN) of an ACTIVE organization.
    async fn manager_role<C: ConnectionTrait>(
        conn: &C,
        org: &organizations::Model,
        actor: &RegisteredActor,
    ) -> ApiResult<Option<OrganizationRole>> {
        if actor.platform_admin {
            return Ok(Some(OrganizationRole::Owner));
        }
        if org.status != OrganizationStatus::Active {
            return Ok(None);
        }
        Ok(Self::current_membership(conn, org.id, actor.user_id)
            .await?
            .filter(|m| m.status == OrganizationMemberStatus::Active)
            .map(|m| m.role)
            .filter(|r| matches!(r, OrganizationRole::Owner | OrganizationRole::Admin)))
    }

    /// Platform admin creates an organization, optionally with an initial OWNER.
    pub async fn create(
        &self,
        ctx: &ActorContext,
        name: &str,
        slug: &str,
        owner_user_id: Option<Uuid>,
    ) -> ApiResult<organizations::Model> {
        let actor = ctx.require_registered()?;
        Self::require_platform_admin(actor)?;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(validation("name must be between 1 and 200 characters"));
        }
        validate_slug(slug)?;
        let txn = self.core.db.begin().await?;
        let now = Utc::now();
        let org = organizations::ActiveModel {
            id: Set(Uuid::new_v4()),
            name: Set(name.to_string()),
            slug: Set(slug.to_string()),
            status: Set(OrganizationStatus::Active),
            created_by: Set(Some(actor.user_id)),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                validation("slug is already in use")
            } else {
                e.into()
            }
        })?;
        if let Some(owner) = owner_user_id {
            Self::insert_membership(&txn, org.id, owner, OrganizationRole::Owner, actor.user_id)
                .await?;
        }
        txn.commit().await?;
        tracing::info!(organization_id = %org.id, by = %actor.user_id, "organization created");
        Ok(org)
    }

    async fn insert_membership<C: ConnectionTrait>(
        conn: &C,
        org: Uuid,
        user: Uuid,
        role: OrganizationRole,
        by: Uuid,
    ) -> ApiResult<organization_memberships::Model> {
        users::Entity::find_by_id(user)
            .one(conn)
            .await?
            .filter(|u| u.status != users::UserStatus::Inactive && u.role != users::UserRole::Guest)
            .ok_or_else(|| validation("userId must reference an active registered user"))?;
        let now = Utc::now();
        Ok(organization_memberships::ActiveModel {
            id: Set(Uuid::new_v4()),
            organization_id: Set(org),
            user_id: Set(user),
            role: Set(role),
            status: Set(OrganizationMemberStatus::Active),
            created_by: Set(Some(by)),
            created_at: Set(now),
            updated_at: Set(now),
            removed_at: Set(None),
        }
        .insert(conn)
        .await?)
    }

    pub async fn set_status(
        &self,
        ctx: &ActorContext,
        org_id: Uuid,
        status: OrganizationStatus,
    ) -> ApiResult<organizations::Model> {
        let actor = ctx.require_registered()?;
        Self::require_platform_admin(actor)?;
        let org = Self::find_org(&self.core.db, org_id).await?;
        let mut am: organizations::ActiveModel = org.into();
        am.status = Set(status);
        am.updated_at = Set(Utc::now());
        let org = am.update(&self.core.db).await?;
        tracing::info!(organization_id = %org.id, %status, by = %actor.user_id, "organization status changed");
        Ok(org)
    }

    /// Create or change a membership. Platform admins and org OWNERs manage any
    /// non-owner role; org ADMINs manage MEMBERs only; OWNER is granted only by a
    /// platform admin or an existing OWNER; nobody changes their own membership.
    pub async fn upsert_member(
        &self,
        ctx: &ActorContext,
        org_id: Uuid,
        user_id: Uuid,
        role: OrganizationRole,
        status: OrganizationMemberStatus,
    ) -> ApiResult<organization_memberships::Model> {
        let actor = ctx.require_registered()?;
        let txn = self.core.db.begin().await?;
        let org = Self::find_org(&txn, org_id).await?;
        let actor_role = Self::manager_role(&txn, &org, actor)
            .await?
            .ok_or_else(|| permission_denied("You cannot manage this organization's members"))?;
        if user_id == actor.user_id && !actor.platform_admin {
            return Err(permission_denied(
                "You cannot change your own organization membership",
            ));
        }
        let existing = Self::current_membership(&txn, org_id, user_id).await?;
        let touches_privileged = role != OrganizationRole::Member
            || existing
                .as_ref()
                .map(|m| m.role != OrganizationRole::Member)
                .unwrap_or(false);
        if actor_role == OrganizationRole::Admin && touches_privileged {
            return Err(permission_denied(
                "Organization admins can only manage members",
            ));
        }
        let now = Utc::now();
        let membership = match existing {
            Some(m) => {
                let mut am: organization_memberships::ActiveModel = m.into();
                am.role = Set(role);
                am.status = Set(status);
                am.removed_at = Set((status == OrganizationMemberStatus::Removed).then_some(now));
                am.updated_at = Set(now);
                am.update(&txn).await?
            }
            None if status == OrganizationMemberStatus::Removed => {
                return Err(validation("No membership to remove"));
            }
            None => {
                let mut m =
                    Self::insert_membership(&txn, org_id, user_id, role, actor.user_id).await?;
                if status != OrganizationMemberStatus::Active {
                    let mut am: organization_memberships::ActiveModel = m.into();
                    am.status = Set(status);
                    m = am.update(&txn).await?;
                }
                m
            }
        };
        txn.commit().await?;
        tracing::info!(organization_id = %org_id, user_id = %user_id, %role, %status, by = %actor.user_id, "organization membership changed");
        Ok(membership)
    }

    pub async fn list_members(
        &self,
        ctx: &ActorContext,
        org_id: Uuid,
    ) -> ApiResult<Vec<organization_memberships::Model>> {
        let actor = ctx.require_registered()?;
        let org = Self::find_org(&self.core.db, org_id).await?;
        Self::manager_role(&self.core.db, &org, actor)
            .await?
            .ok_or_else(|| permission_denied("You cannot view this organization's members"))?;
        Ok(organization_memberships::Entity::find()
            .filter(organization_memberships::Column::OrganizationId.eq(org_id))
            .order_by_asc(organization_memberships::Column::CreatedAt)
            .all(&self.core.db)
            .await?)
    }

    /// Organizations the caller verifiably belongs to (resolved server-side).
    pub async fn mine(
        &self,
        ctx: &ActorContext,
    ) -> ApiResult<Vec<(organizations::Model, OrganizationRole)>> {
        let actor = ctx.require_registered()?;
        let memberships = organization_memberships::Entity::find()
            .filter(organization_memberships::Column::UserId.eq(actor.user_id))
            .filter(organization_memberships::Column::Status.eq(OrganizationMemberStatus::Active))
            .all(&self.core.db)
            .await?;
        let mut out = vec![];
        for m in memberships {
            if !ctx.verified_organizations.contains(&m.organization_id) {
                continue;
            }
            if let Some(org) = organizations::Entity::find_by_id(m.organization_id)
                .one(&self.core.db)
                .await?
            {
                out.push((org, m.role));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rules() {
        assert!(validate_slug("acme-health").is_ok());
        assert!(validate_slug("Acme").is_err());
        assert!(validate_slug("-x").is_err());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("a b").is_err());
    }
}

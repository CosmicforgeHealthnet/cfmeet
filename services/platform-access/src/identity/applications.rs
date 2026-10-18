//! Applications / projects: every consumer product — first-party or external — is an
//! application of a tenant. There is no product-specific schema or code path.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde::Serialize;

use super::{display_name, PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{ApplicationId, ApplicationStatus, Slug, TenantId},
    models::applications,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationView {
    pub id: ApplicationId,
    pub tenant_id: TenantId,
    pub slug: String,
    pub display_name: String,
    pub status: ApplicationStatus,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<applications::Model> for ApplicationView {
    fn from(m: applications::Model) -> Self {
        Self {
            id: ApplicationId::from_uuid(m.id),
            tenant_id: TenantId::from_uuid(m.tenant_id),
            slug: m.slug,
            display_name: m.display_name,
            status: m.status,
            revision: m.revision,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewApplication {
    pub slug: String,
    pub display_name: String,
}

impl PlatformService {
    /// Creates an application owned by `tenant_id` (never re-parentable).
    pub async fn create_application(
        &self,
        tenant_id: TenantId,
        new: NewApplication,
    ) -> PlatformResult<ApplicationView> {
        self.create_application_in(&self.db, tenant_id, new).await
    }

    pub(crate) async fn create_application_in<C: ConnectionTrait>(
        &self,
        db: &C,
        tenant_id: TenantId,
        new: NewApplication,
    ) -> PlatformResult<ApplicationView> {
        let slug = Slug::parse(&new.slug).map_err(PlatformError::Validation)?;
        let display_name = display_name(&new.display_name)?;
        let tenant = self.tenant(tenant_id).await?;
        let now = self.clock.now();
        let model = applications::ActiveModel {
            id: Set(ApplicationId::generate().as_uuid()),
            tenant_id: Set(tenant.id.as_uuid()),
            slug: Set(slug.as_str().to_string()),
            display_name: Set(display_name),
            status: Set(ApplicationStatus::Active),
            revision: sea_orm::ActiveValue::NotSet,
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(|e| match PlatformError::from(e) {
            PlatformError::Conflict(_) => PlatformError::Conflict(format!(
                "application `{slug}` already exists in tenant `{}`",
                tenant.slug
            )),
            other => other,
        })?;
        tracing::info!(tenant_id = %model.tenant_id, application_id = %model.id, "application created");
        Ok(model.into())
    }

    pub async fn application(&self, id: ApplicationId) -> PlatformResult<ApplicationView> {
        applications::Entity::find_by_id(id.as_uuid())
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("application"))
    }

    pub async fn application_by_slug(
        &self,
        tenant_id: TenantId,
        slug: &str,
    ) -> PlatformResult<ApplicationView> {
        applications::Entity::find()
            .filter(applications::Column::TenantId.eq(tenant_id.as_uuid()))
            .filter(applications::Column::Slug.eq(slug))
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("application"))
    }

    pub async fn list_applications(
        &self,
        tenant_id: TenantId,
    ) -> PlatformResult<Vec<ApplicationView>> {
        Ok(applications::Entity::find()
            .filter(applications::Column::TenantId.eq(tenant_id.as_uuid()))
            .order_by_asc(applications::Column::Slug)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn set_application_status(
        &self,
        id: ApplicationId,
        status: ApplicationStatus,
    ) -> PlatformResult<ApplicationView> {
        self.set_application_status_in(&self.db, id, status).await
    }

    pub(crate) async fn set_application_status_in<C: ConnectionTrait>(
        &self,
        db: &C,
        id: ApplicationId,
        status: ApplicationStatus,
    ) -> PlatformResult<ApplicationView> {
        let model = applications::Entity::find_by_id(id.as_uuid())
            .one(db)
            .await?
            .ok_or(PlatformError::NotFound("application"))?;
        if model.status == status {
            return Ok(model.into());
        }
        let mut active: applications::ActiveModel = model.into();
        active.status = Set(status);
        active.updated_at = Set(self.clock.now());
        let model = active.update(db).await?;
        tracing::info!(application_id = %model.id, status = %model.status, "application status changed");
        Ok(model.into())
    }
}

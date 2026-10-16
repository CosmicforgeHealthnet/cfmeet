//! Applications / projects: every consumer product — first-party or external — is an
//! application of a tenant. There is no product-specific schema or code path.
//!
//! Read-only projection of the Platform Access authority (ADR 0032).

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;

use super::{PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{ApplicationId, ApplicationStatus, TenantId},
    models::applications,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplicationView {
    pub id: ApplicationId,
    pub tenant_id: TenantId,
    pub slug: String,
    pub display_name: String,
    pub status: ApplicationStatus,
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
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl PlatformService {
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
}

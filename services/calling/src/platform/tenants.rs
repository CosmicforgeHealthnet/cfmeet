//! Tenants: the top-level security, ownership, policy and usage boundary.
//!
//! Read-only projection of the Platform Access authority (ADR 0032): rows are written by
//! [`super::projection`] only.

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;

use super::{PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{TenantClassification, TenantId, TenantStatus},
    models::tenants,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TenantView {
    pub id: TenantId,
    pub slug: String,
    pub display_name: String,
    pub classification: TenantClassification,
    pub status: TenantStatus,
    pub billing_currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<tenants::Model> for TenantView {
    fn from(m: tenants::Model) -> Self {
        Self {
            id: TenantId::from_uuid(m.id),
            slug: m.slug,
            display_name: m.display_name,
            classification: m.classification,
            status: m.status,
            billing_currency: m.billing_currency,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl PlatformService {
    pub async fn tenant(&self, id: TenantId) -> PlatformResult<TenantView> {
        tenants::Entity::find_by_id(id.as_uuid())
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("tenant"))
    }

    pub async fn tenant_by_slug(&self, slug: &str) -> PlatformResult<TenantView> {
        tenants::Entity::find()
            .filter(tenants::Column::Slug.eq(slug))
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("tenant"))
    }

    pub async fn list_tenants(&self) -> PlatformResult<Vec<TenantView>> {
        Ok(tenants::Entity::find()
            .order_by_asc(tenants::Column::Slug)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }
}

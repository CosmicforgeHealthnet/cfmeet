//! Tenants: the top-level security, ownership, policy and usage boundary.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde::Serialize;

use super::{display_name, PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{Slug, TenantClassification, TenantId, TenantStatus},
    models::tenants,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantView {
    pub id: TenantId,
    pub slug: String,
    pub display_name: String,
    pub classification: TenantClassification,
    pub status: TenantStatus,
    pub billing_currency: String,
    pub revision: i64,
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
            revision: m.revision,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewTenant {
    pub slug: String,
    pub display_name: String,
    pub classification: TenantClassification,
    /// ISO 4217 code for telecom-spend limits; defaults to USD. Immutable once created.
    pub billing_currency: Option<String>,
}

fn billing_currency(value: Option<&str>) -> PlatformResult<String> {
    match value.map(str::trim) {
        None | Some("") => Ok("USD".to_string()),
        Some(c) if c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()) => Ok(c.to_string()),
        Some(_) => Err(PlatformError::Validation(
            "billing currency must be a three-letter upper-case ISO 4217 code".to_string(),
        )),
    }
}

impl PlatformService {
    pub async fn create_tenant(&self, new: NewTenant) -> PlatformResult<TenantView> {
        self.create_tenant_in(&self.db, new).await
    }

    pub(crate) async fn create_tenant_in<C: ConnectionTrait>(
        &self,
        db: &C,
        new: NewTenant,
    ) -> PlatformResult<TenantView> {
        let slug = Slug::parse(&new.slug).map_err(PlatformError::Validation)?;
        let display_name = display_name(&new.display_name)?;
        let currency = billing_currency(new.billing_currency.as_deref())?;
        let now = self.clock.now();
        let model = tenants::ActiveModel {
            id: Set(TenantId::generate().as_uuid()),
            slug: Set(slug.as_str().to_string()),
            display_name: Set(display_name),
            classification: Set(new.classification),
            status: Set(TenantStatus::Active),
            billing_currency: Set(currency),
            revision: sea_orm::ActiveValue::NotSet,
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(|e| match PlatformError::from(e) {
            PlatformError::Conflict(_) => {
                PlatformError::Conflict(format!("tenant `{slug}` already exists"))
            }
            other => other,
        })?;
        tracing::info!(tenant_id = %model.id, classification = %model.classification, "tenant created");
        Ok(model.into())
    }

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

    /// Suspends or reactivates a tenant. A suspended tenant fails authentication through
    /// every subordinate credential immediately (no caches).
    pub async fn set_tenant_status(
        &self,
        id: TenantId,
        status: TenantStatus,
    ) -> PlatformResult<TenantView> {
        self.set_tenant_status_in(&self.db, id, status).await
    }

    pub(crate) async fn set_tenant_status_in<C: ConnectionTrait>(
        &self,
        db: &C,
        id: TenantId,
        status: TenantStatus,
    ) -> PlatformResult<TenantView> {
        let model = tenants::Entity::find_by_id(id.as_uuid())
            .one(db)
            .await?
            .ok_or(PlatformError::NotFound("tenant"))?;
        if model.status == status {
            return Ok(model.into());
        }
        let mut active: tenants::ActiveModel = model.into();
        active.status = Set(status);
        active.updated_at = Set(self.clock.now());
        let model = active.update(db).await?;
        tracing::info!(tenant_id = %model.id, status = %model.status, "tenant status changed");
        Ok(model.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billing_currency_validation() {
        assert_eq!(billing_currency(None).unwrap(), "USD");
        assert_eq!(billing_currency(Some("NGN")).unwrap(), "NGN");
        assert!(billing_currency(Some("ngn")).is_err());
        assert!(billing_currency(Some("NAIRA")).is_err());
    }
}

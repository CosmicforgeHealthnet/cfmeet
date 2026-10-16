//! Service accounts: machine identities bound to exactly one application environment.
//!
//! Read-only projection of the Platform Access authority (ADR 0032).

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;

use super::{PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{
        ApplicationId, EnvironmentId, ServiceAccountId, ServiceAccountStatus, TenantId,
    },
    models::service_accounts,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceAccountView {
    pub id: ServiceAccountId,
    pub tenant_id: TenantId,
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub description: Option<String>,
    pub status: ServiceAccountStatus,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<service_accounts::Model> for ServiceAccountView {
    fn from(m: service_accounts::Model) -> Self {
        Self {
            id: ServiceAccountId::from_uuid(m.id),
            tenant_id: TenantId::from_uuid(m.tenant_id),
            application_id: ApplicationId::from_uuid(m.application_id),
            environment_id: EnvironmentId::from_uuid(m.environment_id),
            name: m.name,
            description: m.description,
            status: m.status,
            revoked_at: m.revoked_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl PlatformService {
    pub async fn service_account(
        &self,
        id: ServiceAccountId,
    ) -> PlatformResult<ServiceAccountView> {
        service_accounts::Entity::find_by_id(id.as_uuid())
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("service account"))
    }

    pub async fn service_account_by_name(
        &self,
        environment_id: EnvironmentId,
        name: &str,
    ) -> PlatformResult<ServiceAccountView> {
        service_accounts::Entity::find()
            .filter(service_accounts::Column::EnvironmentId.eq(environment_id.as_uuid()))
            .filter(service_accounts::Column::Name.eq(name))
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("service account"))
    }

    pub async fn list_service_accounts(
        &self,
        environment_id: EnvironmentId,
    ) -> PlatformResult<Vec<ServiceAccountView>> {
        Ok(service_accounts::Entity::find()
            .filter(service_accounts::Column::EnvironmentId.eq(environment_id.as_uuid()))
            .order_by_asc(service_accounts::Column::Name)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

}

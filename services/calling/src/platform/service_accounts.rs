//! Service accounts: machine identities bound to exactly one application environment.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde::Serialize;

use super::{optional_text, PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{
        ApplicationId, EnvironmentId, ServiceAccountId, ServiceAccountStatus, Slug, TenantId,
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

#[derive(Debug, Clone)]
pub struct NewServiceAccount {
    pub name: String,
    pub description: Option<String>,
}

impl PlatformService {
    /// Creates a service account inside `environment_id`. Tenant and application are
    /// taken from the persisted environment, never from the caller.
    pub async fn create_service_account(
        &self,
        environment_id: EnvironmentId,
        new: NewServiceAccount,
    ) -> PlatformResult<ServiceAccountView> {
        self.create_service_account_in(&self.db, environment_id, new)
            .await
    }

    pub(crate) async fn create_service_account_in<C: ConnectionTrait>(
        &self,
        db: &C,
        environment_id: EnvironmentId,
        new: NewServiceAccount,
    ) -> PlatformResult<ServiceAccountView> {
        let name = Slug::parse(&new.name).map_err(PlatformError::Validation)?;
        let description = optional_text(new.description.as_deref(), "description", 500)?;
        let environment = self.environment(environment_id).await?;
        let now = self.clock.now();
        let model = service_accounts::ActiveModel {
            id: Set(ServiceAccountId::generate().as_uuid()),
            tenant_id: Set(environment.tenant_id.as_uuid()),
            application_id: Set(environment.application_id.as_uuid()),
            environment_id: Set(environment.id.as_uuid()),
            name: Set(name.as_str().to_string()),
            description: Set(description),
            status: Set(ServiceAccountStatus::Active),
            revoked_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(|e| match PlatformError::from(e) {
            PlatformError::Conflict(_) => PlatformError::Conflict(format!(
                "service account `{name}` already exists in this environment"
            )),
            other => other,
        })?;
        tracing::info!(
            environment_id = %model.environment_id,
            service_account_id = %model.id,
            "service account created"
        );
        Ok(model.into())
    }

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

    /// ACTIVE ⇄ SUSPENDED, or → REVOKED (terminal). Revocation is idempotent.
    pub async fn set_service_account_status(
        &self,
        id: ServiceAccountId,
        status: ServiceAccountStatus,
    ) -> PlatformResult<ServiceAccountView> {
        self.set_service_account_status_in(&self.db, id, status)
            .await
    }

    pub(crate) async fn set_service_account_status_in<C: ConnectionTrait>(
        &self,
        db: &C,
        id: ServiceAccountId,
        status: ServiceAccountStatus,
    ) -> PlatformResult<ServiceAccountView> {
        let model = service_accounts::Entity::find_by_id(id.as_uuid())
            .one(db)
            .await?
            .ok_or(PlatformError::NotFound("service account"))?;
        if model.status == status {
            return Ok(model.into());
        }
        if model.status == ServiceAccountStatus::Revoked {
            return Err(PlatformError::InvalidState(
                "a revoked service account cannot be reactivated or suspended".to_string(),
            ));
        }
        let now = self.clock.now();
        let mut active: service_accounts::ActiveModel = model.into();
        active.status = Set(status);
        if status == ServiceAccountStatus::Revoked {
            active.revoked_at = Set(Some(now));
        }
        active.updated_at = Set(now);
        let model = active.update(db).await?;
        tracing::info!(service_account_id = %model.id, status = %model.status, "service account status changed");
        Ok(model.into())
    }
}

//! Application environments: SANDBOX, TEST and PRODUCTION, isolated from each other.
//!
//! The environment of a request is always the persisted environment of the credential
//! that authenticated it — never a request field. `live_telecom_allowed` is the
//! server-authoritative switch future telecom work must consult before producing any real
//! public-network effect; it defaults to `false` and can only be enabled for PRODUCTION
//! (validated here and by a database `CHECK`).

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde::Serialize;

use super::{PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{ApplicationId, EnvironmentId, EnvironmentKind, EnvironmentStatus, TenantId},
    models::environments,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentView {
    pub id: EnvironmentId,
    pub tenant_id: TenantId,
    pub application_id: ApplicationId,
    pub kind: EnvironmentKind,
    pub status: EnvironmentStatus,
    pub live_telecom_allowed: bool,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<environments::Model> for EnvironmentView {
    fn from(m: environments::Model) -> Self {
        Self {
            id: EnvironmentId::from_uuid(m.id),
            tenant_id: TenantId::from_uuid(m.tenant_id),
            application_id: ApplicationId::from_uuid(m.application_id),
            kind: m.kind,
            status: m.status,
            live_telecom_allowed: m.live_telecom_allowed,
            revision: m.revision,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl PlatformService {
    /// Creates the `kind` environment of an application. The tenant is taken from the
    /// persisted application, never from the caller.
    pub async fn create_environment(
        &self,
        application_id: ApplicationId,
        kind: EnvironmentKind,
    ) -> PlatformResult<EnvironmentView> {
        self.create_environment_in(&self.db, application_id, kind)
            .await
    }

    pub(crate) async fn create_environment_in<C: ConnectionTrait>(
        &self,
        db: &C,
        application_id: ApplicationId,
        kind: EnvironmentKind,
    ) -> PlatformResult<EnvironmentView> {
        let application = self.application(application_id).await?;
        let now = self.clock.now();
        let model = environments::ActiveModel {
            id: Set(EnvironmentId::generate().as_uuid()),
            tenant_id: Set(application.tenant_id.as_uuid()),
            application_id: Set(application.id.as_uuid()),
            kind: Set(kind),
            status: Set(EnvironmentStatus::Active),
            live_telecom_allowed: Set(false),
            revision: sea_orm::ActiveValue::NotSet,
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(|e| match PlatformError::from(e) {
            PlatformError::Conflict(_) => PlatformError::Conflict(format!(
                "application `{}` already has a {kind} environment",
                application.slug
            )),
            other => other,
        })?;
        tracing::info!(
            application_id = %model.application_id,
            environment_id = %model.id,
            kind = %model.kind,
            "environment created"
        );
        Ok(model.into())
    }

    pub async fn environment(&self, id: EnvironmentId) -> PlatformResult<EnvironmentView> {
        environments::Entity::find_by_id(id.as_uuid())
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("environment"))
    }

    pub async fn environment_by_kind(
        &self,
        application_id: ApplicationId,
        kind: EnvironmentKind,
    ) -> PlatformResult<EnvironmentView> {
        environments::Entity::find()
            .filter(environments::Column::ApplicationId.eq(application_id.as_uuid()))
            .filter(environments::Column::Kind.eq(kind))
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("environment"))
    }

    pub async fn list_environments(
        &self,
        application_id: ApplicationId,
    ) -> PlatformResult<Vec<EnvironmentView>> {
        Ok(environments::Entity::find()
            .filter(environments::Column::ApplicationId.eq(application_id.as_uuid()))
            .order_by_asc(environments::Column::Kind)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn set_environment_status(
        &self,
        id: EnvironmentId,
        status: EnvironmentStatus,
    ) -> PlatformResult<EnvironmentView> {
        self.set_environment_status_in(&self.db, id, status).await
    }

    pub(crate) async fn set_environment_status_in<C: ConnectionTrait>(
        &self,
        db: &C,
        id: EnvironmentId,
        status: EnvironmentStatus,
    ) -> PlatformResult<EnvironmentView> {
        let model = environments::Entity::find_by_id(id.as_uuid())
            .one(db)
            .await?
            .ok_or(PlatformError::NotFound("environment"))?;
        if model.status == status {
            return Ok(model.into());
        }
        let mut active: environments::ActiveModel = model.into();
        active.status = Set(status);
        active.updated_at = Set(self.clock.now());
        let model = active.update(db).await?;
        tracing::info!(environment_id = %model.id, status = %model.status, "environment status changed");
        Ok(model.into())
    }

    /// Enables / disables real public-network telecom effects for a PRODUCTION
    /// environment. SANDBOX and TEST can never be enabled.
    pub async fn set_live_telecom_allowed(
        &self,
        id: EnvironmentId,
        allowed: bool,
    ) -> PlatformResult<EnvironmentView> {
        self.set_live_telecom_allowed_in(&self.db, id, allowed)
            .await
    }

    pub(crate) async fn set_live_telecom_allowed_in<C: ConnectionTrait>(
        &self,
        db: &C,
        id: EnvironmentId,
        allowed: bool,
    ) -> PlatformResult<EnvironmentView> {
        let model = environments::Entity::find_by_id(id.as_uuid())
            .one(db)
            .await?
            .ok_or(PlatformError::NotFound("environment"))?;
        if allowed && !model.kind.may_allow_live_telecom() {
            return Err(PlatformError::PolicyViolation(format!(
                "live telecom can never be enabled for a {} environment",
                model.kind
            )));
        }
        if model.live_telecom_allowed == allowed {
            return Ok(model.into());
        }
        let mut active: environments::ActiveModel = model.into();
        active.live_telecom_allowed = Set(allowed);
        active.updated_at = Set(self.clock.now());
        let model = active.update(db).await?;
        tracing::info!(
            environment_id = %model.id,
            live_telecom_allowed = model.live_telecom_allowed,
            "environment live-telecom policy changed"
        );
        Ok(model.into())
    }
}

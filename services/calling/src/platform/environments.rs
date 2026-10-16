//! Application environments: SANDBOX, TEST and PRODUCTION, isolated from each other.
//!
//! The environment of a request is always the persisted environment of the credential
//! that authenticated it — never a request field. `live_telecom_allowed` is the
//! server-authoritative switch future telecom work must consult before producing any real
//! public-network effect; it defaults to `false` and can only be enabled for PRODUCTION
//! (validated by Platform Access and by a database `CHECK`).
//!
//! Read-only projection of the Platform Access authority (ADR 0032).

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;

use super::{PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{ApplicationId, EnvironmentId, EnvironmentKind, EnvironmentStatus, TenantId},
    models::environments,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentView {
    pub id: EnvironmentId,
    pub tenant_id: TenantId,
    pub application_id: ApplicationId,
    pub kind: EnvironmentKind,
    pub status: EnvironmentStatus,
    pub live_telecom_allowed: bool,
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
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl PlatformService {
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
}

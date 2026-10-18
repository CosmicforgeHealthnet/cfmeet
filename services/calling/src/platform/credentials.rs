//! API credentials as seen by Calling: a read-only projection of the Platform Access
//! authority (ADR 0032). Issuance, rotation, revocation and verification happen in
//! Platform Access; Calling reads the projected rows for its own policy (limits,
//! inbound owners, audit) and never holds — or needs — verifier material for them.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;

use super::{PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{
        ApplicationId, CredentialId, CredentialStatus, EnvironmentId, EnvironmentKind,
        ServiceAccountId, TenantId,
    },
    models::{api_credential_scopes, api_credentials, environments},
};

/// Time-dependent state of a credential, derived from persisted fields. Authentication
/// accepts only [`Active`](CredentialEffectiveState::Active) and
/// [`RotationOverlap`](CredentialEffectiveState::RotationOverlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CredentialEffectiveState {
    Active,
    /// Superseded by rotation, still inside its overlap window.
    RotationOverlap,
    NotYetValid,
    Expired,
    /// Superseded by rotation; overlap window ended.
    Superseded,
    Revoked,
}

impl CredentialEffectiveState {
    pub fn evaluate(
        status: CredentialStatus,
        not_before: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        supersession_valid_until: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        if status == CredentialStatus::Revoked {
            return Self::Revoked;
        }
        if now < not_before {
            return Self::NotYetValid;
        }
        if now >= expires_at {
            return Self::Expired;
        }
        match supersession_valid_until {
            Some(until) if now >= until => Self::Superseded,
            Some(_) => Self::RotationOverlap,
            None => Self::Active,
        }
    }

    pub fn authenticates(&self) -> bool {
        matches!(self, Self::Active | Self::RotationOverlap)
    }
}

/// Safe representation of a credential: no secret, no verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialView {
    pub id: CredentialId,
    pub public_id: String,
    pub masked: String,
    pub tenant_id: TenantId,
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub environment_kind: EnvironmentKind,
    pub service_account_id: ServiceAccountId,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    pub status: CredentialStatus,
    pub effective_state: CredentialEffectiveState,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub rotated_from_credential_id: Option<CredentialId>,
    pub superseded_by_credential_id: Option<CredentialId>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub supersession_valid_until: Option<DateTime<Utc>>,
    /// Only on Part III rows awaiting the hand-off completion (never the verifier).
    pub verifier_algorithm: Option<String>,
    pub verifier_key_version: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Which credentials to list.
#[derive(Debug, Clone, Copy)]
pub enum CredentialFilter {
    ServiceAccount(ServiceAccountId),
    Environment(EnvironmentId),
    Application(ApplicationId),
}

impl PlatformService {
    pub async fn credential(&self, id: CredentialId) -> PlatformResult<CredentialView> {
        self.credential_in(&self.db, id).await
    }

    pub(crate) async fn credential_in<C: ConnectionTrait>(
        &self,
        db: &C,
        id: CredentialId,
    ) -> PlatformResult<CredentialView> {
        let model = api_credentials::Entity::find_by_id(id.as_uuid())
            .one(db)
            .await?
            .ok_or(PlatformError::NotFound("credential"))?;
        Ok(self.views_in(db, vec![model]).await?.remove(0))
    }

    pub async fn credential_by_public_id(&self, public_id: &str) -> PlatformResult<CredentialView> {
        let model = api_credentials::Entity::find()
            .filter(api_credentials::Column::PublicId.eq(public_id))
            .one(&self.db)
            .await?
            .ok_or(PlatformError::NotFound("credential"))?;
        Ok(self.views(vec![model]).await?.remove(0))
    }

    pub async fn list_credentials(
        &self,
        filter: CredentialFilter,
    ) -> PlatformResult<Vec<CredentialView>> {
        let query = api_credentials::Entity::find();
        let query = match filter {
            CredentialFilter::ServiceAccount(id) => {
                query.filter(api_credentials::Column::ServiceAccountId.eq(id.as_uuid()))
            }
            CredentialFilter::Environment(id) => {
                query.filter(api_credentials::Column::EnvironmentId.eq(id.as_uuid()))
            }
            CredentialFilter::Application(id) => {
                query.filter(api_credentials::Column::ApplicationId.eq(id.as_uuid()))
            }
        };
        let models = query
            .order_by_asc(api_credentials::Column::CreatedAt)
            .order_by_asc(api_credentials::Column::Id)
            .all(&self.db)
            .await?;
        self.views(models).await
    }

    async fn views(
        &self,
        models: Vec<api_credentials::Model>,
    ) -> PlatformResult<Vec<CredentialView>> {
        self.views_in(&self.db, models).await
    }

    async fn views_in<C: ConnectionTrait>(
        &self,
        db: &C,
        models: Vec<api_credentials::Model>,
    ) -> PlatformResult<Vec<CredentialView>> {
        if models.is_empty() {
            return Ok(Vec::new());
        }
        let now = self.clock.now();
        let ids: Vec<_> = models.iter().map(|m| m.id).collect();
        let env_ids: BTreeSet<_> = models.iter().map(|m| m.environment_id).collect();
        let kinds: HashMap<_, _> = environments::Entity::find()
            .filter(environments::Column::Id.is_in(env_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|e| (e.id, e.kind))
            .collect();
        let mut scopes: HashMap<_, Vec<String>> = HashMap::new();
        for row in scopes_of(db, ids).await? {
            scopes.entry(row.credential_id).or_default().push(row.scope);
        }
        models
            .into_iter()
            .map(|m| {
                let kind = *kinds
                    .get(&m.environment_id)
                    .ok_or(PlatformError::NotFound("environment"))?;
                Ok(CredentialView {
                    id: CredentialId::from_uuid(m.id),
                    masked: masked(kind, &m.public_id, &m.last_four),
                    public_id: m.public_id,
                    tenant_id: TenantId::from_uuid(m.tenant_id),
                    application_id: ApplicationId::from_uuid(m.application_id),
                    environment_id: EnvironmentId::from_uuid(m.environment_id),
                    environment_kind: kind,
                    service_account_id: ServiceAccountId::from_uuid(m.service_account_id),
                    label: m.label,
                    scopes: scopes.remove(&m.id).unwrap_or_default(),
                    status: m.status,
                    effective_state: CredentialEffectiveState::evaluate(
                        m.status,
                        m.not_before,
                        m.expires_at,
                        m.supersession_valid_until,
                        now,
                    ),
                    not_before: m.not_before,
                    expires_at: m.expires_at,
                    revoked_at: m.revoked_at,
                    rotated_from_credential_id: m
                        .rotated_from_credential_id
                        .map(CredentialId::from_uuid),
                    superseded_by_credential_id: m
                        .superseded_by_credential_id
                        .map(CredentialId::from_uuid),
                    superseded_at: m.superseded_at,
                    supersession_valid_until: m.supersession_valid_until,
                    verifier_algorithm: m.verifier_algorithm,
                    verifier_key_version: m.verifier_key_version,
                    created_at: m.created_at,
                    updated_at: m.updated_at,
                })
            })
            .collect()
    }
}

/// Safe display form of a credential (`cfmk_<tag>_<public id>_<stars><last four>`): it
/// identifies the credential without revealing anything of its secret.
fn masked(kind: EnvironmentKind, public_id: &str, last_four: &str) -> String {
    format!(
        "cfmk_{}_{public_id}_{}{last_four}",
        kind.credential_tag(),
        "*".repeat(60)
    )
}

async fn scopes_of<C: ConnectionTrait>(
    db: &C,
    credential_ids: Vec<uuid::Uuid>,
) -> PlatformResult<Vec<api_credential_scopes::Model>> {
    Ok(api_credential_scopes::Entity::find()
        .filter(api_credential_scopes::Column::CredentialId.is_in(credential_ids))
        .order_by_asc(api_credential_scopes::Column::Scope)
        .all(db)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(minutes)
    }

    #[test]
    fn effective_state_is_derived_from_persisted_fields() {
        use CredentialEffectiveState as S;
        let active = CredentialStatus::Active;
        assert_eq!(
            S::evaluate(active, t(0), t(100), None, t(-1)),
            S::NotYetValid
        );
        assert_eq!(S::evaluate(active, t(0), t(100), None, t(0)), S::Active);
        assert_eq!(S::evaluate(active, t(0), t(100), None, t(99)), S::Active);
        assert_eq!(S::evaluate(active, t(0), t(100), None, t(100)), S::Expired);
        assert_eq!(
            S::evaluate(active, t(0), t(100), Some(t(50)), t(49)),
            S::RotationOverlap
        );
        assert_eq!(
            S::evaluate(active, t(0), t(100), Some(t(50)), t(50)),
            S::Superseded
        );
        assert_eq!(
            S::evaluate(CredentialStatus::Revoked, t(0), t(100), None, t(10)),
            S::Revoked
        );
        assert_eq!(
            S::evaluate(CredentialStatus::Revoked, t(0), t(100), Some(t(50)), t(10)),
            S::Revoked
        );
    }

    #[test]
    fn only_active_and_overlap_states_authenticate() {
        use CredentialEffectiveState as S;
        assert!(S::Active.authenticates());
        assert!(S::RotationOverlap.authenticates());
        for s in [S::NotYetValid, S::Expired, S::Superseded, S::Revoked] {
            assert!(!s.authenticates());
        }
    }
}

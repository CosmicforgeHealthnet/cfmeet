//! Routing policy persistence (`call_routing_policies`). Policies are validated typed
//! documents; an environment without one gets the restrictive default.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};
use uuid::Uuid;

use super::policy::{RoutingPolicy, ROUTING_POLICY_VERSION};

/// The effective policy of an environment and the version label routes record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    pub policy: RoutingPolicy,
    /// 0 = the built-in default (no stored policy).
    pub revision: i32,
}

impl EffectivePolicy {
    /// `ROUTING_POLICY_V1.r<revision>` (`.default` without a stored policy).
    pub fn version_label(&self) -> String {
        if self.revision == 0 {
            format!("{ROUTING_POLICY_VERSION}.default")
        } else {
            format!("{ROUTING_POLICY_VERSION}.r{}", self.revision)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyStoreError {
    #[error("invalid routing policy: {0}")]
    Invalid(&'static str),
    #[error("database error")]
    Database(#[from] DbErr),
}

/// Loads the effective routing policy of `environment_id`. A stored document that no
/// longer validates fails CLOSED to the restrictive default.
pub async fn load_policy<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<EffectivePolicy, DbErr> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT policy, revision FROM call_routing_policies WHERE environment_id = $1",
            vec![environment_id.into()],
        ))
        .await?;
    let Some(row) = row else {
        return Ok(EffectivePolicy {
            policy: RoutingPolicy::default(),
            revision: 0,
        });
    };
    let value: serde_json::Value = row.try_get_by_index(0)?;
    let revision: i32 = row.try_get_by_index(1)?;
    match RoutingPolicy::parse(&value) {
        Ok(policy) => Ok(EffectivePolicy { policy, revision }),
        Err(reason) => {
            tracing::error!(%environment_id, reason, "stored routing policy is invalid; using the restrictive default");
            Ok(EffectivePolicy {
                policy: RoutingPolicy::default(),
                revision: 0,
            })
        }
    }
}

/// Stores (creates or replaces) the routing policy of an environment; returns the new
/// revision. The environment must belong to the tenant / application (foreign key).
pub async fn store_policy<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    policy: &RoutingPolicy,
    now: DateTime<Utc>,
) -> Result<i32, PolicyStoreError> {
    policy.validate().map_err(PolicyStoreError::Invalid)?;
    let doc = serde_json::to_value(policy).map_err(|_| PolicyStoreError::Invalid("encoding"))?;
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO call_routing_policies (tenant_id, application_id, environment_id, policy, revision, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 1, $5, $5) \
             ON CONFLICT (environment_id) DO UPDATE SET policy = EXCLUDED.policy, \
               revision = call_routing_policies.revision + 1, \
               updated_at = GREATEST(EXCLUDED.updated_at, call_routing_policies.updated_at) \
             WHERE call_routing_policies.tenant_id = EXCLUDED.tenant_id \
               AND call_routing_policies.application_id = EXCLUDED.application_id \
             RETURNING revision",
            vec![
                tenant_id.into(),
                application_id.into(),
                environment_id.into(),
                doc.into(),
                now.into(),
            ],
        ))
        .await?
        .ok_or(PolicyStoreError::Invalid("the environment belongs to another application"))?;
    Ok(row.try_get_by_index(0)?)
}

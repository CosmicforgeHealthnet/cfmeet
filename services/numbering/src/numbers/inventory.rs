//! Inventory observations (Part IV §D5): append-only snapshots of what a source reported
//! about a number, with freshness. An observation is evidence that a number was OFFERED;
//! it is never an allocation — allocation is a reservation, confirmed by a provider hold.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

use super::{
    resources::{lock, ResourceRow},
    stmt, Availability, Capability, CostEvidence, DomainError, DomainResult, InventorySource,
    NumberDomain, NumberType,
};
use crate::domain::RequestContext;

/// What a source reported.
#[derive(Debug, Clone)]
pub struct NewObservation {
    pub source: InventorySource,
    /// Internal provider key (`LIVE_PROVIDER` only; never exposed).
    pub provider_id: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub fresh_until: DateTime<Utc>,
    pub availability: Availability,
    pub offered_capabilities: Vec<Capability>,
    pub observed_number_type: Option<NumberType>,
    pub monthly_cost_minor: Option<i64>,
    pub setup_cost_minor: Option<i64>,
    pub currency: Option<String>,
    pub evidence_ref: Option<Uuid>,
    pub query_ref: Option<Uuid>,
}

impl NewObservation {
    /// A minimal observation (tests and synthetic inventory).
    pub fn offered(
        source: InventorySource,
        observed_at: DateTime<Utc>,
        ttl: chrono::Duration,
    ) -> Self {
        Self {
            source,
            provider_id: match source {
                InventorySource::LiveProvider => Some("live-provider".into()),
                _ => None,
            },
            observed_at,
            fresh_until: observed_at + ttl,
            availability: Availability::Available,
            offered_capabilities: Vec::new(),
            observed_number_type: None,
            monthly_cost_minor: None,
            setup_cost_minor: None,
            currency: None,
            evidence_ref: None,
            query_ref: None,
        }
    }

    pub(crate) fn validate_for(&self, resource_source: InventorySource) -> DomainResult<()> {
        if (self.source == InventorySource::SyntheticTest)
            != (resource_source == InventorySource::SyntheticTest)
        {
            return Err(DomainError::Validation(
                "synthetic observations describe synthetic inventory only".into(),
            ));
        }
        if self.fresh_until <= self.observed_at {
            return Err(DomainError::Validation(
                "an observation expires after it was made".into(),
            ));
        }
        match (self.source, self.provider_id.as_deref()) {
            (InventorySource::LiveProvider, None) => {
                return Err(DomainError::Validation(
                    "a live-provider observation names its provider".into(),
                ))
            }
            (_, Some(p))
                if !valid_provider(p)
                    || p == "synthetic" && self.source != InventorySource::SyntheticTest =>
            {
                return Err(DomainError::Validation(
                    "invalid provider identifier".into(),
                ))
            }
            _ => {}
        }
        if (self.monthly_cost_minor.is_some() || self.setup_cost_minor.is_some())
            && !self
                .currency
                .as_deref()
                .is_some_and(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()))
        {
            return Err(DomainError::Validation(
                "a cost needs its ISO 4217 currency".into(),
            ));
        }
        if self.monthly_cost_minor.is_some_and(|c| c < 0)
            || self.setup_cost_minor.is_some_and(|c| c < 0)
        {
            return Err(DomainError::Validation("costs are not negative".into()));
        }
        Ok(())
    }

    /// A quoted price is an estimate until billing evidence exists.
    pub(crate) fn cost_evidence(&self) -> CostEvidence {
        if self.monthly_cost_minor.is_some() || self.setup_cost_minor.is_some() {
            CostEvidence::Estimated
        } else {
            CostEvidence::Unknown
        }
    }
}

pub(crate) fn valid_provider(p: &str) -> bool {
    (2..=64).contains(&p.len())
        && p.as_bytes()[0].is_ascii_lowercase()
        && p.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub(crate) async fn insert_snapshot<C: ConnectionTrait>(
    db: &C,
    resource_id: Uuid,
    o: &NewObservation,
    now: DateTime<Utc>,
) -> DomainResult<Uuid> {
    let id = Uuid::new_v4();
    let caps: Vec<String> = o
        .offered_capabilities
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    db.execute(stmt(
        "INSERT INTO number_inventory_snapshots (id, number_resource_id, source, provider_id, observed_at, \
           fresh_until, availability, offered_capabilities, observed_number_type, monthly_cost_minor, \
           setup_cost_minor, currency, evidence_ref, query_ref, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        vec![
            id.into(),
            resource_id.into(),
            o.source.as_str().into(),
            o.provider_id.clone().into(),
            o.observed_at.into(),
            o.fresh_until.into(),
            o.availability.as_str().into(),
            caps.into(),
            o.observed_number_type
                .filter(|t| *t != NumberType::Unclassified)
                .map(|t| t.as_str().to_string())
                .into(),
            o.monthly_cost_minor.into(),
            o.setup_cost_minor.into(),
            o.currency.clone().into(),
            o.evidence_ref.into(),
            o.query_ref.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(id)
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub source: String,
    pub observed_at: DateTime<Utc>,
    pub fresh_until: DateTime<Utc>,
    pub availability: String,
    pub offered_capabilities: Vec<String>,
}

impl SnapshotRow {
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.fresh_until > now
    }
}

impl NumberDomain {
    /// Appends an observation; the resource's freshness follows the NEWEST observation
    /// (an older, late-arriving report never makes a resource look fresher).
    pub async fn observe(
        &self,
        _ctx: &RequestContext,
        resource_id: Uuid,
        observation: NewObservation,
    ) -> DomainResult<ResourceRow> {
        let now = self.clock.now();
        let txn: DatabaseTransaction = self.db.begin().await?;
        let row = lock(&txn, resource_id).await?;
        observation.validate_for(row.inventory())?;
        insert_snapshot(&txn, resource_id, &observation, now).await?;
        let newer = row
            .last_observed_at
            .is_none_or(|t| observation.observed_at > t);
        let row = if newer {
            ResourceRow::find_by_statement(stmt(
                "UPDATE number_resources SET last_observed_at = $2, observation_fresh_until = $3, \
                   revision = revision + 1, updated_at = $4 WHERE id = $1 RETURNING *",
                vec![
                    resource_id.into(),
                    observation.observed_at.into(),
                    observation.fresh_until.into(),
                    now.into(),
                ],
            ))
            .one(&txn)
            .await?
            .ok_or(DomainError::NotFound("number resource"))?
        } else {
            row
        };
        txn.commit().await?;
        Ok(row)
    }

    /// The newest observation of a resource.
    pub async fn latest_observation(&self, resource_id: Uuid) -> DomainResult<Option<SnapshotRow>> {
        Ok(SnapshotRow::find_by_statement(stmt(
            "SELECT id, number_resource_id, source, observed_at, fresh_until, availability, offered_capabilities \
               FROM number_inventory_snapshots WHERE number_resource_id = $1 \
              ORDER BY observed_at DESC, sequence DESC LIMIT 1",
            vec![resource_id.into()],
        ))
        .one(&self.db)
        .await?)
    }
}

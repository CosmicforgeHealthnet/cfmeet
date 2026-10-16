//! Canonical Number Resources (Part IV §D1): identity, provider / inventory facts,
//! capabilities, lifecycle, compliance / risk references, routing references and
//! commercial readiness. A resource is created DISCOVERED from an inventory observation;
//! its id is the canonical CF Meet identity of the number and never changes.

use chrono::{DateTime, Utc};
use sea_orm::{DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    e164, inventory::NewObservation, lifecycle::TransitionRequest, stmt, Actor,
    ClassificationSource, DomainError, DomainResult, InventorySource, LifecycleState, NumberDomain,
    NumberType, RestrictionFlag, RiskTier,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

/// A persisted Number Resource (every column of `number_resources`).
#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRow {
    pub id: Uuid,
    pub e164: String,
    pub country_code: String,
    pub calling_code: String,
    pub region: Option<String>,
    pub number_type: String,
    pub classification_source: String,
    pub inventory_source: String,
    pub state: String,
    pub revision: i64,
    pub state_changed_at: DateTime<Utc>,
    pub discovered_at: DateTime<Utc>,
    pub available_at: Option<DateTime<Utc>>,
    pub reserved_at: Option<DateTime<Utc>>,
    pub provisioned_at: Option<DateTime<Utc>>,
    pub activated_at: Option<DateTime<Utc>>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub quarantine_until: Option<DateTime<Utc>>,
    pub suspension_reason: Option<String>,
    pub failure_category: Option<String>,
    pub uncertain_operation: Option<String>,
    pub uncertain_from_state: Option<String>,
    pub uncertain_since: Option<DateTime<Utc>>,
    pub last_observed_at: Option<DateTime<Utc>>,
    pub observation_fresh_until: Option<DateTime<Utc>>,
    pub binding_revision: i64,
    pub kyc_requirement_ref: Option<Uuid>,
    pub kyc_profile_ref: Option<Uuid>,
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub risk_tier: String,
    pub restriction_flags: Vec<String>,
    pub voice_route_ref: Option<Uuid>,
    pub sms_route_ref: Option<Uuid>,
    pub routing_revision: i64,
    pub capability_revision: i64,
    pub monthly_cost_minor: Option<i64>,
    pub setup_cost_minor: Option<i64>,
    pub currency: Option<String>,
    pub cost_evidence: String,
    pub subscription_policy_ref: Option<Uuid>,
    pub usage_policy_ref: Option<Uuid>,
    pub renewal_at: Option<DateTime<Utc>>,
    pub renewal_period_days: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ResourceRow {
    pub fn lifecycle_state(&self) -> LifecycleState {
        LifecycleState::parse(&self.state).expect("CHECK-constrained state")
    }

    pub fn inventory(&self) -> InventorySource {
        InventorySource::parse(&self.inventory_source).expect("CHECK-constrained source")
    }

    /// Whether the latest observation is still fresh at `now`.
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.observation_fresh_until
            .is_some_and(|until| until > now)
    }
}

/// Compliance / risk / commercial facts known at discovery (all optional).
#[derive(Debug, Clone, Default)]
pub struct ResourceFacts {
    pub region: Option<String>,
    pub kyc_requirement_ref: Option<Uuid>,
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub risk_tier: Option<RiskTier>,
    pub restriction_flags: Vec<RestrictionFlag>,
    pub subscription_policy_ref: Option<Uuid>,
    pub usage_policy_ref: Option<Uuid>,
    pub renewal_period_days: Option<i32>,
}

/// A number to discover.
#[derive(Debug, Clone)]
pub struct NewResource {
    /// Raw number (normalized to E.164 here).
    pub number: String,
    /// ISO 3166-1 alpha-2, stated by the source (never inferred).
    pub country: String,
    pub inventory_source: InventorySource,
    /// Classification with its evidence source, when the source states one.
    pub classification: Option<(NumberType, ClassificationSource)>,
    pub facts: ResourceFacts,
}

/// Locks a resource row (`FOR UPDATE`).
pub(crate) async fn lock(txn: &DatabaseTransaction, id: Uuid) -> DomainResult<ResourceRow> {
    ResourceRow::find_by_statement(stmt(
        "SELECT * FROM number_resources WHERE id = $1 FOR UPDATE",
        vec![id.into()],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::NotFound("number resource"))
}

fn validate_classification(
    source: InventorySource,
    classification: Option<(NumberType, ClassificationSource)>,
) -> DomainResult<(NumberType, ClassificationSource)> {
    let (number_type, class_source) =
        classification.unwrap_or((NumberType::Unclassified, ClassificationSource::None));
    if (number_type == NumberType::Unclassified) != (class_source == ClassificationSource::None) {
        return Err(DomainError::Validation(
            "a classification needs its evidence source (and only a classification has one)".into(),
        ));
    }
    let synthetic = source == InventorySource::SyntheticTest;
    if synthetic != (class_source == ClassificationSource::Synthetic)
        && class_source != ClassificationSource::None
    {
        return Err(DomainError::Validation(
            "synthetic inventory is classified synthetically, real inventory from real evidence"
                .into(),
        ));
    }
    Ok((number_type, class_source))
}

impl NumberDomain {
    /// Creates a DISCOVERED resource from its first inventory observation (one
    /// transaction: resource, observation, lifecycle audit, outbox event).
    pub async fn discover(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        new: NewResource,
        observation: NewObservation,
    ) -> DomainResult<ResourceRow> {
        let number = e164::validate(&new.number, &new.country)?;
        let (number_type, class_source) =
            validate_classification(new.inventory_source, new.classification)?;
        observation.validate_for(new.inventory_source)?;
        if let Some(region) = &new.facts.region {
            if region.trim().is_empty() || region.len() > 100 {
                return Err(DomainError::Validation(
                    "the region is 1–100 characters".into(),
                ));
            }
        }
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let id = Uuid::new_v4();
        let flags: Vec<String> = new
            .facts
            .restriction_flags
            .iter()
            .map(|f| f.as_str().to_string())
            .collect();
        let row = ResourceRow::find_by_statement(stmt(
            "INSERT INTO number_resources (id, e164, country_code, calling_code, region, number_type, \
               classification_source, inventory_source, state, revision, state_changed_at, discovered_at, \
               last_observed_at, observation_fresh_until, kyc_requirement_ref, jurisdiction_policy_ref, risk_tier, \
               restriction_flags, subscription_policy_ref, usage_policy_ref, renewal_period_days, \
               monthly_cost_minor, setup_cost_minor, currency, cost_evidence, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'DISCOVERED', 1, $9, $9, $10, $11, $12, $13, $14, $15, $16, \
               $17, $18, $19, $20, $21, $22, $9, $9) RETURNING *",
            vec![
                id.into(),
                number.e164.clone().into(),
                number.country_code.clone().into(),
                number.calling_code.clone().into(),
                new.facts.region.clone().into(),
                number_type.as_str().into(),
                class_source.as_str().into(),
                new.inventory_source.as_str().into(),
                now.into(),
                observation.observed_at.into(),
                observation.fresh_until.into(),
                new.facts.kyc_requirement_ref.into(),
                new.facts.jurisdiction_policy_ref.into(),
                new.facts.risk_tier.unwrap_or(RiskTier::Unassessed).as_str().into(),
                flags.into(),
                new.facts.subscription_policy_ref.into(),
                new.facts.usage_policy_ref.into(),
                new.facts.renewal_period_days.into(),
                observation.monthly_cost_minor.into(),
                observation.setup_cost_minor.into(),
                observation.currency.clone().into(),
                observation.cost_evidence().as_str().into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or_else(|| DomainError::Database {
            summary: "insert returned no row".into(),
            connectivity: false,
        })?;
        super::inventory::insert_snapshot(&txn, id, &observation, now).await?;
        let creation =
            TransitionRequest::new(LifecycleState::Discovered, 1, actor.clone(), "DISCOVERED");
        super::lifecycle::record_event(&txn, ctx, &row, None, &creation, now).await?;
        outbox::enqueue(
            &txn,
            NewEvent {
                event_type: "number.resource.discovered",
                aggregate_type: "NUMBER_RESOURCE",
                aggregate_id: id,
                scope: None,
                payload: json!({
                    "number_resource_id": id,
                    "country_code": row.country_code,
                    "inventory_source": row.inventory_source,
                    "number_type": row.number_type,
                    "state": row.state,
                    "revision": row.revision,
                    "actor_kind": actor.kind.as_str(),
                }),
            },
            ctx,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// A resource by its canonical id.
    pub async fn resource(&self, id: Uuid) -> DomainResult<ResourceRow> {
        ResourceRow::find_by_statement(stmt(
            "SELECT * FROM number_resources WHERE id = $1",
            vec![id.into()],
        ))
        .one(&self.db)
        .await?
        .ok_or(DomainError::NotFound("number resource"))
    }

    /// Every resource of a canonical number (a real and a synthetic test resource may
    /// share digits); real inventory first.
    pub async fn resources_by_e164(&self, e164: &str) -> DomainResult<Vec<ResourceRow>> {
        Ok(ResourceRow::find_by_statement(stmt(
            "SELECT * FROM number_resources WHERE e164 = $1 ORDER BY (inventory_source = 'SYNTHETIC_TEST'), id",
            vec![e164.into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// A resource by number within an inventory class (real or synthetic).
    pub async fn resource_by_number(
        &self,
        number: &str,
        synthetic: bool,
    ) -> DomainResult<ResourceRow> {
        let e164 = e164::normalize(number)?;
        ResourceRow::find_by_statement(stmt(
            "SELECT * FROM number_resources WHERE e164 = $1 AND (inventory_source = 'SYNTHETIC_TEST') = $2",
            vec![e164.into(), synthetic.into()],
        ))
        .one(&self.db)
        .await?
        .ok_or(DomainError::NotFound("number resource"))
    }

    /// One lifecycle transition in its own transaction (see [`super::lifecycle`]).
    pub async fn transition(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        request: TransitionRequest,
    ) -> DomainResult<ResourceRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let row = super::lifecycle::transition_in(&txn, ctx, resource_id, request, now).await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Records an evidence-backed classification (never inferred).
    pub async fn classify(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        expected_revision: i64,
        number_type: NumberType,
        source: ClassificationSource,
    ) -> DomainResult<ResourceRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let row = lock(&txn, resource_id).await?;
        if row.revision != expected_revision {
            return Err(DomainError::VersionConflict("number resource"));
        }
        validate_classification(row.inventory(), Some((number_type, source)))?;
        let updated = ResourceRow::find_by_statement(stmt(
            "UPDATE number_resources SET number_type = $2, classification_source = $3, revision = revision + 1, \
               updated_at = $4 WHERE id = $1 RETURNING *",
            vec![resource_id.into(), number_type.as_str().into(), source.as_str().into(), now.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("number resource"))?;
        outbox::enqueue(
            &txn,
            NewEvent {
                event_type: "number.resource.classified",
                aggregate_type: "NUMBER_RESOURCE",
                aggregate_id: resource_id,
                scope: None,
                payload: json!({
                    "number_resource_id": resource_id,
                    "number_type": number_type.as_str(),
                    "classification_source": source.as_str(),
                    "revision": updated.revision,
                }),
            },
            ctx,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }

    /// The lifecycle audit of a resource, oldest first.
    pub async fn lifecycle_events(
        &self,
        resource_id: Uuid,
    ) -> DomainResult<Vec<LifecycleEventRow>> {
        Ok(LifecycleEventRow::find_by_statement(stmt(
            "SELECT number_resource_id, from_state, to_state, revision, actor_kind, actor_ref, reason_code, \
               failure_category, evidence_ref, correlation_id, occurred_at \
             FROM number_lifecycle_events WHERE number_resource_id = $1 ORDER BY sequence",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleEventRow {
    pub number_resource_id: Uuid,
    pub from_state: Option<String>,
    pub to_state: String,
    pub revision: i64,
    pub actor_kind: String,
    pub actor_ref: String,
    pub reason_code: String,
    pub failure_category: Option<String>,
    pub evidence_ref: Option<Uuid>,
    pub correlation_id: String,
    pub occurred_at: DateTime<Utc>,
}

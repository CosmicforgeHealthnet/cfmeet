//! Capabilities (Part IV §D4): per number and capability, the current status with its
//! source, evidence, verification and effective dates and limitations; every change
//! closes the current record and appends a new one (append-oriented history).
//!
//! * VERIFIED requires evidence (`evidence_ref`, `verified_at`) from a real source;
//!   synthetic inventory is never VERIFIED.
//! * Nothing is inferred: SMS support does not make a number mobile, and neither does its
//!   E.164 format — MOBILE_NUMBER_CLASS is recorded only when a source states it.
//! * Downgrade-safe: restricting or withdrawing a capability that an open assignment
//!   relies on is refused unless the caller acknowledges the impact; the impacted
//!   assignments are returned (and evented) so routing can be adjusted.

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    resources::lock, stmt, Capability, CapabilitySource, CapabilityStatus, DomainError,
    DomainResult, InventorySource, NumberDomain,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub capability: String,
    pub status: String,
    pub source: String,
    pub evidence_ref: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub limitations: serde_json::Value,
    pub superseded_by: Option<Uuid>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewCapability {
    pub capability: Capability,
    pub status: CapabilityStatus,
    pub source: CapabilitySource,
    pub evidence_ref: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    /// Defaults to now.
    pub effective_from: Option<DateTime<Utc>>,
    /// Bounded, structured limitations (e.g. `{"smsCountries": ["NG"]}`).
    pub limitations: serde_json::Value,
    /// Required to downgrade a capability an open assignment relies on.
    pub acknowledge_impact: bool,
}

impl NewCapability {
    pub fn declared(capability: Capability, source: CapabilitySource) -> Self {
        Self {
            capability,
            status: CapabilityStatus::Declared,
            source,
            evidence_ref: None,
            verified_at: None,
            effective_from: None,
            limitations: json!({}),
            acknowledge_impact: false,
        }
    }
}

/// Result of recording a capability.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityChange {
    pub capability: CapabilityRow,
    pub previous_status: Option<String>,
    /// Open assignments that relied on a now-unusable capability.
    pub impacted_assignments: Vec<Uuid>,
}

fn validate(new: &NewCapability, source: InventorySource) -> DomainResult<()> {
    let synthetic_source = new.source == CapabilitySource::Synthetic;
    if synthetic_source != (source == InventorySource::SyntheticTest) {
        return Err(DomainError::Validation(
            "synthetic capabilities describe synthetic inventory only".into(),
        ));
    }
    if new.status == CapabilityStatus::Verified
        && (new.evidence_ref.is_none() || new.verified_at.is_none() || synthetic_source)
    {
        return Err(DomainError::precondition(
            "EVIDENCE_REQUIRED",
            "only evidence-backed facts are VERIFIED (never synthetic ones)",
        ));
    }
    if !new.limitations.is_object() || new.limitations.to_string().len() > 2_000 {
        return Err(DomainError::Validation(
            "limitations are a small JSON object".into(),
        ));
    }
    Ok(())
}

impl NumberDomain {
    /// Records the new current status of one capability.
    pub async fn record_capability(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        new: NewCapability,
    ) -> DomainResult<CapabilityChange> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let row = lock(&txn, resource_id).await?;
        validate(&new, row.inventory())?;
        let current = CapabilityRow::find_by_statement(stmt(
            "SELECT * FROM number_capabilities WHERE number_resource_id = $1 AND capability = $2 \
               AND effective_to IS NULL",
            vec![resource_id.into(), new.capability.as_str().into()],
        ))
        .one(&txn)
        .await?;
        // Downgrade safety.
        let mut impacted = Vec::new();
        if !new.status.is_usable() {
            #[derive(FromQueryResult)]
            struct Id {
                id: Uuid,
            }
            impacted = Id::find_by_statement(stmt(
                "SELECT id FROM dcn_assignments WHERE number_resource_id = $1 AND status <> 'CLOSED' \
                   AND $2 = ANY (capability_snapshot) ORDER BY requested_at",
                vec![resource_id.into(), new.capability.as_str().into()],
            ))
            .all(&txn)
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect();
            if !impacted.is_empty() && !new.acknowledge_impact {
                return Err(DomainError::precondition(
                    "CAPABILITY_IN_USE",
                    "an open assignment relies on this capability; acknowledge the impact to downgrade it",
                ));
            }
        }
        let id = Uuid::new_v4();
        if let Some(current) = &current {
            sea_orm::ConnectionTrait::execute(
                &txn,
                stmt(
                    "UPDATE number_capabilities SET effective_to = $2, superseded_by = $3 WHERE id = $1",
                    vec![current.id.into(), now.into(), id.into()],
                ),
            )
            .await?;
        }
        let recorded = CapabilityRow::find_by_statement(stmt(
            "INSERT INTO number_capabilities (id, number_resource_id, capability, status, source, evidence_ref, \
               verified_at, effective_from, limitations, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *",
            vec![
                id.into(),
                resource_id.into(),
                new.capability.as_str().into(),
                new.status.as_str().into(),
                new.source.as_str().into(),
                new.evidence_ref.into(),
                new.verified_at.into(),
                new.effective_from.unwrap_or(now).into(),
                new.limitations.clone().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        outbox::enqueue(
            &txn,
            NewEvent {
                event_type: "number.capability.changed",
                aggregate_type: "NUMBER_RESOURCE",
                aggregate_id: resource_id,
                scope: None,
                payload: json!({
                    "number_resource_id": resource_id,
                    "capability": new.capability.as_str(),
                    "status": new.status.as_str(),
                    "previous_status": current.as_ref().map(|c| c.status.clone()),
                    "source": new.source.as_str(),
                    "impacted_assignment_ids": impacted,
                }),
            },
            ctx,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(CapabilityChange {
            capability: recorded,
            previous_status: current.map(|c| c.status),
            impacted_assignments: impacted,
        })
    }

    /// The current capability records of a resource.
    pub async fn current_capabilities(
        &self,
        resource_id: Uuid,
    ) -> DomainResult<Vec<CapabilityRow>> {
        Ok(CapabilityRow::find_by_statement(stmt(
            "SELECT * FROM number_capabilities WHERE number_resource_id = $1 AND effective_to IS NULL \
             ORDER BY capability",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// The full capability history of a resource, oldest first.
    pub async fn capability_history(&self, resource_id: Uuid) -> DomainResult<Vec<CapabilityRow>> {
        Ok(CapabilityRow::find_by_statement(stmt(
            "SELECT * FROM number_capabilities WHERE number_resource_id = $1 ORDER BY recorded_at, capability",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }
}

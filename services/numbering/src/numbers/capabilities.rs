//! Capabilities (Part IV §D4, Workstream E): per number and capability, the current
//! status with its source, evidence, verification, effective dates, expiry and
//! limitations. Every change closes the current record and appends a new one
//! (append-oriented history) and advances the resource's `capability_revision`, which
//! consumers (grants, routing) compare to reject stale decisions.
//!
//! * VERIFIED requires evidence (`evidence_ref`, `verified_at`) from a real source;
//!   synthetic inventory is never VERIFIED. A PROVIDER_CONFIRMATION names the ACTIVE
//!   binding that confirmed it (and stops counting once that binding ends).
//! * Nothing is inferred: SMS support does not make a number mobile, inbound does not
//!   imply outbound — every capability has its own record.
//! * Stale updates are refused: a source observation older than the current record's
//!   (`STALE_EVIDENCE`), or a stale `expected_capability_revision` (VERSION_CONFLICT).
//! * Source authority: a lower-authority source never overrides the current truth set by
//!   a higher-authority one (`SOURCE_AUTHORITY_CONFLICT`); downgrades are accepted from
//!   any source (fail safe). A REVOKED capability is re-enabled only by an operator or
//!   reconciliation.
//! * Downgrade-safe: restricting or withdrawing a capability an open assignment relies on
//!   is refused unless the impact is acknowledged; revocation and expiry are authoritative
//!   and report the impacted assignments. Effective truth changes immediately.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    limitations,
    resources::{lock, ResourceRow},
    stmt,
    types::reason_code,
    Actor, ActorKind, Capability, CapabilityChangeKind, CapabilitySource, CapabilityStatus,
    DomainError, DomainResult, InventorySource, NumberDomain,
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
    pub expires_at: Option<DateTime<Utc>>,
    pub source_observed_at: Option<DateTime<Utc>>,
    /// Internal: the binding that confirmed it (exposed as an opaque binding ref only).
    pub binding_id: Option<Uuid>,
    pub capability_revision: Option<i64>,
    pub change_kind: Option<String>,
    pub actor_kind: Option<String>,
    pub actor_ref: Option<String>,
    pub reason_code: Option<String>,
}

impl CapabilityRow {
    pub fn status(&self) -> CapabilityStatus {
        CapabilityStatus::parse(&self.status).expect("CHECK-constrained status")
    }

    pub fn source(&self) -> CapabilitySource {
        CapabilitySource::parse(&self.source).expect("CHECK-constrained source")
    }

    /// When the source observed the fact (records before Workstream E: when recorded).
    pub fn observed_at(&self) -> DateTime<Utc> {
        self.source_observed_at.unwrap_or(self.recorded_at)
    }

    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }

    /// Whether the record grants the capability at `now` (status, effective date, expiry).
    pub fn grants_at(&self, now: DateTime<Utc>) -> bool {
        self.status().is_usable() && self.effective_from <= now && !self.is_expired_at(now)
    }
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
    /// When the evidence stops counting (`None`: until superseded).
    pub expires_at: Option<DateTime<Utc>>,
    /// When the SOURCE observed the fact (defaults to now): older than the current
    /// record's observation → `STALE_EVIDENCE`.
    pub source_observed_at: Option<DateTime<Utc>>,
    /// The ACTIVE binding that confirmed it (required for PROVIDER_CONFIRMATION).
    pub binding_id: Option<Uuid>,
    /// Optimistic concurrency on the resource's capability revision.
    pub expected_capability_revision: Option<i64>,
    /// Bounded, structured limitations ([`limitations::validate`]).
    pub limitations: serde_json::Value,
    /// Required to downgrade a capability an open assignment relies on.
    pub acknowledge_impact: bool,
    /// Who records it (defaults to a system recorder).
    pub actor: Option<Actor>,
    /// Defaults to CAPABILITY_RECORDED.
    pub reason_code: Option<String>,
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
            expires_at: None,
            source_observed_at: None,
            binding_id: None,
            expected_capability_revision: None,
            limitations: json!({}),
            acknowledge_impact: false,
            actor: None,
            reason_code: None,
        }
    }

    /// An evidence-backed VERIFIED fact.
    pub fn verified(
        capability: Capability,
        source: CapabilitySource,
        evidence_ref: Uuid,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            status: CapabilityStatus::Verified,
            evidence_ref: Some(evidence_ref),
            verified_at: Some(at),
            ..Self::declared(capability, source)
        }
    }
}

/// Result of a capability change.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityChange {
    pub capability: CapabilityRow,
    pub previous_status: Option<String>,
    pub change_kind: CapabilityChangeKind,
    /// The resource's capability revision after the change.
    pub capability_revision: i64,
    /// Open assignments that relied on a now-unusable capability.
    pub impacted_assignments: Vec<Uuid>,
}

/// Source authority (higher wins; synthetic data only ever describes synthetic numbers).
pub fn authority(source: CapabilitySource) -> u8 {
    match source {
        CapabilitySource::ProviderConfirmation => 3,
        CapabilitySource::ProviderInventory => 2,
        CapabilitySource::ManualImport => 1,
        CapabilitySource::Synthetic => 0,
    }
}

/// Assurance of a status at `now` (VERIFIED > DECLARED > anything unusable).
fn assurance(row: Option<&CapabilityRow>, now: DateTime<Utc>) -> u8 {
    match row {
        Some(r) if r.grants_at(now) => match r.status() {
            CapabilityStatus::Verified => 2,
            _ => 1,
        },
        _ => 0,
    }
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
    if matches!(
        new.status,
        CapabilityStatus::Expired | CapabilityStatus::Revoked
    ) {
        return Err(DomainError::Validation(
            "expiry and revocation are recorded by their own operations".into(),
        ));
    }
    limitations::validate(&new.limitations)?;
    Ok(())
}

async fn impacted_assignments(
    txn: &DatabaseTransaction,
    resource_id: Uuid,
    capability: Capability,
) -> DomainResult<Vec<Uuid>> {
    #[derive(FromQueryResult)]
    struct Id {
        id: Uuid,
    }
    Ok(Id::find_by_statement(stmt(
        "SELECT id FROM dcn_assignments WHERE number_resource_id = $1 AND status <> 'CLOSED' \
           AND $2 = ANY (capability_snapshot) ORDER BY requested_at",
        vec![resource_id.into(), capability.as_str().into()],
    ))
    .all(txn)
    .await?
    .into_iter()
    .map(|r| r.id)
    .collect())
}

async fn current_record(
    txn: &DatabaseTransaction,
    resource_id: Uuid,
    capability: Capability,
) -> DomainResult<Option<CapabilityRow>> {
    Ok(CapabilityRow::find_by_statement(stmt(
        "SELECT * FROM number_capabilities WHERE number_resource_id = $1 AND capability = $2 AND effective_to IS NULL",
        vec![resource_id.into(), capability.as_str().into()],
    ))
    .one(txn)
    .await?)
}

/// A record to append (all provenance already validated).
struct Append<'a> {
    capability: Capability,
    status: CapabilityStatus,
    source: CapabilitySource,
    evidence_ref: Option<Uuid>,
    verified_at: Option<DateTime<Utc>>,
    effective_from: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    source_observed_at: DateTime<Utc>,
    binding_id: Option<Uuid>,
    limitations: serde_json::Value,
    change_kind: CapabilityChangeKind,
    actor: &'a Actor,
    reason: &'a str,
}

/// Closes `current`, appends the new record, advances the resource's capability revision
/// and writes the outbox event — inside `txn`.
async fn append(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    resource: &ResourceRow,
    current: Option<&CapabilityRow>,
    a: Append<'_>,
    impacted: Vec<Uuid>,
    now: DateTime<Utc>,
) -> DomainResult<CapabilityChange> {
    let id = Uuid::new_v4();
    if let Some(current) = current {
        txn.execute(stmt(
            "UPDATE number_capabilities SET effective_to = $2, superseded_by = $3 WHERE id = $1",
            vec![
                current.id.into(),
                now.max(current.effective_from).into(),
                id.into(),
            ],
        ))
        .await?;
    }
    let revision = ResourceRow::find_by_statement(stmt(
        "UPDATE number_resources SET capability_revision = capability_revision + 1, revision = revision + 1, \
           updated_at = $2 WHERE id = $1 RETURNING *",
        vec![resource.id.into(), now.into()],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::NotFound("number resource"))?
    .capability_revision;
    let recorded = CapabilityRow::find_by_statement(stmt(
        "INSERT INTO number_capabilities (id, number_resource_id, capability, status, source, evidence_ref, \
           verified_at, effective_from, limitations, recorded_at, expires_at, source_observed_at, binding_id, \
           capability_revision, change_kind, actor_kind, actor_ref, reason_code) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING *",
        vec![
            id.into(),
            resource.id.into(),
            a.capability.as_str().into(),
            a.status.as_str().into(),
            a.source.as_str().into(),
            a.evidence_ref.into(),
            a.verified_at.into(),
            a.effective_from.into(),
            a.limitations.clone().into(),
            now.into(),
            a.expires_at.into(),
            a.source_observed_at.into(),
            a.binding_id.into(),
            revision.into(),
            a.change_kind.as_str().into(),
            a.actor.kind.as_str().into(),
            a.actor.reference.clone().into(),
            a.reason.into(),
        ],
    ))
    .one(txn)
    .await?
    .expect("insert returns the row");
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.capability.changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: resource.id,
            scope: None,
            payload: json!({
                "number_resource_id": resource.id,
                "capability": a.capability.as_str(),
                "status": a.status.as_str(),
                "previous_status": current.map(|c| c.status.clone()),
                "source": a.source.as_str(),
                "change_kind": a.change_kind.as_str(),
                "capability_revision": revision,
                "binding_ref": a.binding_id,
                "expires_at": a.expires_at,
                "reason_code": a.reason,
                "impacted_assignment_ids": impacted,
            }),
        },
        ctx,
        now,
    )
    .await?;
    crate::metrics::inc(
        "numbering_capability_changes_total",
        &[("kind", a.change_kind.as_str())],
    );
    Ok(CapabilityChange {
        capability: recorded,
        previous_status: current.map(|c| c.status.clone()),
        change_kind: a.change_kind,
        capability_revision: revision,
        impacted_assignments: impacted,
    })
}

impl NumberDomain {
    /// Records the new current status of one capability (see the module rules).
    pub async fn record_capability(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        new: NewCapability,
    ) -> DomainResult<CapabilityChange> {
        let now = self.clock.now();
        let actor = new
            .actor
            .clone()
            .unwrap_or_else(|| Actor::system("capability-recorder"));
        let reason = new
            .reason_code
            .clone()
            .unwrap_or_else(|| "CAPABILITY_RECORDED".into());
        reason_code(&reason)?;
        let observed = new.source_observed_at.unwrap_or(now);
        if observed > now + chrono::Duration::minutes(5) {
            return Err(DomainError::Validation(
                "a source cannot observe the future".into(),
            ));
        }
        let effective_from = new.effective_from.unwrap_or(now);
        if new.expires_at.is_some_and(|t| t <= effective_from) {
            return Err(DomainError::Validation(
                "evidence expires after it takes effect".into(),
            ));
        }
        let txn = self.db.begin().await?;
        let resource = lock(&txn, resource_id).await?;
        validate(&new, resource.inventory())?;
        if new
            .expected_capability_revision
            .is_some_and(|r| r != resource.capability_revision)
        {
            return Err(DomainError::VersionConflict("capability"));
        }
        let current = current_record(&txn, resource_id, new.capability).await?;
        if let Some(c) = &current {
            if observed < c.observed_at() {
                return Err(DomainError::precondition(
                    "STALE_EVIDENCE",
                    "the source observation is older than the current capability record",
                ));
            }
        }
        // Binding provenance.
        if let Some(binding_id) = new.binding_id {
            #[derive(FromQueryResult)]
            struct B {
                number_resource_id: Uuid,
                status: String,
                capabilities: Vec<String>,
            }
            let b = B::find_by_statement(stmt(
                "SELECT number_resource_id, status, capabilities FROM number_provider_bindings WHERE id = $1",
                vec![binding_id.into()],
            ))
            .one(&txn)
            .await?;
            let ok = b.is_some_and(|b| {
                b.number_resource_id == resource_id
                    && b.status == "ACTIVE"
                    && (!new.capability.is_routable()
                        || b.capabilities.iter().any(|c| c == new.capability.as_str()))
            });
            if !ok {
                return Err(DomainError::precondition(
                    "BINDING_MISMATCH",
                    "the confirming binding is not an active binding of this number serving the capability",
                ));
            }
        } else if new.source == CapabilitySource::ProviderConfirmation {
            return Err(DomainError::precondition(
                "BINDING_REQUIRED",
                "a provider confirmation names the binding that confirmed it",
            ));
        }
        let new_usable = new.status.is_usable();
        if let Some(c) = &current {
            if new_usable
                && c.status() == CapabilityStatus::Revoked
                && !matches!(actor.kind, ActorKind::Operator | ActorKind::Reconciliation)
            {
                return Err(DomainError::precondition(
                    "REVOCATION_ACTIVE",
                    "a revoked capability is re-enabled only by an operator or reconciliation",
                ));
            }
            let current_still_counts =
                !c.is_expired_at(now) && c.status() != CapabilityStatus::Expired;
            if new_usable && current_still_counts && authority(new.source) < authority(c.source()) {
                return Err(DomainError::precondition(
                    "SOURCE_AUTHORITY_CONFLICT",
                    "a lower-authority source cannot override the current capability truth",
                ));
            }
        }
        let mut impacted = Vec::new();
        if !new_usable {
            impacted = impacted_assignments(&txn, resource_id, new.capability).await?;
            if !impacted.is_empty() && !new.acknowledge_impact {
                return Err(DomainError::precondition(
                    "CAPABILITY_IN_USE",
                    "an open assignment relies on this capability; acknowledge the impact to downgrade it",
                ));
            }
        }
        let before = assurance(current.as_ref(), now);
        let after = match new.status {
            CapabilityStatus::Verified => 2,
            CapabilityStatus::Declared => 1,
            _ => 0,
        };
        let change_kind = match (&current, before.cmp(&after)) {
            (None, _) => CapabilityChangeKind::Initial,
            (_, std::cmp::Ordering::Greater) => CapabilityChangeKind::Downgrade,
            (_, std::cmp::Ordering::Less) => CapabilityChangeKind::Upgrade,
            (_, std::cmp::Ordering::Equal) => CapabilityChangeKind::Refresh,
        };
        let change = append(
            &txn,
            ctx,
            &resource,
            current.as_ref(),
            Append {
                capability: new.capability,
                status: new.status,
                source: new.source,
                evidence_ref: new.evidence_ref,
                verified_at: new.verified_at,
                effective_from,
                expires_at: new.expires_at,
                source_observed_at: observed,
                binding_id: new.binding_id,
                limitations: new.limitations.clone(),
                change_kind,
                actor: &actor,
                reason: &reason,
            },
            impacted,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(change)
    }

    /// Revokes a capability (operator decision): immediately not usable, whatever the
    /// evidence said; impacted open assignments are reported.
    pub async fn revoke_capability(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        resource_id: Uuid,
        capability: Capability,
        reason: &str,
    ) -> DomainResult<CapabilityChange> {
        reason_code(reason)?;
        if actor.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "capabilities are revoked by an operator",
            ));
        }
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let resource = lock(&txn, resource_id).await?;
        let current = current_record(&txn, resource_id, capability)
            .await?
            .ok_or(DomainError::NotFound("capability"))?;
        if current.status() == CapabilityStatus::Revoked {
            return Err(DomainError::precondition(
                "ALREADY_REVOKED",
                "the capability is already revoked",
            ));
        }
        let impacted = impacted_assignments(&txn, resource_id, capability).await?;
        let change = append(
            &txn,
            ctx,
            &resource,
            Some(&current),
            Append {
                capability,
                status: CapabilityStatus::Revoked,
                source: current.source(),
                evidence_ref: None,
                verified_at: None,
                effective_from: now,
                expires_at: None,
                source_observed_at: now,
                binding_id: None,
                limitations: current.limitations.clone(),
                change_kind: CapabilityChangeKind::Revocation,
                actor: &actor,
                reason,
            },
            impacted,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(change)
    }

    /// Records expiry for evidence past its `expires_at` (bounded batch). Resolution
    /// already treats such evidence as not granting; this makes the change durable and
    /// evented. Returns the changes made.
    pub async fn expire_capabilities(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<Vec<CapabilityChange>> {
        let now = self.clock.now();
        #[derive(FromQueryResult)]
        struct Due {
            number_resource_id: Uuid,
            capability: String,
        }
        let due = Due::find_by_statement(stmt(
            "SELECT number_resource_id, capability FROM number_capabilities \
              WHERE effective_to IS NULL AND expires_at <= $1 AND status IN ('VERIFIED', 'DECLARED') \
              ORDER BY expires_at LIMIT $2",
            vec![now.into(), (limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(&self.db)
        .await?;
        let actor = Actor::system("capability-expiry");
        let mut changes = Vec::new();
        for d in due {
            let capability = Capability::parse(&d.capability).expect("CHECK-constrained");
            let txn = self.db.begin().await?;
            let resource = lock(&txn, d.number_resource_id).await?;
            // Re-read under the lock: it may have been refreshed meanwhile.
            let Some(current) = current_record(&txn, d.number_resource_id, capability).await?
            else {
                continue;
            };
            if !current.status().is_usable() || !current.is_expired_at(now) {
                continue;
            }
            let impacted = impacted_assignments(&txn, d.number_resource_id, capability).await?;
            let change = append(
                &txn,
                ctx,
                &resource,
                Some(&current),
                Append {
                    capability,
                    status: CapabilityStatus::Expired,
                    source: current.source(),
                    evidence_ref: None,
                    verified_at: None,
                    effective_from: now,
                    expires_at: None,
                    source_observed_at: now,
                    binding_id: None,
                    limitations: current.limitations.clone(),
                    change_kind: CapabilityChangeKind::Expiry,
                    actor: &actor,
                    reason: "EVIDENCE_EXPIRED",
                },
                impacted,
                now,
            )
            .await?;
            txn.commit().await?;
            changes.push(change);
        }
        Ok(changes)
    }

    /// The current capability records of a resource.
    pub async fn current_capabilities(
        &self,
        resource_id: Uuid,
    ) -> DomainResult<Vec<CapabilityRow>> {
        current_capabilities_in(&self.db, resource_id).await
    }

    /// The full capability history of a resource, oldest first.
    pub async fn capability_history(&self, resource_id: Uuid) -> DomainResult<Vec<CapabilityRow>> {
        Ok(CapabilityRow::find_by_statement(stmt(
            "SELECT * FROM number_capabilities WHERE number_resource_id = $1 \
             ORDER BY recorded_at, capability_revision NULLS FIRST, capability",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }
}

pub(crate) async fn current_capabilities_in<C: ConnectionTrait>(
    db: &C,
    resource_id: Uuid,
) -> DomainResult<Vec<CapabilityRow>> {
    Ok(CapabilityRow::find_by_statement(stmt(
        "SELECT * FROM number_capabilities WHERE number_resource_id = $1 AND effective_to IS NULL ORDER BY capability",
        vec![resource_id.into()],
    ))
    .all(db)
    .await?)
}

//! Part P §§119–120 — the Part III relay-number inventory under Numbering's authority.
//!
//! Privacy Relay keeps what it owns: relay SESSIONS, their ids, their relay-number
//! assignments and its routing of an inbound relay call to its session. What moves is the
//! authority over the NUMBERS:
//!
//! * **import** (idempotent backfill): each Privacy Relay inventory row becomes a Number
//!   Resource of service class `PRIVACY_RELAY` (DISCOVERED → AVAILABLE from an import
//!   observation, an ACTIVE provider binding carrying the opaque binding reference) and a
//!   `number_relay_pool_members` row keyed by the Privacy Relay inventory id. A relay-class
//!   number is never searchable, reservable or assignable as a DCN (database triggers);
//! * **leases**: Privacy Relay acquires a temporary lease (one ACTIVE lease per number;
//!   idempotent per holder key; bounded lifetime) before lending a number to a session,
//!   and releases it when the session's assignment ends. Numbering refuses a lease when the
//!   number left service (quarantined / released by an operator), the pool member is
//!   retired, the `RELAY_LEASES` kill switch is engaged or another lease is active;
//! * **compatibility resolution**: the current lease of a relay number (its Privacy Relay
//!   inventory reference, holder and generation) — what Privacy Relay's existing mapping
//!   must agree with.
//!
//! The E.164 crosses this boundary only on import (Privacy Relay → Numbering, the same
//! public relay number both already hold); no lease view carries it.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

use super::Stewardship;
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
    numbers::{
        bindings::NewBinding,
        e164,
        inventory::NewObservation,
        lifecycle::{transition_in, TransitionRequest},
        resources::{lock, NewResource, ResourceFacts},
        stmt, Actor, ActorKind, BindingChangeReason, Capability, DomainError, DomainResult,
        InventorySource, LifecycleState,
    },
};

/// The provider id of imported (non-synthetic) relay inventory bindings.
pub const RELAY_IMPORT_PROVIDER: &str = "relay-import";

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolMemberRow {
    pub number_resource_id: Uuid,
    pub relay_inventory_ref: Uuid,
    #[serde(skip)]
    pub binding_ref: String,
    pub capabilities: Vec<String>,
    pub status: String,
    pub imported_at: DateTime<Utc>,
    pub imported_by: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub holder_service: String,
    pub holder_ref: Uuid,
    pub generation: i64,
    #[serde(skip)]
    pub idempotency_key: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub end_reason: Option<String>,
    pub correlation_id: String,
}

/// One Privacy Relay inventory row to import.
#[derive(Debug, Clone)]
pub struct RelayImport {
    pub relay_inventory_ref: Uuid,
    pub e164: String,
    pub country: String,
    pub region: Option<String>,
    pub capabilities: Vec<Capability>,
    pub binding_ref: String,
    /// SYNTHETIC_TEST in sandbox / test deployments, MANUAL_IMPORT otherwise.
    pub synthetic: bool,
}

/// A lease request.
#[derive(Debug, Clone)]
pub struct LeaseRequest {
    pub relay_inventory_ref: Uuid,
    /// The holder's own reference (Privacy Relay: the relay-number assignment id).
    pub holder_ref: Uuid,
    /// Privacy Relay's assignment generation of the number.
    pub generation: i64,
    pub expires_at: DateTime<Utc>,
    pub idempotency_key: String,
}

/// The compatibility resolution of a relay number.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayResolution {
    pub number_resource_id: Uuid,
    pub relay_inventory_ref: Uuid,
    pub lifecycle_state: String,
    pub member_status: String,
    pub lease: Option<LeaseRow>,
}

async fn member_by_ref<C: ConnectionTrait>(db: &C, r: Uuid) -> DomainResult<Option<PoolMemberRow>> {
    Ok(PoolMemberRow::find_by_statement(stmt(
        "SELECT * FROM number_relay_pool_members WHERE relay_inventory_ref = $1",
        vec![r.into()],
    ))
    .one(db)
    .await?)
}

async fn lease_event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    lease: &LeaseRow,
    member: &PoolMemberRow,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.relay-lease.changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: lease.number_resource_id,
            scope: None,
            payload: serde_json::json!({
                "number_resource_id": lease.number_resource_id,
                "relay_inventory_ref": member.relay_inventory_ref,
                "lease_id": lease.id,
                "generation": lease.generation,
                "status": lease.status,
                "end_reason": lease.end_reason,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

impl Stewardship {
    /// Imports one relay number (idempotent per Privacy Relay inventory id). Returns the
    /// pool member and whether it was new. The same id with another number is a conflict;
    /// a number already used as a DCN is refused.
    pub async fn import_relay_number(
        &self,
        ctx: &RequestContext,
        importer: &str,
        new: RelayImport,
    ) -> DomainResult<(PoolMemberRow, bool)> {
        let number = e164::validate(&new.e164, &new.country)?;
        if new.capabilities.is_empty()
            || new
                .capabilities
                .iter()
                .any(|c| !matches!(c, Capability::VoiceInbound | Capability::VoiceOutbound))
        {
            return Err(DomainError::Validation(
                "relay numbers carry VOICE_INBOUND / VOICE_OUTBOUND only".into(),
            ));
        }
        let source = if new.synthetic {
            InventorySource::SyntheticTest
        } else {
            InventorySource::ManualImport
        };
        if let Some(existing) = member_by_ref(self.numbers.db(), new.relay_inventory_ref).await? {
            let r = self.numbers.resource(existing.number_resource_id).await?;
            if r.e164 != number.e164 {
                return Err(DomainError::conflict(
                    "RELAY_IMPORT_CONFLICT",
                    "the relay inventory id was imported with another number",
                ));
            }
            return Ok((existing, false));
        }
        let actor = Actor::new(ActorKind::Service, importer);
        let now = self.now();
        // An existing resource for the number: only an unused DISCOVERED / AVAILABLE one
        // (an interrupted earlier import) is adopted.
        let existing = self
            .numbers
            .resources_by_e164(&number.e164)
            .await?
            .into_iter()
            .find(|r| r.inventory() == source);
        let resource = match existing {
            Some(r) => r,
            None => {
                let mut observation = NewObservation::offered(source, now, Duration::days(3650));
                observation.offered_capabilities = new.capabilities.clone();
                self.numbers
                    .discover(
                        ctx,
                        actor.clone(),
                        NewResource {
                            number: number.e164.clone(),
                            country: number.country_code.clone(),
                            inventory_source: source,
                            classification: None,
                            facts: ResourceFacts {
                                region: new.region.clone(),
                                ..Default::default()
                            },
                        },
                        observation,
                    )
                    .await?
            }
        };
        let txn = self.numbers.db().begin().await?;
        let r = lock(&txn, resource.id).await?;
        if r.service_class != "PRIVACY_RELAY" {
            if !matches!(
                r.lifecycle_state(),
                LifecycleState::Discovered | LifecycleState::Available
            ) {
                return Err(DomainError::conflict(
                    "NUMBER_ALREADY_MANAGED",
                    "the number is already managed as a DCN",
                ));
            }
            txn.execute(stmt(
                "UPDATE number_resources SET service_class = 'PRIVACY_RELAY', revision = revision + 1, updated_at = $2 \
                 WHERE id = $1",
                vec![r.id.into(), now.into()],
            ))
            .await?;
        }
        let r = lock(&txn, resource.id).await?;
        if r.lifecycle_state() == LifecycleState::Discovered {
            transition_in(
                &txn,
                ctx,
                r.id,
                TransitionRequest::new(
                    LifecycleState::Available,
                    r.revision,
                    actor.clone(),
                    "RELAY_IMPORTED",
                ),
                now,
            )
            .await?;
        }
        if !self
            .numbers
            .bindings(r.id)
            .await?
            .iter()
            .any(|b| b.status == "ACTIVE")
        {
            self.numbers
                .activate_binding_in(
                    &txn,
                    ctx,
                    r.id,
                    NewBinding {
                        provider_id: if new.synthetic {
                            crate::providers::registry::SYNTHETIC_PROVIDER.into()
                        } else {
                            RELAY_IMPORT_PROVIDER.into()
                        },
                        external_ref: new.binding_ref.clone(),
                        capabilities: new.capabilities.clone(),
                        change_reason: BindingChangeReason::Initial,
                    },
                    now,
                )
                .await?;
        }
        let mut caps: Vec<String> = new
            .capabilities
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        caps.sort();
        caps.dedup();
        let member = PoolMemberRow::find_by_statement(stmt(
            "INSERT INTO number_relay_pool_members (number_resource_id, relay_inventory_ref, binding_ref, capabilities, \
               status, imported_at, imported_by, updated_at) VALUES ($1, $2, $3, $4, 'ACTIVE', $5, $6, $5) RETURNING *",
            vec![
                r.id.into(),
                new.relay_inventory_ref.into(),
                new.binding_ref.clone().into(),
                caps.into(),
                now.into(),
                importer.trim_start_matches("cfmeet.").into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        txn.commit().await?;
        crate::metrics::inc("numbering_relay_imports_total", &[]);
        Ok((member, true))
    }

    /// Grants (or replays) a lease of a relay number.
    pub async fn lease_relay_number(
        &self,
        ctx: &RequestContext,
        holder: &str,
        req: LeaseRequest,
    ) -> DomainResult<(LeaseRow, bool)> {
        if !crate::infra::idempotency::valid_key(&req.idempotency_key) {
            return Err(DomainError::Validation(
                "an idempotency key is 8–128 of [A-Za-z0-9._:-]".into(),
            ));
        }
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        if let Some(existing) = LeaseRow::find_by_statement(stmt(
            "SELECT * FROM number_relay_leases WHERE holder_service = $1 AND idempotency_key = $2",
            vec![holder.into(), req.idempotency_key.clone().into()],
        ))
        .one(&txn)
        .await?
        {
            txn.rollback().await?;
            return Ok((existing, true));
        }
        super::controls::require_open(&txn, super::controls::Control::RelayLeases).await?;
        let member = member_by_ref(&txn, req.relay_inventory_ref)
            .await?
            .ok_or(DomainError::NotFound("relay number"))?;
        let resource = lock(&txn, member.number_resource_id).await?;
        if member.status != "ACTIVE" || resource.lifecycle_state() != LifecycleState::Available {
            return Err(DomainError::precondition(
                "RELAY_NUMBER_NOT_LEASABLE",
                "the relay number is not in service",
            ));
        }
        if req.expires_at <= now || req.expires_at > now + self.settings.relay_lease_max {
            return Err(DomainError::Validation(
                "a lease ends in the future, within the maximum lease lifetime".into(),
            ));
        }
        if req.generation < 1 {
            return Err(DomainError::Validation("a generation is at least 1".into()));
        }
        let lease = LeaseRow::find_by_statement(stmt(
            "INSERT INTO number_relay_leases (id, number_resource_id, holder_service, holder_ref, generation, \
               idempotency_key, status, granted_at, expires_at, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, 'ACTIVE', $7, $8, $9) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                member.number_resource_id.into(),
                holder.into(),
                req.holder_ref.into(),
                req.generation.into(),
                req.idempotency_key.clone().into(),
                now.into(),
                req.expires_at.into(),
                ctx.correlation_id.as_str().into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        lease_event(&txn, ctx, &lease, &member, now).await?;
        txn.commit().await?;
        crate::metrics::inc("numbering_relay_leases_total", &[("change", "GRANTED")]);
        Ok((lease, false))
    }

    /// Ends a lease (idempotent: an ended lease answers its state). Only its holder ends it.
    pub async fn release_relay_lease(
        &self,
        ctx: &RequestContext,
        holder: &str,
        lease_id: Uuid,
        reason: &str,
    ) -> DomainResult<LeaseRow> {
        crate::numbers::types::reason_code(reason)?;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let lease = LeaseRow::find_by_statement(stmt(
            "SELECT * FROM number_relay_leases WHERE id = $1 FOR UPDATE",
            vec![lease_id.into()],
        ))
        .one(&txn)
        .await?
        .filter(|l| l.holder_service == holder)
        .ok_or(DomainError::NotFound("relay lease"))?;
        if lease.status != "ACTIVE" {
            txn.rollback().await?;
            return Ok(lease);
        }
        let ended = self
            .end_lease_in(&txn, ctx, &lease, "RELEASED", reason, now)
            .await?;
        txn.commit().await?;
        Ok(ended)
    }

    async fn end_lease_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        lease: &LeaseRow,
        status: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<LeaseRow> {
        let ended = LeaseRow::find_by_statement(stmt(
            "UPDATE number_relay_leases SET status = $2, ended_at = $3, end_reason = $4 \
             WHERE id = $1 AND status = 'ACTIVE' RETURNING *",
            vec![lease.id.into(), status.into(), now.into(), reason.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::VersionConflict("relay lease"))?;
        let member = PoolMemberRow::find_by_statement(stmt(
            "SELECT * FROM number_relay_pool_members WHERE number_resource_id = $1",
            vec![lease.number_resource_id.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::NotFound("relay number"))?;
        lease_event(txn, ctx, &ended, &member, now).await?;
        crate::metrics::inc("numbering_relay_leases_total", &[("change", "ENDED")]);
        Ok(ended)
    }

    /// Worker pass: expires leases past their end (the holder's own expiry normally ends
    /// them first; this bounds an orphaned lease after a holder crash).
    pub async fn expire_relay_leases(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<usize> {
        let now = self.now();
        let due = LeaseRow::find_by_statement(stmt(
            "SELECT * FROM number_relay_leases WHERE status = 'ACTIVE' AND expires_at <= $1 ORDER BY expires_at LIMIT $2",
            vec![now.into(), (limit.clamp(1, 500) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        for l in due {
            let txn = self.numbers.db().begin().await?;
            if self
                .end_lease_in(&txn, ctx, &l, "EXPIRED", "LEASE_EXPIRED", now)
                .await
                .is_ok()
            {
                n += 1;
            }
            txn.commit().await?;
        }
        Ok(n)
    }

    /// The compatibility resolution of a relay number by its Privacy Relay inventory id.
    pub async fn resolve_relay_number(
        &self,
        relay_inventory_ref: Uuid,
    ) -> DomainResult<RelayResolution> {
        let member = member_by_ref(self.numbers.db(), relay_inventory_ref)
            .await?
            .ok_or(DomainError::NotFound("relay number"))?;
        let resource = self.numbers.resource(member.number_resource_id).await?;
        let lease = LeaseRow::find_by_statement(stmt(
            "SELECT * FROM number_relay_leases WHERE number_resource_id = $1 AND status = 'ACTIVE'",
            vec![member.number_resource_id.into()],
        ))
        .one(self.numbers.db())
        .await?;
        Ok(RelayResolution {
            number_resource_id: member.number_resource_id,
            relay_inventory_ref: member.relay_inventory_ref,
            lifecycle_state: resource.state,
            member_status: member.status,
            lease,
        })
    }

    /// Retires a relay number from the pool (operators): no new lease; the active one
    /// runs to its end.
    pub async fn retire_relay_number(
        &self,
        operator: &Actor,
        relay_inventory_ref: Uuid,
    ) -> DomainResult<PoolMemberRow> {
        if operator.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "relay pool changes are operator decisions",
            ));
        }
        PoolMemberRow::find_by_statement(stmt(
            "UPDATE number_relay_pool_members SET status = 'RETIRED', updated_at = $2 \
             WHERE relay_inventory_ref = $1 RETURNING *",
            vec![relay_inventory_ref.into(), self.now().into()],
        ))
        .one(self.numbers.db())
        .await?
        .ok_or(DomainError::NotFound("relay number"))
    }
}

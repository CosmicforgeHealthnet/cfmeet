//! Authenticated provider callback ingestion (Part IV Workstream G §15).
//!
//! 1. The adapter authenticates the raw request (signature over the raw body, timestamp
//!    window) and normalizes it; a refused callback is never stored or applied, and raises
//!    a de-duplicated `CALLBACK_AUTHENTICATION_FAILED` incident.
//! 2. The stable provider event id is the de-duplication key: a redelivered event is
//!    acknowledged as a duplicate and nothing happens twice.
//! 3. The event is correlated to its Number Resource (inventory ref / binding) and, when
//!    the provider echoes it, the command; an event older than the newest version already
//!    applied for the object is STALE (out-of-order safety); an event for a released /
//!    quarantined number never resurrects it.
//! 4. Command-related events never finalize a command by themselves: an uncertain command
//!    is re-checked by read-back reconciliation (scheduled immediately); a completed one is
//!    confirmed. Capability changes update E's capability evidence (downgrades take effect
//!    at once). Unsolicited releases / revocations are drift: fail closed, incident.
//! 5. Every stored event is append-only with its disposition and an outbox event.

use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    commands::{CommandRow, CommandStatus, ProviderCommands, RECONCILE_JOB},
    incidents, CallbackRejection, CallbackRequest, NormalizedProviderEvent, ProviderEventKind as K,
};
use crate::{
    domain::RequestContext,
    infra::{
        jobs::{self, NewJob, RetrySafety},
        outbox::{self, NewEvent},
    },
    numbers::{
        capabilities::NewCapability, resources::ResourceRow, stmt, Capability, CapabilitySource,
        CapabilityStatus, DomainError, DomainResult, InventorySource, LifecycleState as S,
    },
};

/// Every disposition a stored event can have (equal to `ck_number_provider_events_disposition`).
pub const DISPOSITIONS: &[&str] = &[
    "APPLIED",
    "RECONCILE_REQUESTED",
    "CONFIRMED_EXISTING",
    "STALE",
    "UNMATCHED",
    "CONFLICT",
    "IGNORED_TERMINAL",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ingested {
    /// `None` for a duplicate delivery.
    pub event_id: Option<Uuid>,
    pub disposition: &'static str,
    pub duplicate: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unknown provider")]
    UnknownProvider,
    #[error(transparent)]
    Rejected(#[from] CallbackRejection),
    #[error(transparent)]
    Domain(#[from] DomainError),
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEventRow {
    pub id: Uuid,
    #[serde(skip)]
    pub provider_id: String,
    pub provider_event_id: String,
    pub event_kind: String,
    #[serde(skip)]
    pub external_ref: Option<String>,
    pub number_resource_id: Option<Uuid>,
    pub command_id: Option<Uuid>,
    pub provider_sequence: Option<i64>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub payload_sha256: String,
    pub normalized: serde_json::Value,
    pub disposition: String,
    pub correlation_id: String,
}

async fn resource_of<C: ConnectionTrait>(
    db: &C,
    provider_id: &str,
    external_ref: &str,
) -> DomainResult<Option<ResourceRow>> {
    Ok(ResourceRow::find_by_statement(stmt(
        "SELECT r.* FROM number_resources r WHERE r.id IN ( \
           SELECT number_resource_id FROM number_provider_inventory_refs WHERE provider_id = $1 AND external_ref = $2 \
           UNION SELECT number_resource_id FROM number_provider_bindings WHERE provider_id = $1 AND external_ref = $2) \
         LIMIT 1",
        vec![provider_id.into(), external_ref.into()],
    ))
    .one(db)
    .await?)
}

impl ProviderCommands {
    /// Ingests one callback for `provider_id` (see module rules).
    pub async fn ingest_callback(
        &self,
        ctx: &RequestContext,
        provider_id: &str,
        request: &CallbackRequest,
    ) -> Result<Ingested, IngestError> {
        let provider = self
            .registry
            .get(provider_id)
            .ok_or(IngestError::UnknownProvider)?;
        let now = self.now();
        let ev = match provider.verify_event(request, now) {
            Ok(ev) => ev,
            Err(rejection) => {
                crate::metrics::inc("numbering_provider_callbacks_rejected_total", &[]);
                incidents::raise(
                    self.numbers.db(),
                    incidents::Category::CallbackAuthenticationFailed,
                    &format!("callback-auth:{provider_id}"),
                    incidents::Subject {
                        provider_id: Some(provider_id.into()),
                        ..Default::default()
                    },
                    json!({ "rejection": format!("{rejection:?}") }),
                    now,
                )
                .await
                .map_err(DomainError::from)?;
                return Err(rejection.into());
            }
        };
        let digest = cf_object_storage::sha256_hex(&request.body);
        let txn = self.numbers.db().begin().await.map_err(DomainError::from)?;
        // Replay / redelivery: the stable event id was already received.
        let dup = txn
            .query_one(stmt(
                "SELECT id FROM number_provider_events WHERE provider_id = $1 AND provider_event_id = $2",
                vec![provider_id.into(), ev.provider_event_id.clone().into()],
            ))
            .await
            .map_err(DomainError::from)?;
        if dup.is_some() {
            txn.commit().await.map_err(DomainError::from)?;
            crate::metrics::inc("numbering_provider_callbacks_duplicate_total", &[]);
            return Ok(Ingested {
                event_id: None,
                disposition: "DUPLICATE",
                duplicate: true,
            });
        }
        let resource = resource_of(&txn, provider_id, &ev.external_ref).await?;
        let command = match ev.request_id {
            Some(id) => CommandRow::find_by_statement(stmt(
                "SELECT * FROM number_provider_commands WHERE id = $1 AND provider_id = $2",
                vec![id.into(), provider_id.into()],
            ))
            .one(&txn)
            .await
            .map_err(DomainError::from)?
            .filter(|c| {
                resource
                    .as_ref()
                    .is_some_and(|r| r.id == c.number_resource_id)
            }),
            None => None,
        };
        #[derive(FromQueryResult)]
        struct Max {
            n: Option<i64>,
        }
        let latest = Max::find_by_statement(stmt(
            "SELECT MAX(provider_sequence) AS n FROM number_provider_events WHERE provider_id = $1 AND external_ref = $2 \
               AND disposition <> 'STALE'",
            vec![provider_id.into(), ev.external_ref.clone().into()],
        ))
        .one(&txn)
        .await
        .map_err(DomainError::from)?
        .and_then(|m| m.n);

        let mut reconcile: Option<Uuid> = None;
        let mut capability_update = false;
        let disposition: &'static str = match &resource {
            None => {
                incidents::raise(
                    &txn,
                    incidents::Category::UnmatchedProviderEvent,
                    &format!("unmatched:{provider_id}"),
                    incidents::Subject {
                        provider_id: Some(provider_id.into()),
                        ..Default::default()
                    },
                    json!({ "eventKind": ev.kind.as_str() }),
                    now,
                )
                .await
                .map_err(DomainError::from)?;
                "UNMATCHED"
            }
            Some(_) if matches!((ev.sequence, latest), (Some(s), Some(l)) if s < l) => "STALE",
            Some(r)
                if matches!(
                    r.lifecycle_state(),
                    S::Released | S::Quarantined | S::Reassignable
                ) && !matches!(ev.kind, K::Released | K::NumberRevoked) =>
            {
                "IGNORED_TERMINAL"
            }
            Some(r) => match ev.kind {
                K::CapabilityChanged => {
                    if ev.capabilities.is_none() {
                        "CONFLICT"
                    } else {
                        capability_update = true;
                        "APPLIED"
                    }
                }
                K::NumberRevoked | K::Released
                    if command.as_ref().is_none_or(|c| c.operation != "RELEASE") =>
                {
                    if matches!(
                        r.lifecycle_state(),
                        S::Released | S::Quarantined | S::Reassignable
                    ) {
                        "IGNORED_TERMINAL"
                    } else {
                        incidents::raise(
                            &txn,
                            incidents::Category::ProviderReleasedUnexpectedly,
                            &format!("provider-released:{}", r.id),
                            incidents::Subject {
                                provider_id: Some(provider_id.into()),
                                number_resource_id: Some(r.id),
                                command_id: None,
                            },
                            json!({ "eventKind": ev.kind.as_str(), "localState": r.state }),
                            now,
                        )
                        .await
                        .map_err(DomainError::from)?;
                        "CONFLICT"
                    }
                }
                K::HoldExpired if command.is_none() && r.lifecycle_state() == S::Reserved => {
                    incidents::raise(
                        &txn,
                        incidents::Category::ProviderStateDrift,
                        &format!("hold-expired:{}", r.id),
                        incidents::Subject {
                            provider_id: Some(provider_id.into()),
                            number_resource_id: Some(r.id),
                            command_id: None,
                        },
                        json!({ "reason": "PROVIDER_HOLD_LAPSED" }),
                        now,
                    )
                    .await
                    .map_err(DomainError::from)?;
                    "CONFLICT"
                }
                _ => match &command {
                    Some(c)
                        if matches!(
                            c.status(),
                            CommandStatus::Unknown
                                | CommandStatus::Dispatching
                                | CommandStatus::Pending
                        ) =>
                    {
                        reconcile = Some(c.id);
                        "RECONCILE_REQUESTED"
                    }
                    _ => "CONFIRMED_EXISTING",
                },
            },
        };
        let row = ProviderEventRow::find_by_statement(stmt(
            "INSERT INTO number_provider_events (id, provider_id, provider_event_id, event_kind, external_ref, number_resource_id, \
               command_id, provider_sequence, occurred_at, received_at, payload_sha256, normalized, disposition, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                provider_id.into(),
                ev.provider_event_id.clone().into(),
                ev.kind.as_str().into(),
                ev.external_ref.clone().into(),
                resource.as_ref().map(|r| r.id).into(),
                command.as_ref().map(|c| c.id).into(),
                ev.sequence.into(),
                ev.occurred_at.into(),
                now.into(),
                digest.into(),
                normalized(&ev).into(),
                disposition.into(),
                ctx.correlation_id.as_str().into(),
            ],
        ))
        .one(&txn)
        .await
        .map_err(DomainError::from)?
        .expect("insert returns the row");
        if let Some(cmd) = reconcile {
            jobs::enqueue(
                &txn,
                NewJob {
                    kind: RECONCILE_JOB,
                    dedupe_key: Some(format!("reconcile:{cmd}:{}", row.id)),
                    payload: json!({ "command_id": cmd }),
                    retry_safety: RetrySafety::Idempotent,
                    max_attempts: 20,
                    run_at: now,
                },
                ctx,
                now,
            )
            .await
            .map_err(DomainError::from)?;
        }
        outbox::enqueue(
            &txn,
            NewEvent {
                event_type: "number.provider-event.received",
                aggregate_type: if resource.is_some() {
                    "NUMBER_RESOURCE"
                } else {
                    "PROVIDER_EVENT"
                },
                aggregate_id: resource.as_ref().map(|r| r.id).unwrap_or(row.id),
                scope: None,
                payload: json!({
                    "provider_event_ref": row.id,
                    "event_kind": row.event_kind,
                    "number_resource_id": row.number_resource_id,
                    "provider_command_id": row.command_id,
                    "disposition": disposition,
                }),
            },
            ctx,
            now,
        )
        .await
        .map_err(DomainError::from)?;
        txn.commit().await.map_err(DomainError::from)?;
        crate::metrics::inc(
            "numbering_provider_callbacks_total",
            &[("disposition", disposition)],
        );
        if capability_update {
            if let (Some(r), Some(caps)) = (&resource, &ev.capabilities) {
                self.apply_capability_change(ctx, r, caps, ev.occurred_at)
                    .await?;
            }
        }
        Ok(Ingested {
            event_id: Some(row.id),
            disposition,
            duplicate: false,
        })
    }

    /// The provider's complete current capability set: capabilities it no longer serves
    /// are withdrawn at once (E downgrade), newly reported ones recorded. Stale evidence is
    /// refused by E itself.
    async fn apply_capability_change(
        &self,
        ctx: &RequestContext,
        resource: &ResourceRow,
        current: &[Capability],
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> DomainResult<()> {
        let synthetic = resource.inventory() == InventorySource::SyntheticTest;
        let source = if synthetic {
            CapabilitySource::Synthetic
        } else {
            CapabilitySource::ProviderInventory
        };
        let recorded = self.numbers.current_capabilities(resource.id).await?;
        for rec in recorded {
            let Some(cap) = Capability::parse(&rec.capability) else {
                continue;
            };
            let granting = matches!(rec.status.as_str(), "VERIFIED" | "DECLARED");
            if granting && !current.contains(&cap) {
                let mut new = NewCapability::declared(cap, source);
                new.status = CapabilityStatus::Withdrawn;
                new.source_observed_at = Some(observed_at);
                new.acknowledge_impact = true;
                new.reason_code = Some("PROVIDER_DOWNGRADE".into());
                match self.numbers.record_capability(ctx, resource.id, new).await {
                    Ok(_)
                    | Err(DomainError::Precondition {
                        code: "STALE_EVIDENCE",
                        ..
                    }) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        for cap in current {
            let new = {
                let mut n = NewCapability::declared(*cap, source);
                n.source_observed_at = Some(observed_at);
                n.reason_code = Some("PROVIDER_REPORTED".into());
                n
            };
            match self.numbers.record_capability(ctx, resource.id, new).await {
                Ok(_)
                | Err(DomainError::Precondition {
                    code: "STALE_EVIDENCE",
                    ..
                })
                | Err(DomainError::Precondition {
                    code: "SOURCE_AUTHORITY_CONFLICT",
                    ..
                })
                | Err(DomainError::Precondition {
                    code: "REVOCATION_ACTIVE",
                    ..
                }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub async fn provider_events(&self, resource_id: Uuid) -> DomainResult<Vec<ProviderEventRow>> {
        Ok(ProviderEventRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_events WHERE number_resource_id = $1 ORDER BY received_at, id",
            vec![resource_id.into()],
        ))
        .all(self.numbers.db())
        .await?)
    }
}

fn normalized(ev: &NormalizedProviderEvent) -> serde_json::Value {
    json!({
        "kind": ev.kind.as_str(),
        "requestId": ev.request_id,
        "sequence": ev.sequence,
        "occurredAt": ev.occurred_at,
        "capabilities": ev.capabilities,
    })
}

//! Number Provider Bindings (Part IV §D3): which provider serves which capabilities of a
//! number, with full history. Provider identifiers are INTERNAL: they never appear in a
//! customer response, an event payload or the canonical identity. Replacing the provider
//! (or porting) ends the old binding and starts a new one; the Number Resource id stays.
//!
//! At most one ACTIVE binding serves each routable capability (database-enforced with the
//! resource row locked, so concurrent activations cannot both win). The executable
//! provider adapter is Workstream G; this is the domain and persistence model.

use chrono::{DateTime, Utc};
use sea_orm::{DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    inventory::valid_provider, resources::lock, stmt, BindingChangeReason, Capability, DomainError,
    DomainResult, InventorySource, NumberDomain,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

/// A binding as stored (internal view; contains the provider reference).
#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct BindingRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub provider_id: String,
    pub external_ref: String,
    pub capabilities: Vec<String>,
    pub status: String,
    pub binding_revision: i64,
    pub change_reason: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub superseded_by_binding_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// The provider-independent view of a binding (what leaves Numbering).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingRef {
    pub binding_ref: Uuid,
    pub capabilities: Vec<String>,
    pub binding_revision: i64,
    pub active: bool,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
}

impl BindingRow {
    pub fn public_ref(&self) -> BindingRef {
        BindingRef {
            binding_ref: self.id,
            capabilities: self.capabilities.clone(),
            binding_revision: self.binding_revision,
            active: self.status == "ACTIVE",
            effective_from: self.effective_from,
            effective_to: self.effective_to,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewBinding {
    pub provider_id: String,
    pub external_ref: String,
    pub capabilities: Vec<Capability>,
    pub change_reason: BindingChangeReason,
}

fn validate(new: &NewBinding, source: InventorySource) -> DomainResult<Vec<String>> {
    if !valid_provider(&new.provider_id) {
        return Err(DomainError::Validation(
            "invalid provider identifier".into(),
        ));
    }
    if (source == InventorySource::SyntheticTest) != (new.provider_id == "synthetic") {
        return Err(DomainError::Validation(
            "synthetic inventory binds to the synthetic provider only, and only synthetic inventory does".into(),
        ));
    }
    if new.external_ref.is_empty()
        || new.external_ref.len() > 200
        || new.external_ref.chars().any(char::is_control)
    {
        return Err(DomainError::Validation("invalid provider reference".into()));
    }
    if new.capabilities.is_empty() || new.capabilities.iter().any(|c| !c.is_routable()) {
        return Err(DomainError::Validation(
            "a binding serves one or more routable capabilities (voice / SMS / MMS)".into(),
        ));
    }
    let mut caps: Vec<String> = new
        .capabilities
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    caps.sort();
    caps.dedup();
    Ok(caps)
}

/// Inserts an ACTIVE binding at the next binding revision (resource row already locked).
async fn insert_active(
    txn: &DatabaseTransaction,
    id: Uuid,
    resource_id: Uuid,
    new: &NewBinding,
    caps: Vec<String>,
    now: DateTime<Utc>,
) -> DomainResult<BindingRow> {
    let revision = super::resources::ResourceRow::find_by_statement(stmt(
        "UPDATE number_resources SET binding_revision = binding_revision + 1, revision = revision + 1, \
           updated_at = $2 WHERE id = $1 RETURNING *",
        vec![resource_id.into(), now.into()],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::NotFound("number resource"))?
    .binding_revision;
    Ok(BindingRow::find_by_statement(stmt(
        "INSERT INTO number_provider_bindings (id, number_resource_id, provider_id, external_ref, capabilities, \
           status, binding_revision, change_reason, effective_from, created_at) \
         VALUES ($1, $2, $3, $4, $5, 'ACTIVE', $6, $7, $8, $8) RETURNING *",
        vec![
            id.into(),
            resource_id.into(),
            new.provider_id.clone().into(),
            new.external_ref.clone().into(),
            caps.into(),
            revision.into(),
            new.change_reason.as_str().into(),
            now.into(),
        ],
    ))
    .one(txn)
    .await?
    .expect("insert returns the row"))
}

async fn event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    binding: &BindingRow,
    change: &str,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.binding.changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: binding.number_resource_id,
            scope: None,
            payload: json!({
                "number_resource_id": binding.number_resource_id,
                "binding_ref": binding.id,
                "change": change,
                "capabilities": binding.capabilities,
                "binding_revision": binding.binding_revision,
                "change_reason": binding.change_reason,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

impl NumberDomain {
    /// Activates a binding. A conflicting active binding for any of its capabilities is
    /// refused (`BINDING_CONFLICT`).
    pub async fn activate_binding(
        &self,
        ctx: &RequestContext,
        resource_id: Uuid,
        new: NewBinding,
    ) -> DomainResult<BindingRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let row = lock(&txn, resource_id).await?;
        let caps = validate(&new, row.inventory())?;
        let binding = insert_active(&txn, Uuid::new_v4(), resource_id, &new, caps, now).await?;
        event(&txn, ctx, &binding, "ACTIVATED", now).await?;
        txn.commit().await?;
        Ok(binding)
    }

    /// Ends `old` and activates `new` in one transaction (provider replacement / port):
    /// the canonical resource is unchanged and the history keeps both.
    pub async fn replace_binding(
        &self,
        ctx: &RequestContext,
        old_binding_id: Uuid,
        new: NewBinding,
    ) -> DomainResult<BindingRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let old = BindingRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_bindings WHERE id = $1",
            vec![old_binding_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("provider binding"))?;
        let row = lock(&txn, old.number_resource_id).await?;
        if old.status != "ACTIVE" {
            return Err(DomainError::precondition(
                "BINDING_ENDED",
                "only an active binding can be replaced",
            ));
        }
        let caps = validate(&new, row.inventory())?;
        // End and link first (frees its capabilities; the successor link is a deferred
        // foreign key), then activate the successor.
        let successor_id = Uuid::new_v4();
        let ended = BindingRow::find_by_statement(stmt(
            "UPDATE number_provider_bindings SET status = 'ENDED', effective_to = $2, superseded_by_binding_id = $3 \
             WHERE id = $1 RETURNING *",
            vec![old.id.into(), now.into(), successor_id.into()],
        ))
        .one(&txn)
        .await?
        .expect("locked binding");
        let successor =
            insert_active(&txn, successor_id, old.number_resource_id, &new, caps, now).await?;
        event(&txn, ctx, &ended, "ENDED", now).await?;
        event(&txn, ctx, &successor, "ACTIVATED", now).await?;
        txn.commit().await?;
        Ok(successor)
    }

    /// Ends an active binding (no successor), e.g. on release.
    pub async fn end_binding(
        &self,
        ctx: &RequestContext,
        binding_id: Uuid,
    ) -> DomainResult<BindingRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = BindingRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_bindings WHERE id = $1",
            vec![binding_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("provider binding"))?;
        lock(&txn, current.number_resource_id).await?;
        if current.status != "ACTIVE" {
            return Err(DomainError::precondition(
                "BINDING_ENDED",
                "the binding has already ended",
            ));
        }
        let ended = BindingRow::find_by_statement(stmt(
            "UPDATE number_provider_bindings SET status = 'ENDED', effective_to = $2 WHERE id = $1 RETURNING *",
            vec![binding_id.into(), now.into()],
        ))
        .one(&txn)
        .await?
        .expect("locked binding");
        event(&txn, ctx, &ended, "ENDED", now).await?;
        txn.commit().await?;
        Ok(ended)
    }

    /// Every binding of a resource, oldest first (internal).
    pub async fn bindings(&self, resource_id: Uuid) -> DomainResult<Vec<BindingRow>> {
        Ok(BindingRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_bindings WHERE number_resource_id = $1 ORDER BY binding_revision",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }
}

//! Operator inventory management (§34, `privacy-relay-admin relay-number …`). There is
//! deliberately no network API: provisioning belongs to a future Telephony / numbering
//! capability, and until then an operator with shell access adds, drains and disables
//! numbers here. Every change appends the inventory ledger.

use std::collections::BTreeSet;

use sea_orm::TransactionTrait;
use uuid::Uuid;

use super::{
    assignment::{ledger, AssignmentAudit},
    InventoryRow, INVENTORY_COLUMNS,
};
use crate::{
    contact_vault::normalization::canonical_public_number,
    db,
    error::{ErrorCode, RelayError, RelayResult},
    models::{ContextRow, CONTEXT_COLUMNS},
    relay::session::session_row,
    service::RelayService,
    vocabulary::{RelayAssignmentEndReason, RelayNumberCapability, RelayNumberStatus},
};

const OPERATOR: (&str, &str) = ("OPERATOR", "privacy-relay-admin");

/// A number to add to the inventory.
pub struct NewRelayNumber<'a> {
    /// International form (`+…`); canonicalized by the same parser as contacts.
    pub number: &'a str,
    pub region: Option<&'a str>,
    pub capabilities: &'a [&'a str],
    /// Opaque internal provider binding (never the number, never exposed).
    pub provider_binding_ref: &'a str,
    /// AVAILABLE immediately (else RESERVED until `activate`).
    pub available: bool,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct Holder {
    context_id: Uuid,
    relay_session_id: Uuid,
}

impl RelayService {
    /// Adds a platform-owned public relay number (generation 0).
    pub async fn add_relay_number(&self, new: NewRelayNumber<'_>) -> RelayResult<Uuid> {
        let number = canonical_public_number(new.number).map_err(|_| {
            RelayError::Validation("the relay number is not a valid international number")
        })?;
        let mut caps = BTreeSet::new();
        for c in new.capabilities {
            let c = RelayNumberCapability::parse(c).ok_or(RelayError::Validation(
                "capabilities: VOICE_INBOUND and/or VOICE_OUTBOUND",
            ))?;
            caps.insert(c.as_str().to_string());
        }
        if caps.is_empty() {
            return Err(RelayError::Validation(
                "at least one capability is required",
            ));
        }
        let status = if new.available {
            RelayNumberStatus::Available
        } else {
            RelayNumberStatus::Reserved
        };
        let now = self.now();
        let id = Uuid::new_v4();
        let txn = self.db.begin().await?;
        db::exec(
            &txn,
            "INSERT INTO relay_number_inventory (id, canonical_public_e164, country_iso2, region, capabilities, \
               provider_binding_ref, operational_status, generation, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $8)",
            vec![
                id.into(),
                number.e164.clone().into(),
                number.country_iso2.clone().into(),
                new.region.map(str::to_string).into(),
                caps.into_iter().collect::<Vec<_>>().into(),
                new.provider_binding_ref.into(),
                status.as_str().into(),
                now.into(),
            ],
        )
        .await?;
        ledger(
            &txn,
            id,
            "RELAY_NUMBER_ADDED",
            None,
            status,
            0,
            None,
            None,
            OPERATOR,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(id)
    }

    async fn transition(
        &self,
        id: Uuid,
        allowed_from: &[RelayNumberStatus],
        to: RelayNumberStatus,
        stamp: &str,
        event: &str,
    ) -> RelayResult<()> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let row: InventoryRow = db::one(
            &txn,
            &format!(
                "SELECT {INVENTORY_COLUMNS} FROM relay_number_inventory WHERE id = $1 FOR UPDATE"
            ),
            vec![id.into()],
        )
        .await?
        .ok_or(RelayError::Refused(ErrorCode::NotFound))?;
        let from = row.status()?;
        if from == to {
            txn.rollback().await?;
            return Ok(());
        }
        if !allowed_from.contains(&from) {
            return Err(RelayError::Coded(
                ErrorCode::ValidationError,
                "The relay number cannot make this transition from its current status.",
            ));
        }
        db::exec(
            &txn,
            &format!(
                "UPDATE relay_number_inventory SET operational_status = $2, {stamp} updated_at = GREATEST(updated_at, $3) \
                 WHERE id = $1"
            ),
            vec![id.into(), to.as_str().into(), now.into()],
        )
        .await?;
        ledger(
            &txn,
            id,
            event,
            Some(from),
            to,
            row.generation,
            None,
            None,
            OPERATOR,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// RESERVED → AVAILABLE.
    pub async fn activate_relay_number(&self, id: Uuid) -> RelayResult<()> {
        self.transition(
            id,
            &[RelayNumberStatus::Reserved],
            RelayNumberStatus::Available,
            "",
            "RELAY_NUMBER_ACTIVATED",
        )
        .await
    }

    /// UNAVAILABLE → RESERVED (an operator restores a disabled number; it is offered
    /// again only after `activate`, and never while an assignment still holds it).
    pub async fn enable_relay_number(&self, id: Uuid) -> RelayResult<()> {
        self.transition(
            id,
            &[RelayNumberStatus::Unavailable],
            RelayNumberStatus::Reserved,
            "unavailable_at = NULL, draining_at = NULL,",
            "RELAY_NUMBER_ADDED",
        )
        .await
    }

    /// Stops offering the number; an active assignment keeps its authority until it
    /// ends, then the drain completes (UNAVAILABLE) after the cooldown.
    pub async fn drain_relay_number(&self, id: Uuid) -> RelayResult<()> {
        self.transition(
            id,
            &[
                RelayNumberStatus::Reserved,
                RelayNumberStatus::Available,
                RelayNumberStatus::Assigned,
                RelayNumberStatus::Cooldown,
            ],
            RelayNumberStatus::Draining,
            "draining_at = $3,",
            "RELAY_NUMBER_DRAINING",
        )
        .await
    }

    /// Takes the number out of service NOW: an active assignment loses its routing
    /// authority (REVOKED after its cooldown) and the number becomes UNAVAILABLE.
    pub async fn disable_relay_number(&self, id: Uuid) -> RelayResult<()> {
        let now = self.now();
        let holder: Option<Holder> = db::one(
            &self.db,
            "SELECT context_id, relay_session_id FROM relay_number_assignments \
             WHERE relay_number_id = $1 AND status = 'ACTIVE'",
            vec![id.into()],
        )
        .await?;
        let txn = self.db.begin().await?;
        if let Some(holder) = holder {
            // Lock order: context, session, then (inside the release) assignment, number.
            let ctx_row: ContextRow = db::one(
                &txn,
                &format!(
                    "SELECT {CONTEXT_COLUMNS} FROM communication_contexts WHERE id = $1 FOR UPDATE"
                ),
                vec![holder.context_id.into()],
            )
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
            let cmd = crate::workers::system_cmd(&ctx_row)?;
            let session = session_row(
                &txn,
                cmd.scope(),
                holder.relay_session_id,
                crate::context::repository::Lock::Update,
            )
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
            self.end_relay_assignment_in(
                &txn,
                &AssignmentAudit {
                    cmd: &cmd,
                    context_id: ctx_row.id,
                    context_version: ctx_row.version,
                    context_status: ctx_row.status()?.as_str(),
                    session_id: session.id,
                    session_version: session.version,
                },
                RelayAssignmentEndReason::NumberDisabled,
                now,
            )
            .await?;
        }
        let row: InventoryRow = db::one(
            &txn,
            &format!(
                "SELECT {INVENTORY_COLUMNS} FROM relay_number_inventory WHERE id = $1 FOR UPDATE"
            ),
            vec![id.into()],
        )
        .await?
        .ok_or(RelayError::Refused(ErrorCode::NotFound))?;
        let from = row.status()?;
        if from == RelayNumberStatus::Unavailable {
            txn.commit().await?;
            return Ok(());
        }
        if from == RelayNumberStatus::Assigned {
            // An assignment was created between the probe and the lock: retry.
            return Err(RelayError::Refused(ErrorCode::ServiceUnavailable));
        }
        db::exec(
            &txn,
            "UPDATE relay_number_inventory SET operational_status = 'UNAVAILABLE', unavailable_at = $2, \
               updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        ledger(
            &txn,
            id,
            "RELAY_NUMBER_DISABLED",
            Some(from),
            RelayNumberStatus::Unavailable,
            row.generation,
            None,
            None,
            OPERATOR,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// The inventory (operator output; numbers masked).
    pub async fn list_relay_numbers(&self) -> RelayResult<Vec<InventoryRow>> {
        db::all(
            &self.db,
            &format!(
                "SELECT {INVENTORY_COLUMNS} FROM relay_number_inventory \
                 ORDER BY country_iso2, operational_status, created_at, id"
            ),
            vec![],
        )
        .await
    }
}

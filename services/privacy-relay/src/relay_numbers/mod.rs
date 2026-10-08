//! Temporary relay-number pool (Part D §34): platform-owned PUBLIC relay identities lent
//! to ONE Privacy Relay Session at a time, then quarantined (cooldown) before reuse.
//!
//! This is temporary relay infrastructure inside Privacy Relay — not a numbering
//! product (no customer numbers, porting, marketplace or ownership). There is no carrier
//! SDK and no provisioning API: an operator manages the inventory with
//! `privacy-relay-admin relay-number …`, and `provider_binding_ref` stays an opaque
//! internal reference that no API, event or log ever carries.
//!
//! * Allocation is deterministic and concurrency-safe: the session's context and session
//!   rows are locked (one assignment per session; a replay returns the same assignment),
//!   then the least recently changed eligible number is claimed with
//!   `FOR UPDATE SKIP LOCKED`. The database independently guarantees one holder per
//!   number (ACTIVE or COOLDOWN), containment in the session, generation monotonicity and
//!   that a held number is never offered again.
//! * Routing authority ends atomically with its session (revoke, completion beyond grace,
//!   expiry, context cascade), its own expiry, an explicit release or the number being
//!   disabled: the assignment and the number enter COOLDOWN in the same transaction, and
//!   only the cooldown worker returns the number to AVAILABLE.
//! * `RELAY_NUMBER_POOL_ENABLED=false` refuses NEW assignments only; release, expiry,
//!   cooldown and drain keep running.
//! * Lock order everywhere: context → session → assignment → inventory (allocation never
//!   waits on an inventory row: SKIP LOCKED).

pub mod assignment;
pub mod inbound;
pub mod inventory;

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    config::{FeatureFlag, DEFAULT_RELAY_NUMBER_COOLDOWN_SECONDS},
    context::aggregate::ts,
    error::{ErrorCode, RelayError, RelayResult},
    vocabulary::{RelayAssignmentEndReason, RelayAssignmentStatus, RelayNumberStatus},
};

/// `RELAY_NUMBER_POOL_ENABLED` + `RELAY_NUMBER_COOLDOWN_SECONDS`.
#[derive(Debug, Clone)]
pub struct RelayNumberPoolSettings {
    pub enabled: FeatureFlag,
    pub cooldown: chrono::Duration,
}

impl Default for RelayNumberPoolSettings {
    fn default() -> Self {
        Self {
            enabled: FeatureFlag::new(false),
            cooldown: chrono::Duration::seconds(DEFAULT_RELAY_NUMBER_COOLDOWN_SECONDS as i64),
        }
    }
}

fn corrupt(what: &'static str) -> RelayError {
    tracing::error!(what, "corrupt relay number row");
    RelayError::Refused(ErrorCode::InternalError)
}

pub(crate) const ASSIGNMENT_COLUMNS: &str = "id, relay_number_id, tenant_id, application_id, \
    environment_id, context_id, relay_session_id, assignment_generation, country_iso2, region, \
    capabilities, status, end_reason, assigned_at, expires_at, released_at, cooldown_until";

/// A relay-number assignment (never carries the number or the provider binding).
#[derive(Debug, Clone, FromQueryResult)]
pub struct AssignmentRow {
    pub id: Uuid,
    pub relay_number_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub context_id: Uuid,
    pub relay_session_id: Uuid,
    pub assignment_generation: i64,
    pub country_iso2: String,
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    pub status: String,
    pub end_reason: Option<String>,
    pub assigned_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
    pub cooldown_until: Option<DateTime<Utc>>,
}

impl AssignmentRow {
    pub fn status(&self) -> RelayResult<RelayAssignmentStatus> {
        RelayAssignmentStatus::parse(&self.status).ok_or_else(|| corrupt("assignment status"))
    }

    pub fn end_reason(&self) -> RelayResult<Option<RelayAssignmentEndReason>> {
        self.end_reason
            .as_deref()
            .map(|r| RelayAssignmentEndReason::parse(r).ok_or_else(|| corrupt("end reason")))
            .transpose()
    }

    /// The consumer-safe view: ids, generation, country, capabilities, lifecycle. No
    /// number, no provider binding, no inventory id.
    pub fn view(&self) -> RelayResult<Value> {
        let mut v = json!({
            "assignmentId": self.id,
            "relaySessionId": self.relay_session_id,
            "contextId": self.context_id,
            "status": self.status()?.as_str(),
            "assignmentGeneration": self.assignment_generation,
            "country": self.country_iso2,
            "capabilities": self.capabilities,
            "assignedAt": ts(self.assigned_at),
            "expiresAt": ts(self.expires_at),
        });
        if let Some(region) = &self.region {
            v["region"] = json!(region);
        }
        if let Some(reason) = self.end_reason()? {
            v["endReason"] = json!(reason.as_str());
        }
        for (key, value) in [
            ("releasedAt", self.released_at),
            ("cooldownUntil", self.cooldown_until),
        ] {
            if let Some(at) = value {
                v[key] = json!(ts(at));
            }
        }
        Ok(v)
    }
}

pub(crate) const INVENTORY_COLUMNS: &str = "id, canonical_public_e164, country_iso2, region, \
    capabilities, operational_status, generation, created_at, updated_at, draining_at, \
    cooldown_until, unavailable_at";

/// An inventory row. The number is platform-owned public data, but it is still only
/// shown masked (operator output) and never leaves Privacy Relay.
#[derive(Clone, FromQueryResult)]
pub struct InventoryRow {
    pub id: Uuid,
    pub(crate) canonical_public_e164: String,
    pub country_iso2: String,
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    pub operational_status: String,
    pub generation: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub draining_at: Option<DateTime<Utc>>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub unavailable_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for InventoryRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InventoryRow")
            .field("id", &self.id)
            .field("country_iso2", &self.country_iso2)
            .field("operational_status", &self.operational_status)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl InventoryRow {
    pub fn status(&self) -> RelayResult<RelayNumberStatus> {
        RelayNumberStatus::parse(&self.operational_status).ok_or_else(|| corrupt("number status"))
    }

    /// Masked display (`+44••••••0123`-style) for operator output.
    pub fn masked(&self) -> String {
        crate::contact_vault::normalization::mask_public_number(&self.canonical_public_e164)
    }
}

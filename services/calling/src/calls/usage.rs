//! Provider-independent call usage records (Part C §16).
//!
//! Usage records are consumption facts (connected time of the Call, a participant, a
//! session or a leg). They are idempotent per Call and idempotency key, and finalization
//! is set-once and idempotent. This is not billing: no invoicing, pricing, provider cost
//! settlement or quota accounting happens here.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{
    errors::CallError,
    ids::{CallId, CallLegId, CallParticipantId, CallSessionId, CallUsageRecordId},
    kinds::{UsageType, UsageUnit},
};
use crate::models::call_usage_records;

/// Caller-chosen idempotency key, unique per Call (`^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: &str) -> Result<Self, CallError> {
        let bytes = value.as_bytes();
        let ok = !bytes.is_empty()
            && bytes.len() <= 128
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
        if ok {
            Ok(Self(value.to_string()))
        } else {
            Err(CallError::Validation("invalid idempotency key"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A usage fact to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUsage {
    pub usage_type: UsageType,
    pub participant_id: Option<CallParticipantId>,
    pub session_id: Option<CallSessionId>,
    pub leg_id: Option<CallLegId>,
    pub quantity: i64,
    pub unit: UsageUnit,
    pub usage_started_at: Option<DateTime<Utc>>,
    pub usage_ended_at: Option<DateTime<Utc>>,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallUsageRecord {
    pub id: CallUsageRecordId,
    pub call_id: CallId,
    pub usage_type: UsageType,
    pub participant_id: Option<CallParticipantId>,
    pub session_id: Option<CallSessionId>,
    pub leg_id: Option<CallLegId>,
    pub quantity: i64,
    pub unit: UsageUnit,
    pub usage_started_at: Option<DateTime<Utc>>,
    pub usage_ended_at: Option<DateTime<Utc>>,
    pub idempotency_key: String,
    pub finalized_at: Option<DateTime<Utc>>,
}

impl CallUsageRecord {
    /// Whether this persisted record states exactly the fact `new` describes.
    pub fn matches(&self, new: &NewUsage) -> bool {
        self.usage_type == new.usage_type
            && self.participant_id == new.participant_id
            && self.session_id == new.session_id
            && self.leg_id == new.leg_id
            && self.quantity == new.quantity
            && self.unit == new.unit
            && self.usage_started_at == new.usage_started_at
            && self.usage_ended_at == new.usage_ended_at
    }
}

impl From<call_usage_records::Model> for CallUsageRecord {
    fn from(m: call_usage_records::Model) -> Self {
        Self {
            id: CallUsageRecordId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            usage_type: m.usage_type,
            participant_id: m.participant_id.map(CallParticipantId::from_uuid),
            session_id: m.session_id.map(CallSessionId::from_uuid),
            leg_id: m.leg_id.map(CallLegId::from_uuid),
            quantity: m.quantity,
            unit: m.unit,
            usage_started_at: m.usage_started_at,
            usage_ended_at: m.usage_ended_at,
            idempotency_key: m.idempotency_key,
            finalized_at: m.finalized_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_keys_are_bounded() {
        assert!(IdempotencyKey::parse("participant-connected-time:6f1c2b9e").is_ok());
        for bad in ["", ":x", "has space", &"k".repeat(129), "a/b"] {
            assert!(IdempotencyKey::parse(bad).is_err(), "{bad}");
        }
    }
}

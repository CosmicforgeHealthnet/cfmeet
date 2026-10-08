//! Normalized Telephony events as Calling receives them (`cfmeet.telephony.event` v1,
//! `POST /internal/v1/telephony/events`, permission `telephony-event.apply`).
//!
//! CF Meet ids, the normalized vocabulary and timestamps only: no provider id, provider
//! call id, SIP data, raw provider payload, carrier status or telephone number. Calling
//! deduplicates by `eventId` and orders by the per-leg `sequence`; it validates every
//! transition against its own state machine — Telephony never sets Calling state.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

pub const EVENT_SCHEMA_VERSION: &str = "1";

/// Normalized event types Calling understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelephonyEventType {
    LegAccepted,
    LegRinging,
    LegAnswered,
    LegConnected,
    LegHeld,
    LegResumed,
    LegEnded,
    LegFailed,
    LegBusy,
    LegNoAnswer,
    LegDeclined,
    LegStatusUnknown,
    BridgeEstablished,
    BridgeEnded,
    UsageFinalized,
    /// Facts Calling records but does not act on (DTMF, reconciliation results).
    Other,
}

impl TelephonyEventType {
    pub fn parse(s: &str) -> Self {
        use TelephonyEventType::*;
        match s {
            "LEG_ACCEPTED" => LegAccepted,
            "LEG_RINGING" => LegRinging,
            "LEG_ANSWERED" => LegAnswered,
            "LEG_CONNECTED" => LegConnected,
            "LEG_HELD" => LegHeld,
            "LEG_RESUMED" => LegResumed,
            "LEG_ENDED" => LegEnded,
            "LEG_FAILED" => LegFailed,
            "LEG_BUSY" => LegBusy,
            "LEG_NO_ANSWER" => LegNoAnswer,
            "LEG_DECLINED" => LegDeclined,
            "LEG_STATUS_UNKNOWN" => LegStatusUnknown,
            "BRIDGE_ESTABLISHED" => BridgeEstablished,
            "BRIDGE_ENDED" => BridgeEnded,
            "USAGE_FINALIZED" => UsageFinalized,
            _ => Other,
        }
    }

    pub fn is_terminal_leg(&self) -> bool {
        use TelephonyEventType::*;
        matches!(
            self,
            LegEnded | LegFailed | LegBusy | LegNoAnswer | LegDeclined
        )
    }
}

/// Usage of a finalized public leg.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LegUsage {
    pub connected_duration_ms: i64,
    pub billable_seconds: i64,
    #[serde(default)]
    pub cost_minor: Option<i64>,
    pub cost_known: bool,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub destination_country: Option<String>,
}

/// One Telephony event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelephonyEvent {
    pub schema_version: String,
    pub event_id: Uuid,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub call_id: Uuid,
    #[serde(default)]
    pub call_leg_id: Option<Uuid>,
    #[serde(default)]
    pub telephony_leg_id: Option<Uuid>,
    #[serde(default)]
    pub attempt_id: Option<Uuid>,
    #[serde(default)]
    pub bridge_id: Option<Uuid>,
    #[serde(default)]
    pub sequence: Option<i64>,
    #[serde(default)]
    pub leg_status: Option<String>,
    #[serde(default)]
    pub end_reason: Option<String>,
    #[serde(default)]
    pub failure_category: Option<String>,
    /// For terminal / unknown statuses: whether the outcome is definite.
    #[serde(default)]
    pub definite: Option<bool>,
    #[serde(default)]
    pub usage: Option<LegUsage>,
}

impl TelephonyEvent {
    pub fn kind(&self) -> TelephonyEventType {
        TelephonyEventType::parse(&self.event_type)
    }

    /// Bounded shape checks (the transport is authenticated; this guards the parser).
    pub fn well_formed(&self) -> bool {
        let token = |s: &str| {
            !s.is_empty() && s.len() <= 48 && s.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        };
        self.schema_version == EVENT_SCHEMA_VERSION
            && token(&self.event_type)
            && self.leg_status.as_deref().is_none_or(token)
            && self.end_reason.as_deref().is_none_or(token)
            && self.failure_category.as_deref().is_none_or(token)
            && self.sequence.is_none_or(|s| s >= 1)
            && (self.call_leg_id.is_some() || self.bridge_id.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_parse_and_reject_malformed_shapes() {
        let e: TelephonyEvent = serde_json::from_value(serde_json::json!({
            "schemaVersion": "1", "eventId": Uuid::new_v4(), "eventType": "LEG_BUSY",
            "occurredAt": "2026-10-08T10:00:00Z", "callId": Uuid::new_v4(),
            "callLegId": Uuid::new_v4(), "telephonyLegId": Uuid::new_v4(), "sequence": 3,
            "legStatus": "FAILED", "endReason": "BUSY", "definite": true,
            "direction": "OUTBOUND", "routeClass": "APP_TO_PSTN"
        }))
        .unwrap();
        assert!(e.well_formed());
        assert!(e.kind().is_terminal_leg());
        let mut bad = e.clone();
        bad.end_reason = Some("+2348031234567".into());
        assert!(!bad.well_formed());
        let mut bad = e;
        bad.schema_version = "2".into();
        assert!(!bad.well_formed());
    }
}

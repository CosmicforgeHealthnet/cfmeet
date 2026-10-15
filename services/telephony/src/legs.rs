//! Telephony legs: the execution of one canonical Calling CallLeg attempt (§32–§33).
//!
//! Every status change goes through [`transition_in`], which validates the normalized
//! graph (mirrored by a database trigger), stamps first-entry timestamps, bumps the
//! version and writes the matching normalized event to the outbox IN THE SAME
//! TRANSACTION.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    domain::{Direction, EndReason, FailureCategory, LegStatus, NormalizedEvent},
    error::{ErrorCode, TelephonyError, TelephonyResult},
    events::outbox,
};

pub(crate) const LEG_COLUMNS: &str = "id, calling_call_id, calling_call_leg_id, calling_attempt_id, \
    tenant_id, application_id, environment_id, direction, route_class, route_id, provider_id, status, \
    generation, destination_country, contact_reference, relay_session_id, context_id, \
    privacy_decision_id, call_authorization_id, inbound_resolution_id, spend_reservation_id, \
    spend_reserved_minor, spend_currency, provider_cost_minor, provider_cost_currency, max_duration_seconds, media_attachment, end_reason, \
    failure_category, usage_status, reconcile_after, reconcile_lease_until, reconcile_attempts, \
    last_provider_sequence, event_sequence, created_at, originating_at, ringing_at, answered_at, \
    connected_at, ended_at, updated_at, version, enterprise_device_id, edge_channel_ref, relay_assignment_ref";

#[derive(Debug, Clone, FromQueryResult)]
pub struct LegRow {
    pub id: Uuid,
    pub calling_call_id: Uuid,
    pub calling_call_leg_id: Uuid,
    pub calling_attempt_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub direction: String,
    pub route_class: String,
    pub route_id: Option<String>,
    pub provider_id: Option<String>,
    pub status: String,
    pub generation: i32,
    pub destination_country: Option<String>,
    pub contact_reference: Option<Uuid>,
    pub relay_session_id: Option<Uuid>,
    pub context_id: Option<Uuid>,
    pub privacy_decision_id: Option<Uuid>,
    pub call_authorization_id: Option<Uuid>,
    pub inbound_resolution_id: Option<Uuid>,
    pub spend_reservation_id: Option<Uuid>,
    pub spend_reserved_minor: Option<i64>,
    pub spend_currency: Option<String>,
    pub provider_cost_minor: Option<i64>,
    pub provider_cost_currency: Option<String>,
    pub max_duration_seconds: i32,
    pub media_attachment: String,
    pub end_reason: Option<String>,
    pub failure_category: Option<String>,
    pub usage_status: String,
    pub reconcile_after: Option<DateTime<Utc>>,
    pub reconcile_lease_until: Option<DateTime<Utc>>,
    pub reconcile_attempts: i32,
    pub last_provider_sequence: Option<i64>,
    pub event_sequence: i64,
    pub created_at: DateTime<Utc>,
    pub originating_at: Option<DateTime<Utc>>,
    pub ringing_at: Option<DateTime<Utc>>,
    pub answered_at: Option<DateTime<Utc>>,
    pub connected_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
    /// Part F: the enterprise device of an ENTERPRISE_INTERNAL leg.
    pub enterprise_device_id: Option<Uuid>,
    /// Part F: the edge channel of an ENTERPRISE_INTERNAL leg (internal to Telephony).
    pub edge_channel_ref: Option<String>,
    /// Part H: Privacy Relay's assignment reference of the presented caller identity.
    pub relay_assignment_ref: Option<Uuid>,
}

impl LegRow {
    /// An internal enterprise SIP leg (no provider, no spend).
    pub fn is_enterprise(&self) -> bool {
        self.route_class == "ENTERPRISE_INTERNAL"
    }
}

impl LegRow {
    pub fn status(&self) -> TelephonyResult<LegStatus> {
        LegStatus::parse(&self.status).ok_or(TelephonyError::Refused(ErrorCode::InternalError))
    }

    pub fn direction(&self) -> Direction {
        Direction::parse(&self.direction).unwrap_or(Direction::Outbound)
    }

    /// The normalized, provider-free view returned to Calling (`GET /internal/v1/legs/{id}`).
    pub fn view(&self) -> Value {
        let mut v = json!({
            "telephonyLegId": self.id,
            "callId": self.calling_call_id,
            "callLegId": self.calling_call_leg_id,
            "direction": self.direction,
            "routeClass": self.route_class,
            "status": self.status,
            "generation": self.generation,
            "version": self.version,
            "createdAt": ts(self.created_at),
        });
        for (key, at) in [
            ("originatingAt", self.originating_at),
            ("ringingAt", self.ringing_at),
            ("answeredAt", self.answered_at),
            ("connectedAt", self.connected_at),
            ("endedAt", self.ended_at),
        ] {
            if let Some(at) = at {
                v[key] = json!(ts(at));
            }
        }
        if let Some(a) = self.calling_attempt_id {
            v["attemptId"] = json!(a);
        }
        if let Some(r) = &self.end_reason {
            v["endReason"] = json!(r);
        }
        if let Some(c) = &self.failure_category {
            v["failureCategory"] = json!(c);
        }
        if let Some(c) = &self.destination_country {
            v["destinationCountry"] = json!(c);
        }
        v
    }
}

pub fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

pub async fn load<C: ConnectionTrait>(db: &C, id: Uuid) -> TelephonyResult<Option<LegRow>> {
    db::one(
        db,
        &format!("SELECT {LEG_COLUMNS} FROM telephony_legs WHERE id = $1"),
        vec![id.into()],
    )
    .await
}

pub async fn lock<C: ConnectionTrait>(db: &C, id: Uuid) -> TelephonyResult<LegRow> {
    db::one(
        db,
        &format!("SELECT {LEG_COLUMNS} FROM telephony_legs WHERE id = $1 FOR UPDATE"),
        vec![id.into()],
    )
    .await?
    .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))
}

/// A requested status change.
#[derive(Debug, Clone)]
pub struct Change {
    pub target: LegStatus,
    pub event: NormalizedEvent,
    pub end_reason: Option<EndReason>,
    pub failure_category: Option<FailureCategory>,
    /// The outcome is definite (a failure Calling may safely fail over from).
    pub definite: bool,
    pub provider_sequence: Option<i64>,
    pub at: DateTime<Utc>,
    /// Internal bookkeeping transition without a normalized event (e.g. the transmit
    /// fence PLANNED -> ORIGINATING; Calling learns LEG_ACCEPTED from the outcome).
    pub silent: bool,
}

impl Change {
    pub fn to(target: LegStatus, event: NormalizedEvent, at: DateTime<Utc>) -> Self {
        Self {
            target,
            event,
            end_reason: None,
            failure_category: None,
            definite: true,
            provider_sequence: None,
            at,
            silent: false,
        }
    }

    pub fn ended(event: NormalizedEvent, reason: EndReason, at: DateTime<Utc>) -> Self {
        let failed = !matches!(reason, EndReason::Completed);
        let category = match reason {
            EndReason::Completed => None,
            EndReason::Busy | EndReason::NoAnswer | EndReason::Declined => {
                Some(FailureCategory::TargetUnavailable)
            }
            EndReason::Timeout => Some(FailureCategory::Timeout),
            EndReason::DurationLimit | EndReason::Cancelled => Some(FailureCategory::Policy),
            EndReason::NotSent => Some(FailureCategory::Execution),
            EndReason::AbsentAtProvider | EndReason::Failed | EndReason::Orphaned => {
                Some(FailureCategory::Execution)
            }
        };
        // A leg that never connected and ends for a non-completion reason is FAILED;
        // DURATION_LIMIT / CANCELLED / ORPHANED of a live leg end it normally.
        let target = if failed
            && !matches!(
                reason,
                EndReason::DurationLimit | EndReason::Cancelled | EndReason::Orphaned
            ) {
            LegStatus::Failed
        } else {
            LegStatus::Ended
        };
        Self {
            target,
            event,
            end_reason: Some(reason),
            failure_category: if target == LegStatus::Failed {
                category
            } else {
                None
            },
            definite: true,
            provider_sequence: None,
            at,
            silent: false,
        }
    }

    pub fn silent(mut self) -> Self {
        self.silent = true;
        self
    }

    pub fn indefinite(mut self) -> Self {
        self.definite = false;
        self
    }

    pub fn category(mut self, category: FailureCategory) -> Self {
        if self.target == LegStatus::Failed {
            self.failure_category = Some(category);
        }
        self
    }

    pub fn sequence(mut self, sequence: Option<i64>) -> Self {
        self.provider_sequence = sequence;
        self
    }
}

/// Applies `change` to the locked `leg` (same transaction), with its outbox event.
/// Returns None (and changes nothing) when the change is not a valid forward move.
pub async fn transition_in<C: ConnectionTrait>(
    txn: &C,
    leg: &LegRow,
    change: &Change,
) -> TelephonyResult<Option<LegRow>> {
    let from = leg.status()?;
    if from == change.target || !from.can_transition_to(change.target) {
        return Ok(None);
    }
    let at = change.at.max(leg.updated_at);
    let terminal = change.target.is_terminal();
    let set_first = |col: &str, current: Option<DateTime<Utc>>, when: bool| -> Option<String> {
        (when && current.is_none()).then(|| col.to_string())
    };
    let mut stamps: Vec<String> = Vec::new();
    stamps.extend(set_first(
        "originating_at",
        leg.originating_at,
        change.target == LegStatus::Originating,
    ));
    stamps.extend(set_first(
        "ringing_at",
        leg.ringing_at,
        change.target == LegStatus::Ringing,
    ));
    stamps.extend(set_first(
        "answered_at",
        leg.answered_at,
        matches!(change.target, LegStatus::Answered | LegStatus::Connected),
    ));
    stamps.extend(set_first(
        "connected_at",
        leg.connected_at,
        change.target == LegStatus::Connected,
    ));
    let assignments: String = stamps
        .iter()
        .map(|c| format!(", {c} = $2"))
        .collect::<String>();
    let reconcile = if terminal {
        ", reconcile_after = NULL, reconcile_lease_until = NULL"
    } else if change.target == LegStatus::Unknown {
        ", reconcile_after = $2"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE telephony_legs SET status = $1, updated_at = $2, version = version + 1, \
           ended_at = CASE WHEN $3 THEN $2 ELSE ended_at END, \
           end_reason = COALESCE($4, end_reason), failure_category = COALESCE($5, failure_category), \
           last_provider_sequence = GREATEST(last_provider_sequence, $6){assignments}{reconcile} \
         WHERE id = $7 AND version = $8 RETURNING {LEG_COLUMNS}"
    );
    let updated: Option<LegRow> = db::one(
        txn,
        &sql,
        vec![
            change.target.as_str().into(),
            at.into(),
            terminal.into(),
            terminal
                .then(|| change.end_reason.unwrap_or(EndReason::Completed).as_str())
                .into(),
            change.failure_category.map(|c| c.as_str()).into(),
            change.provider_sequence.into(),
            leg.id.into(),
            leg.version.into(),
        ],
    )
    .await?;
    let Some(updated) = updated else {
        return Err(TelephonyError::Refused(ErrorCode::LegNotActive));
    };
    crate::metrics::inc(
        "telephony_leg_transitions_total",
        &[("status", change.target.as_str())],
    );
    if matches!(change.target, LegStatus::Answered | LegStatus::Connected)
        && leg.answered_at.is_none()
    {
        if let Some(started) = leg.originating_at {
            crate::metrics::observe_ms(
                "telephony_setup_latency_ms",
                (at - started).num_milliseconds(),
            );
        }
    }
    if change.silent {
        return Ok(Some(updated));
    }
    let mut details = json!({ "legStatus": change.target.as_str() });
    if let Some(r) = updated.end_reason.as_deref().filter(|_| terminal) {
        details["endReason"] = json!(r);
    }
    if let Some(c) = updated.failure_category.as_deref().filter(|_| terminal) {
        details["failureCategory"] = json!(c);
    }
    if terminal || change.target == LegStatus::Unknown {
        details["definite"] = json!(change.definite && change.target != LegStatus::Unknown);
    }
    let updated = outbox::emit_leg_event(txn, &updated, change.event, details, at).await?;
    Ok(Some(updated))
}

//! Transactional outbox of normalized Telephony events for Calling (§36, §166).
//!
//! Events are written in the SAME transaction as the state they describe, with a
//! gap-free per-leg sequence. A delivery worker sends them to Calling's authenticated
//! internal API in per-leg order, retrying with capped backoff; Calling deduplicates by
//! event id. While Calling is down, events stay durable and Calling catches up later.
//! An event Calling refuses permanently (an unknown call / leg, a refused body) or one
//! that exhausted [`MAX_DELIVERY_ATTEMPTS`] is DEAD-LETTERED (final, one deduplicated
//! `OUTBOX_DEAD_LETTER` incident per reason) instead of being retried forever. Each row
//! carries the correlation id of the command whose flow produced it, and delivery
//! continues that correlation.
//!
//! The payload is the contract `cfmeet.telephony.event` v1: CF Meet ids, normalized
//! vocabulary and timestamps only — no provider id, provider call id, SIP data, raw
//! provider payload or telephone number.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    domain::NormalizedEvent,
    error::{ErrorCode, TelephonyError, TelephonyResult},
    legs::{ts, LegRow, LEG_COLUMNS},
    ports::PortError,
};

pub const EVENT_SCHEMA_VERSION: &str = "1";

/// Delivery attempts before an event is dead-lettered. With the 60 s backoff cap this
/// keeps an event retrying for about six hours of Calling unavailability.
pub const MAX_DELIVERY_ATTEMPTS: i32 = 360;

/// A leg event row continues the correlation of the command whose flow produced it: the
/// command being executed on the leg right now, else the leg's latest command (an
/// inbound leg no command touched yet has none).
const LEG_OUTBOX_INSERT: &str =
    "INSERT INTO telephony_outbox (id, telephony_leg_id, calling_call_id, \
       event_type, leg_sequence, payload, created_at, next_attempt_at, correlation_id) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $7, \
       (SELECT c.correlation_id FROM telephony_commands c WHERE c.telephony_leg_id = $2 \
         AND c.correlation_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$' \
         ORDER BY (c.status IN ('CLAIMED', 'RUNNING')) DESC, c.created_at DESC LIMIT 1))";

/// Writes a leg event with the next per-leg sequence; returns the updated leg row.
pub async fn emit_leg_event<C: ConnectionTrait>(
    txn: &C,
    leg: &LegRow,
    event: NormalizedEvent,
    details: Value,
    at: DateTime<Utc>,
) -> TelephonyResult<LegRow> {
    let updated: LegRow = db::one(
        txn,
        &format!(
            "UPDATE telephony_legs SET event_sequence = event_sequence + 1 WHERE id = $1 \
             RETURNING {LEG_COLUMNS}"
        ),
        vec![leg.id.into()],
    )
    .await?
    .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
    let id = Uuid::new_v4();
    let mut payload = json!({
        "schemaVersion": EVENT_SCHEMA_VERSION,
        "eventId": id,
        "eventType": event.as_str(),
        "occurredAt": ts(at),
        "telephonyLegId": leg.id,
        "callId": leg.calling_call_id,
        "callLegId": leg.calling_call_leg_id,
        "sequence": updated.event_sequence,
        "direction": leg.direction,
        "routeClass": leg.route_class,
    });
    if let Some(a) = leg.calling_attempt_id {
        payload["attemptId"] = json!(a);
    }
    if let Value::Object(extra) = details {
        for (k, v) in extra {
            payload[k] = v;
        }
    }
    db::exec(
        txn,
        LEG_OUTBOX_INSERT,
        vec![
            id.into(),
            leg.id.into(),
            leg.calling_call_id.into(),
            event.as_str().into(),
            updated.event_sequence.into(),
            payload.into(),
            at.into(),
        ],
    )
    .await?;
    Ok(updated)
}

/// Writes a bridge event (BRIDGE_ESTABLISHED / BRIDGE_ENDED).
pub async fn emit_bridge_event<C: ConnectionTrait>(
    txn: &C,
    bridge_id: Uuid,
    calling_call_id: Uuid,
    event: NormalizedEvent,
    details: Value,
    at: DateTime<Utc>,
) -> TelephonyResult<()> {
    let id = Uuid::new_v4();
    let mut payload = json!({
        "schemaVersion": EVENT_SCHEMA_VERSION,
        "eventId": id,
        "eventType": event.as_str(),
        "occurredAt": ts(at),
        "bridgeId": bridge_id,
        "callId": calling_call_id,
    });
    if let Value::Object(extra) = details {
        for (k, v) in extra {
            payload[k] = v;
        }
    }
    db::exec(
        txn,
        "INSERT INTO telephony_outbox (id, bridge_id, calling_call_id, event_type, payload, created_at, \
           next_attempt_at, correlation_id) VALUES ($1, $2, $3, $4, $5, $6, $6, \
           (SELECT c.correlation_id FROM telephony_commands c WHERE c.bridge_id = $2 \
             AND c.correlation_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$' \
             ORDER BY (c.status IN ('CLAIMED', 'RUNNING')) DESC, c.created_at DESC LIMIT 1))",
        vec![
            id.into(),
            bridge_id.into(),
            calling_call_id.into(),
            event.as_str().into(),
            payload.into(),
            at.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Writes a non-status event (USAGE_FINALIZED / RECONCILIATION_RESULT) for a leg.
pub async fn emit_leg_fact<C: ConnectionTrait>(
    txn: &C,
    leg: &LegRow,
    event_type: &'static str,
    details: Value,
    at: DateTime<Utc>,
) -> TelephonyResult<()> {
    let updated: LegRow = db::one(
        txn,
        &format!(
            "UPDATE telephony_legs SET event_sequence = event_sequence + 1 WHERE id = $1 \
             RETURNING {LEG_COLUMNS}"
        ),
        vec![leg.id.into()],
    )
    .await?
    .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
    let id = Uuid::new_v4();
    let mut payload = json!({
        "schemaVersion": EVENT_SCHEMA_VERSION,
        "eventId": id,
        "eventType": event_type,
        "occurredAt": ts(at),
        "telephonyLegId": leg.id,
        "callId": leg.calling_call_id,
        "callLegId": leg.calling_call_leg_id,
        "sequence": updated.event_sequence,
        "direction": leg.direction,
        "routeClass": leg.route_class,
        "legStatus": updated.status,
    });
    if let Some(a) = leg.calling_attempt_id {
        payload["attemptId"] = json!(a);
    }
    if let Value::Object(extra) = details {
        for (k, v) in extra {
            payload[k] = v;
        }
    }
    db::exec(
        txn,
        LEG_OUTBOX_INSERT,
        vec![
            id.into(),
            leg.id.into(),
            leg.calling_call_id.into(),
            event_type.into(),
            updated.event_sequence.into(),
            payload.into(),
            at.into(),
        ],
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct OutboxRow {
    pub id: Uuid,
    pub telephony_leg_id: Option<Uuid>,
    pub event_type: String,
    pub payload: Value,
    pub attempts: i32,
    pub correlation_id: Option<String>,
}

/// Claims due events: per leg only the lowest undelivered sequence (ordering), leases
/// them for 30 s, multi-instance safe (`SKIP LOCKED`). A dead-lettered event is final:
/// it is never claimed again and no longer holds back the leg's later events (Calling
/// orders by sequence and tolerates gaps). A worker that dies holding a lease loses it
/// when the lease expires; the event is then claimed again.
pub async fn claim_due<C: ConnectionTrait>(
    db_: &C,
    now: DateTime<Utc>,
    limit: u64,
) -> TelephonyResult<Vec<OutboxRow>> {
    db::all(
        db_,
        "UPDATE telephony_outbox SET lease_until = $1 + interval '30 seconds' WHERE id IN ( \
           SELECT o.id FROM telephony_outbox o \
           WHERE o.delivered_at IS NULL AND o.dead_lettered_at IS NULL AND o.next_attempt_at <= $1 \
             AND (o.lease_until IS NULL OR o.lease_until < $1) \
             AND NOT EXISTS (SELECT 1 FROM telephony_outbox e WHERE e.telephony_leg_id = o.telephony_leg_id \
                             AND e.delivered_at IS NULL AND e.dead_lettered_at IS NULL \
                             AND e.leg_sequence < o.leg_sequence) \
           ORDER BY o.created_at, o.leg_sequence LIMIT $2 FOR UPDATE SKIP LOCKED) \
         RETURNING id, telephony_leg_id, event_type, payload, attempts, correlation_id",
        vec![now.into(), (limit.clamp(1, 500) as i64).into()],
    )
    .await
}

pub async fn mark_delivered<C: ConnectionTrait>(
    db_: &C,
    id: Uuid,
    at: DateTime<Utc>,
) -> TelephonyResult<()> {
    db::exec(
        db_,
        "UPDATE telephony_outbox SET delivered_at = $2, lease_until = NULL, attempts = attempts + 1 \
         WHERE id = $1 AND delivered_at IS NULL AND dead_lettered_at IS NULL",
        vec![id.into(), at.into()],
    )
    .await
    .map(|_| ())
}

/// How a failed delivery is treated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryFailure {
    /// Calling could not be reached or asked to come back: retried with capped backoff.
    Transient(String),
    /// Calling answered that the event can never apply (an unknown call / leg, a refused
    /// body): retrying cannot help.
    Permanent(String),
}

impl DeliveryFailure {
    /// Classifies a port failure. A refusal of the CALLER (authentication / permission,
    /// typically a trust-bundle rollout), a rate limit or an optimistic-concurrency
    /// conflict is transient; any other refusal and NOT_FOUND are permanent.
    pub fn classify(error: &PortError) -> Self {
        match error {
            PortError::Unavailable => DeliveryFailure::Transient("UNAVAILABLE".into()),
            PortError::NotFound => DeliveryFailure::Permanent("NOT_FOUND".into()),
            PortError::Denied(code) => {
                let code = bounded_code(code);
                let transient = code.starts_with("INTERNAL_AUTHENTICATION")
                    || code.starts_with("INTERNAL_PERMISSION")
                    || code == "RATE_LIMITED"
                    || code.ends_with("CONFLICT");
                if transient {
                    DeliveryFailure::Transient(code)
                } else {
                    DeliveryFailure::Permanent(code)
                }
            }
        }
    }

    pub fn code(&self) -> &str {
        match self {
            DeliveryFailure::Transient(c) | DeliveryFailure::Permanent(c) => c,
        }
    }
}

/// A refusal code as stored / logged: an UPPER_SNAKE token of at most 64 bytes.
fn bounded_code(code: &str) -> String {
    let ok = !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        code.to_string()
    } else {
        "DENIED".to_string()
    }
}

/// Why an event was dead-lettered (bounded: incident key, metric label).
pub fn dead_letter_reason(failure: &DeliveryFailure) -> &'static str {
    match failure {
        DeliveryFailure::Permanent(c) if c == "NOT_FOUND" => "CALLING_NOT_FOUND",
        DeliveryFailure::Permanent(_) => "CALLING_REFUSED",
        DeliveryFailure::Transient(_) => "ATTEMPTS_EXHAUSTED",
    }
}

/// Reschedules a transiently failed event (capped exponential backoff).
pub async fn mark_failed<C: ConnectionTrait>(
    db_: &C,
    row: &OutboxRow,
    code: &str,
    now: DateTime<Utc>,
) -> TelephonyResult<()> {
    let backoff = Duration::milliseconds((250_i64 << row.attempts.clamp(0, 8)).min(60_000));
    db::exec(
        db_,
        "UPDATE telephony_outbox SET attempts = attempts + 1, lease_until = NULL, next_attempt_at = $2, \
           last_error_code = $3 WHERE id = $1 AND delivered_at IS NULL AND dead_lettered_at IS NULL",
        vec![row.id.into(), (now + backoff).into(), code.into()],
    )
    .await
    .map(|_| ())
}

/// Dead-letters an event (final; set once). Returns whether this call did it.
pub async fn mark_dead_letter<C: ConnectionTrait>(
    db_: &C,
    row: &OutboxRow,
    code: &str,
    now: DateTime<Utc>,
) -> TelephonyResult<bool> {
    db::exec(
        db_,
        "UPDATE telephony_outbox SET attempts = attempts + 1, lease_until = NULL, dead_lettered_at = $2, \
           last_error_code = $3 WHERE id = $1 AND delivered_at IS NULL AND dead_lettered_at IS NULL",
        vec![row.id.into(), now.into(), code.into()],
    )
    .await
    .map(|n| n > 0)
}

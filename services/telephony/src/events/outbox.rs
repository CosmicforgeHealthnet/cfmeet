//! Transactional outbox of normalized Telephony events for Calling (§36, §166).
//!
//! Events are written in the SAME transaction as the state they describe, with a
//! gap-free per-leg sequence. A delivery worker sends them to Calling's authenticated
//! internal API in per-leg order, retrying with backoff; Calling deduplicates by event
//! id. While Calling is down, events stay durable and Calling catches up later.
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
};

pub const EVENT_SCHEMA_VERSION: &str = "1";

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
        "INSERT INTO telephony_outbox (id, telephony_leg_id, calling_call_id, event_type, leg_sequence, \
           payload, created_at, next_attempt_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
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
           next_attempt_at) VALUES ($1, $2, $3, $4, $5, $6, $6)",
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
        "INSERT INTO telephony_outbox (id, telephony_leg_id, calling_call_id, event_type, leg_sequence, \
           payload, created_at, next_attempt_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
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
    pub payload: Value,
    pub attempts: i32,
}

/// Claims due events: per leg only the lowest undelivered sequence (ordering), leases
/// them for 30 s, multi-instance safe (`SKIP LOCKED`).
pub async fn claim_due<C: ConnectionTrait>(
    db_: &C,
    now: DateTime<Utc>,
    limit: u64,
) -> TelephonyResult<Vec<OutboxRow>> {
    db::all(
        db_,
        "UPDATE telephony_outbox SET lease_until = $1 + interval '30 seconds' WHERE id IN ( \
           SELECT o.id FROM telephony_outbox o \
           WHERE o.delivered_at IS NULL AND o.next_attempt_at <= $1 \
             AND (o.lease_until IS NULL OR o.lease_until < $1) \
             AND NOT EXISTS (SELECT 1 FROM telephony_outbox e WHERE e.telephony_leg_id = o.telephony_leg_id \
                             AND e.delivered_at IS NULL AND e.leg_sequence < o.leg_sequence) \
           ORDER BY o.created_at, o.leg_sequence LIMIT $2 FOR UPDATE SKIP LOCKED) \
         RETURNING id, telephony_leg_id, payload, attempts",
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
         WHERE id = $1 AND delivered_at IS NULL",
        vec![id.into(), at.into()],
    )
    .await
    .map(|_| ())
}

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
           last_error_code = $3 WHERE id = $1 AND delivered_at IS NULL",
        vec![row.id.into(), (now + backoff).into(), code.into()],
    )
    .await
    .map(|_| ())
}

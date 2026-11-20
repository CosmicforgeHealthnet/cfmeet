//! The HQ event feed (design §11, phase A7), for the HQ worker's feed identity
//! (`cfmeet.source_sync`, purpose `event-sync`).
//!
//! * `hq.events.feed` (`GET /hq/events`, SCOPED_WINDOW): the append-only feed of owner
//!   events, in increasing sequence, appended in `[start, end)`. `cursor` is the last
//!   sequence seen; `next_cursor` is the last sequence of this page when more follow.
//! * `hq.events.ack` (`POST /hq/event-acknowledgements`, SYNC, not a command): body
//!   `{consumer, sequence}`; one acknowledged sequence per consumer that never moves
//!   backwards (a lower value answers the stored one). A sequence beyond the last appended
//!   one is refused (`COMMAND_BODY_INVALID`, nothing changed).

use chrono::SecondsFormat;
use serde_json::{json, Value};

use crate::chain::{Answer, Authorized, Bridge};
use crate::errors::ErrorCode;
use crate::store::AckOutcome;

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    Some(match auth.capability.id {
        "hq.events.feed" => feed(bridge, auth).await,
        "hq.events.ack" => ack(bridge, auth).await,
        _ => return None,
    })
}

fn sequence_cursor(c: &str) -> Option<i64> {
    (c.len() <= 19 && c.bytes().all(|b| b.is_ascii_digit()))
        .then(|| c.parse::<i64>().ok())
        .flatten()
}

async fn feed(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let q = &auth.query;
    let (start, end) = (
        q.start.ok_or(ErrorCode::QueryInvalid)?,
        q.end.ok_or(ErrorCode::QueryInvalid)?,
    );
    let after = match &q.cursor {
        None => None,
        Some(c) => Some(sequence_cursor(c).ok_or(ErrorCode::QueryInvalid)?),
    };
    let limit = q.limit.unwrap_or(100);
    let rows = bridge
        .store
        .event_feed(start, end, after, limit + 1)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?;
    let more = rows.len() > limit as usize;
    let page = &rows[..rows.len().min(limit as usize)];
    let fmt = |t: chrono::DateTime<chrono::Utc>| t.to_rfc3339_opts(SecondsFormat::Secs, true);
    let items: Vec<Value> = page
        .iter()
        .map(|f| {
            json!({
                "sequence": f.sequence,
                "event_id": f.event.event_id.to_string(),
                "type": f.event.event_type,
                "producer": f.event.producer,
                "subject": {"kind": f.event.subject_kind, "id": f.event.subject_id},
                "state": f.event.state,
                "version": f.event.version,
                "occurred_at": fmt(f.event.occurred_at),
                "appended_at": fmt(f.appended_at),
            })
        })
        .collect();
    let next = more
        .then(|| page.last().map(|f| f.sequence.to_string()))
        .flatten();
    Ok(Answer {
        result: json!({"items": items, "next_cursor": next}),
        freshness: "LIVE",
    })
}

fn consumer_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.as_bytes()[0].is_ascii_lowercase()
        && v.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

async fn ack(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let bad = ErrorCode::CommandBodyInvalid;
    let v: Value = serde_json::from_slice(&auth.body).map_err(|_| bad)?;
    let o = v.as_object().ok_or(bad)?;
    if o.len() != 2 {
        return Err(bad);
    }
    let consumer = o
        .get("consumer")
        .and_then(Value::as_str)
        .filter(|c| consumer_ok(c))
        .ok_or(bad)?;
    let sequence = o
        .get("sequence")
        .and_then(Value::as_i64)
        .filter(|s| *s >= 0)
        .ok_or(bad)?;
    match bridge
        .store
        .acknowledge_events(consumer, sequence, auth.now)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?
    {
        AckOutcome::Acknowledged(stored) => Ok(Answer {
            result: json!({"consumer": consumer, "acknowledged_sequence": stored}),
            freshness: "LIVE",
        }),
        AckOutcome::BeyondHead => Err(bad),
    }
}

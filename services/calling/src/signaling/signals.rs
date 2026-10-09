//! Realtime signals (contract `cfmeet.calling.realtime-signal` v1) and the push-ready
//! invitation payload (contract `cfmeet.calling.push-invitation` v1).
//!
//! A signal is a low-latency notification DERIVED from committed state; the durable
//! evidence is the CallEvent / outbox written in the same transaction as the change. A
//! signal carries an id, the Call id, kind, safe state, version and event sequence, the
//! issue time, the correlation id, the invitation (id, status, server expiry) where it
//! concerns one, and a small bounded payload — never telephone numbers, provider ids,
//! credentials or tokens. The delivering instance stamps the receiving endpoint and
//! principal as `target`.

use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngCore;
use serde_json::{json, Map, Value};

use crate::models::{call_invitations, calls};

pub const SIGNAL_SCHEMA: &str = "cfmeet.calling.realtime-signal";
pub const PUSH_SCHEMA: &str = "cfmeet.calling.push-invitation";

/// Semantic signal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalType {
    Incoming,
    Ringing,
    RingCancelled,
    Accepted,
    Declined,
    Cancelled,
    Answered,
    Connected,
    Held,
    Resumed,
    TransferStarted,
    Transferred,
    TransferFailed,
    Ended,
    Failed,
    NoAnswer,
    Busy,
    AnsweredElsewhere,
    InvitationExpired,
    /// An enterprise Call joined a queue (to the caller; data: queueId, position).
    Queued,
    /// The caller's authoritative position in its queue changed.
    QueuePosition,
}

impl SignalType {
    pub const ALL: &'static [SignalType] = &[
        SignalType::Incoming,
        SignalType::Ringing,
        SignalType::RingCancelled,
        SignalType::Accepted,
        SignalType::Declined,
        SignalType::Cancelled,
        SignalType::Answered,
        SignalType::Connected,
        SignalType::Held,
        SignalType::Resumed,
        SignalType::TransferStarted,
        SignalType::Transferred,
        SignalType::TransferFailed,
        SignalType::Ended,
        SignalType::Failed,
        SignalType::NoAnswer,
        SignalType::Busy,
        SignalType::AnsweredElsewhere,
        SignalType::InvitationExpired,
        SignalType::Queued,
        SignalType::QueuePosition,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            SignalType::Incoming => "call.incoming",
            SignalType::Ringing => "call.ringing",
            SignalType::RingCancelled => "call.ring_cancelled",
            SignalType::Accepted => "call.accepted",
            SignalType::Declined => "call.declined",
            SignalType::Cancelled => "call.cancelled",
            SignalType::Answered => "call.answered",
            SignalType::Connected => "call.connected",
            SignalType::Held => "call.held",
            SignalType::Resumed => "call.resumed",
            SignalType::TransferStarted => "call.transfer_started",
            SignalType::Transferred => "call.transferred",
            SignalType::TransferFailed => "call.transfer_failed",
            SignalType::Ended => "call.ended",
            SignalType::Failed => "call.failed",
            SignalType::NoAnswer => "call.no_answer",
            SignalType::Busy => "call.busy",
            SignalType::AnsweredElsewhere => "call.answered_elsewhere",
            SignalType::InvitationExpired => "call.invitation_expired",
            SignalType::Queued => "call.queued",
            SignalType::QueuePosition => "call.queue_position",
        }
    }
}

fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn signal_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("sig_{}", hex::encode(bytes))
}

/// Builds a signal from committed state. `sequence` is the Call's latest event sequence.
pub fn build(
    kind: SignalType,
    call: &calls::Model,
    sequence: i64,
    correlation_id: &str,
    invitation: Option<&call_invitations::Model>,
    data: Map<String, Value>,
    issued_at: DateTime<Utc>,
) -> Value {
    let mut signal = json!({
        "schema": SIGNAL_SCHEMA,
        "schemaVersion": 1,
        "eventId": signal_id(),
        "type": kind.as_str(),
        "callId": call.id,
        "callKind": call.call_kind.as_str(),
        "callState": call.current_state.as_str(),
        "callVersion": call.version,
        "sequence": sequence,
        "issuedAt": time(issued_at),
        "correlationId": correlation_id,
        "data": Value::Object(data),
    });
    if let Some(invitation) = invitation {
        signal["invitation"] = json!({
            "invitationId": invitation.id,
            "participantId": invitation.participant_id,
            "status": invitation.status.as_str(),
            "purpose": invitation.purpose.as_str(),
            "issuedAt": time(invitation.issued_at),
            "expiresAt": time(invitation.expires_at),
        });
    }
    signal
}

/// Stamps the receiving endpoint / principal on a delivered signal.
pub fn for_target(mut signal: Value, endpoint_id: uuid::Uuid, principal_id: uuid::Uuid) -> Value {
    signal["target"] = json!({ "endpointId": endpoint_id, "principalId": principal_id });
    signal
}

/// Transport-neutral push payload of a ring offer: what a later push service delivers to
/// the endpoint's `push_target_ref`. No provider token, media credential, API
/// credential or telephone number — the device acts on it with its own signaling
/// credential.
pub fn push_payload(
    call: &calls::Model,
    invitation: &call_invitations::Model,
    caller_display_name: Option<&str>,
) -> Value {
    let mut caller = Map::new();
    if let Some(name) = caller_display_name {
        caller.insert("displayName".into(), Value::String(name.to_string()));
    }
    json!({
        "schema": PUSH_SCHEMA,
        "schemaVersion": 1,
        "callId": call.id,
        "invitationId": invitation.id,
        "callKind": call.call_kind.as_str(),
        "caller": Value::Object(caller),
        "issuedAt": time(invitation.issued_at),
        "expiresAt": time(invitation.expires_at),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_types_are_unique_and_namespaced() {
        let mut names: Vec<_> = SignalType::ALL.iter().map(SignalType::as_str).collect();
        assert!(names.iter().all(|n| n.starts_with("call.")));
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SignalType::ALL.len());
    }
}

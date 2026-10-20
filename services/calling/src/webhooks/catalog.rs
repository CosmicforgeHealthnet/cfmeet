//! The PUBLIC event catalog (Part G §67 / §68) and the projection of internal envelopes
//! into public webhook events.
//!
//! Internal envelopes (`calling.call-state-changed`, `privacy-relay.session.revoked`, …)
//! are never delivered verbatim: each maps to at most one public type (`call.ended`,
//! `relay.revoked`, …) whose `data` is rebuilt from an explicit allow-list of keys, and
//! every value must be an opaque UUID, a bounded upper-case code or a whitelisted
//! integer — so no telephone number, URI, provider identifier, credential or free text can
//! reach a customer endpoint, whatever a producer put in its internal payload.

use serde_json::{Map, Value};

use crate::platform::scopes::names;

/// One public event type and the scope a subscriber's credential must hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventTypeDef {
    pub name: &'static str,
    pub scope: &'static str,
}

macro_rules! catalog {
    ($($name:literal => $scope:expr),* $(,)?) => {
        pub const CATALOG: &[EventTypeDef] = &[$(EventTypeDef { name: $name, scope: $scope }),*];
    };
}

catalog! {
    "call.requested" => names::CALLS_READ,
    "call.incoming" => names::CALLS_READ,
    "call.ringing" => names::CALLS_READ,
    "call.ring_cancelled" => names::CALLS_READ,
    "call.accepted" => names::CALLS_READ,
    "call.declined" => names::CALLS_READ,
    "call.cancelled" => names::CALLS_READ,
    "call.answered" => names::CALLS_READ,
    "call.connected" => names::CALLS_READ,
    "call.route_changed" => names::CALLS_READ,
    "call.held" => names::CALLS_READ,
    "call.resumed" => names::CALLS_READ,
    "call.transfer_started" => names::CALLS_READ,
    "call.transferred" => names::CALLS_READ,
    "call.transfer_failed" => names::CALLS_READ,
    "call.ended" => names::CALLS_READ,
    "call.failed" => names::CALLS_READ,
    "call.no_answer" => names::CALLS_READ,
    "call.busy" => names::CALLS_READ,
    "call.answered_elsewhere" => names::CALLS_READ,
    "call.invitation_expired" => names::CALLS_READ,
    "voicemail.received" => names::ENTERPRISE_VOICEMAIL_READ,
    "voicemail.ready" => names::ENTERPRISE_VOICEMAIL_READ,
    "voicemail.deleted" => names::ENTERPRISE_VOICEMAIL_READ,
    "recording.ready" => names::ENTERPRISE_RECORDING_READ,
    "recording.deleted" => names::ENTERPRISE_RECORDING_READ,
    "relay.created" => names::PRIVACY_SESSIONS_READ,
    "relay.updated" => names::PRIVACY_SESSIONS_READ,
    "relay.revoked" => names::PRIVACY_SESSIONS_READ,
    "relay.expired" => names::PRIVACY_SESSIONS_READ,
    "relay.participant_changed" => names::PRIVACY_SESSIONS_READ,
    "usage.recorded" => names::USAGE_READ,
    "limit.warning" => names::USAGE_READ,
    "limit.exceeded" => names::USAGE_READ,
    "sms.received" => names::SMS_READ,
    "sms.requested" => names::SMS_READ,
    "sms.submitted" => names::SMS_READ,
    "sms.delivered" => names::SMS_READ,
    "sms.failed" => names::SMS_READ,
    "sms.unknown" => names::SMS_READ,
    "sms.blocked" => names::SMS_READ,
    "conversation.created" => names::MESSAGES_READ,
    "conversation.updated" => names::MESSAGES_READ,
    "conversation.member_added" => names::MESSAGES_READ,
    "conversation.member_removed" => names::MESSAGES_READ,
    "message.created" => names::MESSAGES_READ,
    "message.deleted" => names::MESSAGES_READ,
    "message.moderated" => names::MESSAGES_READ,
    "message.expired" => names::MESSAGES_READ,
}

pub fn definition(name: &str) -> Option<&'static EventTypeDef> {
    CATALOG.iter().find(|d| d.name == name)
}

/// A projected public event.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicEvent {
    pub event_type: &'static str,
    pub data: Map<String, Value>,
}

/// Keys that may cross into public `data` (internal snake_case → public camelCase) and
/// whether an integer value is acceptable for them.
const ALLOWED_KEYS: &[(&str, &str, bool)] = &[
    ("call_id", "callId", false),
    ("call_version", "callVersion", true),
    ("participant_id", "participantId", false),
    ("invitation_id", "invitationId", false),
    ("endpoint_id", "endpointId", false),
    ("transfer_id", "transferId", false),
    ("route_id", "routeId", false),
    ("to_route_state", "routeState", false),
    ("failure_category", "failureCategory", false),
    ("failure_reason", "failureReason", false),
    ("reason", "reason", false),
    ("message_id", "messageId", false),
    ("mailbox_id", "mailboxId", false),
    ("recording_id", "recordingId", false),
    ("duration_ms", "durationMs", true),
    ("usage_record_id", "usageRecordId", false),
    ("usage_type", "usageType", false),
    ("quantity", "quantity", true),
    ("unit", "unit", false),
    ("context_id", "contextId", false),
    ("context_version", "contextVersion", true),
    ("context_status", "contextStatus", false),
    ("session_id", "sessionId", false),
    ("session_version", "sessionVersion", true),
    ("session_status", "sessionStatus", false),
    ("limit", "limit", false),
    ("scope", "scope", false),
    ("window", "window", false),
    ("threshold", "threshold", true),
    ("current", "current", true),
    ("ceiling", "ceiling", true),
];

fn is_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// The only values that may leave: an opaque UUID, a bounded code, or (for numeric keys) a
/// non-negative integer.
fn safe_value(v: &Value, numeric: bool) -> Option<Value> {
    match v {
        Value::String(s) if uuid::Uuid::parse_str(s).is_ok() && s.len() == 36 => {
            Some(Value::String(s.to_ascii_lowercase()))
        }
        Value::String(s) if is_code(s) => Some(v.clone()),
        Value::Number(n) if numeric && n.as_u64().is_some() => Some(v.clone()),
        _ => None,
    }
}

/// Copies the allow-listed, safe fields of `sources` (later sources win) into `out`.
pub fn safe_fields(sources: &[&Value], out: &mut Map<String, Value>) {
    for source in sources {
        let Some(obj) = source.as_object() else {
            continue;
        };
        for (internal, public, numeric) in ALLOWED_KEYS {
            if let Some(v) = obj.get(*internal).and_then(|v| safe_value(v, *numeric)) {
                out.insert((*public).to_string(), v);
            }
        }
    }
}

/// Projects a Calling outbox envelope (`cfmeet.calling.call-lifecycle-event` payload).
pub fn project_calling(envelope: &Value) -> Option<PublicEvent> {
    let payload = envelope.get("payload")?;
    let internal = payload.get("event_type")?.as_str()?;
    let to = payload.get("to_state").and_then(Value::as_str);
    let from = payload.get("from_state").and_then(Value::as_str);
    let event_type = match internal {
        "CALL_REQUESTED" => "call.requested",
        "CALL_STATE_CHANGED" => match (from, to?) {
            (_, "RINGING") => "call.ringing",
            (_, "ANSWERED") => "call.answered",
            (Some("HELD"), "CONNECTED") => "call.resumed",
            (_, "CONNECTED") => "call.connected",
            (_, "HELD") => "call.held",
            (_, "ENDED") => "call.ended",
            (_, "FAILED") | (_, "DENIED") => "call.failed",
            (_, "NO_ANSWER") => "call.no_answer",
            (_, "BUSY") => "call.busy",
            (_, "CANCELLED") => "call.cancelled",
            (_, "DECLINED") => "call.declined",
            _ => return None,
        },
        "CALL_INVITATION_ISSUED" => "call.incoming",
        "CALL_INVITATION_ACCEPTED" => "call.accepted",
        "CALL_INVITATION_CANCELLED" => "call.ring_cancelled",
        "CALL_INVITATION_ANSWERED_ELSEWHERE" => "call.answered_elsewhere",
        "CALL_INVITATION_EXPIRED" => "call.invitation_expired",
        "CALL_ROUTE_STATE_CHANGED" => "call.route_changed",
        "CALL_TRANSFER_REQUESTED" => "call.transfer_started",
        "CALL_TRANSFER_COMPLETED" => "call.transferred",
        "CALL_TRANSFER_FAILED" => "call.transfer_failed",
        "VOICEMAIL_RECEIVED" => "voicemail.received",
        "VOICEMAIL_READY" => "voicemail.ready",
        "VOICEMAIL_DELETED" => "voicemail.deleted",
        "RECORDING_READY" => "recording.ready",
        "RECORDING_DELETED" => "recording.deleted",
        "CALL_USAGE_FINALIZED" => "usage.recorded",
        _ => return None,
    };
    let mut data = Map::new();
    safe_fields(
        &[payload, payload.get("data").unwrap_or(&Value::Null)],
        &mut data,
    );
    if let Some(state) = to.filter(|s| is_code(s)) {
        data.insert("state".into(), Value::String(state.to_string()));
    }
    Some(PublicEvent { event_type, data })
}

/// Projects a Privacy Relay outbox envelope (`privacy-relay.<aggregate>.<fact>`).
pub fn project_privacy_relay(envelope: &Value) -> Option<PublicEvent> {
    let internal = envelope.get("event_type")?.as_str()?;
    let payload = envelope.get("payload")?;
    let (event_type, resource) = match internal {
        "privacy-relay.session.created" => ("relay.created", "SESSION"),
        "privacy-relay.session.updated"
        | "privacy-relay.session.suspended"
        | "privacy-relay.session.resumed"
        | "privacy-relay.session.completed" => ("relay.updated", "SESSION"),
        "privacy-relay.session.revoked" => ("relay.revoked", "SESSION"),
        "privacy-relay.session.expired" => ("relay.expired", "SESSION"),
        "privacy-relay.context.created" => ("relay.created", "CONTEXT"),
        "privacy-relay.context.updated"
        | "privacy-relay.context.suspended"
        | "privacy-relay.context.resumed"
        | "privacy-relay.context.completed" => ("relay.updated", "CONTEXT"),
        "privacy-relay.context.revoked" => ("relay.revoked", "CONTEXT"),
        "privacy-relay.context.expired" => ("relay.expired", "CONTEXT"),
        "privacy-relay.context.participants-changed" => ("relay.participant_changed", "CONTEXT"),
        // Enforcement and relay-number facts are internal (never public).
        _ => return None,
    };
    let mut data = Map::new();
    safe_fields(&[payload], &mut data);
    data.insert("resource".into(), Value::String(resource.into()));
    Some(PublicEvent { event_type, data })
}

/// SMS message keys that may cross (internal snake_case → public camelCase; numeric?).
const SMS_KEYS: &[(&str, &str, bool)] = &[
    ("message_id", "messageId", false),
    ("thread_id", "threadId", false),
    ("number_resource_id", "numberResourceId", false),
    ("direction", "direction", false),
    ("status", "status", false),
    ("segment_count", "segmentCount", true),
    ("encoding", "encoding", false),
    ("principal_id", "principalId", false),
    ("target_kind", "targetKind", false),
    ("target_ref", "targetRef", false),
    ("failure_code", "failureCode", false),
    ("blocked_reason", "blockedReason", false),
];

/// Projects an SMS Gateway envelope (`sms-gateway.message.<fact>` → `sms.<fact>`). The
/// public data are references, status and bounded codes only: never content, a number
/// (not even masked), a provider reference or a grant.
pub fn project_sms_gateway(envelope: &Value) -> Option<PublicEvent> {
    let internal = envelope.get("event_type")?.as_str()?;
    let payload = envelope.get("payload")?.as_object()?;
    let event_type = match internal {
        "sms-gateway.message.received" => "sms.received",
        "sms-gateway.message.requested" => "sms.requested",
        "sms-gateway.message.submitted" => "sms.submitted",
        "sms-gateway.message.delivered" => "sms.delivered",
        "sms-gateway.message.failed" => "sms.failed",
        "sms-gateway.message.unknown" => "sms.unknown",
        "sms-gateway.message.blocked" => "sms.blocked",
        _ => return None,
    };
    let mut data = Map::new();
    for (internal, public, numeric) in SMS_KEYS {
        if let Some(v) = payload.get(*internal).and_then(|v| safe_value(v, *numeric)) {
            data.insert((*public).to_string(), v);
        }
    }
    data.get("messageId")?;
    Some(PublicEvent { event_type, data })
}

/// Messaging keys that may cross (internal snake_case → public camelCase; numeric?).
/// Deliberately absent: text, ciphertext, attachment keys, device keys, MLS data, policy
/// digests, display names, telephone numbers, free-text reasons.
const MESSAGING_KEYS: &[(&str, &str, bool)] = &[
    ("conversation_id", "conversationId", false),
    ("conversation_kind", "conversationKind", false),
    ("security_profile", "securityProfile", false),
    ("lifecycle_state", "lifecycleState", false),
    ("previous_state", "previousState", false),
    ("version", "conversationVersion", true),
    ("message_id", "messageId", false),
    ("sequence", "sequence", true),
    ("message_kind", "messageKind", false),
    ("sender_principal_id", "senderPrincipalId", false),
    ("security_provenance", "securityProvenance", false),
    ("principal_id", "principalId", false),
    ("role", "role", false),
    ("reason", "reason", false),
    ("channel", "channel", false),
];

/// Projects a Messaging envelope (`messaging.<aggregate>.<fact>`). Only application
/// messages are public (`message.created` is never emitted for control, MLS, edit or
/// reaction messages); the data are ids, sequences and bounded codes — whatever the
/// security profile, no content can cross (Messaging never puts any in an event, and the
/// allow-list would drop it regardless).
pub fn project_messaging(envelope: &Value) -> Option<PublicEvent> {
    let internal = envelope.get("event_type")?.as_str()?;
    let payload = envelope.get("payload")?.as_object()?;
    let event_type = match internal {
        "messaging.conversation.created" => "conversation.created",
        "messaging.conversation.lifecycle-changed" => "conversation.updated",
        "messaging.member.added" => "conversation.member_added",
        "messaging.member.removed" => "conversation.member_removed",
        "messaging.message.created"
            if payload.get("message_kind").and_then(Value::as_str) == Some("APPLICATION") =>
        {
            "message.created"
        }
        "messaging.message.deleted" => "message.deleted",
        "messaging.message.moderated" => "message.moderated",
        "messaging.message.expired" => "message.expired",
        _ => return None,
    };
    let mut data = Map::new();
    for (internal, public, numeric) in MESSAGING_KEYS {
        if let Some(v) = payload.get(*internal).and_then(|v| safe_value(v, *numeric)) {
            data.insert((*public).to_string(), v);
        }
    }
    data.get("conversationId")?;
    Some(PublicEvent { event_type, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messaging_events_carry_ids_sequences_and_codes_only() {
        let c = "11111111-1111-4111-8111-111111111111";
        let env = json!({ "event_type": "messaging.message.created", "payload": {
            "conversation_id": c, "message_id": "22222222-2222-4222-8222-222222222222",
            "sequence": 7, "message_kind": "APPLICATION", "security_provenance": "NATIVE",
            "sender_principal_id": "33333333-3333-4333-8333-333333333333",
            "sender_device_id": "44444444-4444-4444-8444-444444444444", "notify": true,
            "text": "the launch code is 1234", "ciphertext": "AAEC", "epoch_secret": "c2Vj",
            "policy_digest": "ab12", "display_name": "Alice", "phone": "+15550100123" } });
        let e = project_messaging(&env).unwrap();
        assert_eq!(e.event_type, "message.created");
        assert_eq!(
            Value::Object(e.data),
            json!({ "conversationId": c, "messageId": "22222222-2222-4222-8222-222222222222",
                    "sequence": 7, "messageKind": "APPLICATION", "securityProvenance": "NATIVE",
                    "senderPrincipalId": "33333333-3333-4333-8333-333333333333" })
        );
        // Control / MLS / edit messages and internal facts are never public.
        for (t, kind) in [
            ("messaging.message.created", "COMMIT"),
            ("messaging.message.created", "REACTION"),
            ("messaging.mls.welcome-available", "APPLICATION"),
            ("messaging.device.registered", "APPLICATION"),
            ("messaging.reaction.added", "APPLICATION"),
        ] {
            let mut other = env.clone();
            other["event_type"] = json!(t);
            other["payload"]["message_kind"] = json!(kind);
            assert!(project_messaging(&other).is_none(), "{t} {kind}");
        }
        // A free-text reason never crosses (only bounded codes).
        let removed = json!({ "event_type": "messaging.member.removed", "payload": {
            "conversation_id": c, "principal_id": "33333333-3333-4333-8333-333333333333",
            "reason": "kicked for being rude", "mls_rekey_required": true } });
        let e = project_messaging(&removed).unwrap();
        assert_eq!(e.event_type, "conversation.member_removed");
        assert!(e.data.get("reason").is_none());
    }

    #[test]
    fn sms_events_carry_references_and_codes_only() {
        let env = json!({ "event_type": "sms-gateway.message.received", "payload": {
            "message_id": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "direction": "INBOUND",
            "status": "RECEIVED", "segment_count": 2, "encoding": "GSM7",
            "thread_id": "11111111-1111-4111-8111-111111111111",
            "number_resource_id": "22222222-2222-4222-8222-222222222222",
            "remote_masked": "+***0123", "content": "hello", "from": "+15550100123",
            "provider_message_ref": "dsms-1", "target_kind": "PRINCIPAL",
            "target_ref": "33333333-3333-4333-8333-333333333333" } });
        let e = project_sms_gateway(&env).unwrap();
        assert_eq!(e.event_type, "sms.received");
        assert_eq!(
            Value::Object(e.data),
            json!({ "messageId": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "direction": "INBOUND",
                    "status": "RECEIVED", "segmentCount": 2, "encoding": "GSM7",
                    "threadId": "11111111-1111-4111-8111-111111111111",
                    "numberResourceId": "22222222-2222-4222-8222-222222222222",
                    "targetKind": "PRINCIPAL", "targetRef": "33333333-3333-4333-8333-333333333333" })
        );
        for (internal, public) in [
            ("requested", "sms.requested"),
            ("submitted", "sms.submitted"),
            ("delivered", "sms.delivered"),
            ("failed", "sms.failed"),
            ("unknown", "sms.unknown"),
            ("blocked", "sms.blocked"),
        ] {
            let mut e = env.clone();
            e["event_type"] = json!(format!("sms-gateway.message.{internal}"));
            assert_eq!(project_sms_gateway(&e).unwrap().event_type, public);
            assert!(definition(public).is_some());
        }
        let mut other = env.clone();
        other["event_type"] = json!("sms-gateway.message.internal-fact");
        assert!(project_sms_gateway(&other).is_none());
    }

    #[test]
    fn catalog_names_are_unique_and_scoped() {
        for (i, d) in CATALOG.iter().enumerate() {
            assert!(
                CATALOG[i + 1..].iter().all(|o| o.name != d.name),
                "{}",
                d.name
            );
            assert!(d.name.contains('.'));
        }
        assert!(definition("call.ended").is_some());
        assert!(definition("calling.call-state-changed").is_none());
        assert!(definition("*").is_none());
    }

    #[test]
    fn state_changes_map_to_public_types_with_safe_data_only() {
        let env = json!({ "payload": {
            "event_type": "CALL_STATE_CHANGED", "from_state": "HELD", "to_state": "CONNECTED",
            "call_id": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "call_version": 7,
            "tenant_id": "11111111-1111-4111-8111-111111111111",
            "data": { "reason": "RESUMED", "caller_number": "+2348012345678",
                      "provider_call_id": "CA123", "sip_uri": "sip:a@b", "note": "free text",
                      "participant_id": "not-a-uuid" } } });
        let e = project_calling(&env).unwrap();
        assert_eq!(e.event_type, "call.resumed");
        assert_eq!(
            Value::Object(e.data),
            json!({ "callId": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "callVersion": 7,
                    "reason": "RESUMED", "state": "CONNECTED" })
        );
        let internal = json!({ "payload": { "event_type": "CALL_MEDIA_CREDENTIAL_ISSUED" } });
        assert!(project_calling(&internal).is_none());
    }

    #[test]
    fn relay_events_are_projected_and_internal_ones_dropped() {
        let env = json!({ "event_type": "privacy-relay.session.revoked", "payload": {
            "context_id": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "session_status": "REVOKED",
            "data": { "relay_number": "+2348012345678" } } });
        let e = project_privacy_relay(&env).unwrap();
        assert_eq!(e.event_type, "relay.revoked");
        assert_eq!(e.data["resource"], "SESSION");
        assert!(!Value::Object(e.data).to_string().contains("234"));
        let number = json!({ "event_type": "privacy-relay.relay-number.assigned", "payload": {} });
        assert!(project_privacy_relay(&number).is_none());
    }
}

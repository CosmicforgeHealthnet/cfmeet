//! The PUBLIC webhook event catalog and the projection of committed producer envelopes
//! into public events (Section 53; the Calling catalog of Part G §68 carried over
//! unchanged, plus the meeting, participant, recording / artifact, number, credential and
//! production-access families).
//!
//! Internal envelopes (`calling.call-state-changed`, `privacy-relay.session.revoked`,
//! `number.assignment.changed`, …) are never delivered verbatim: each maps to at most one
//! public type whose `data` is rebuilt from an explicit allow-list of keys, and every
//! value must be an opaque UUID, a bounded upper-case code or a whitelisted non-negative
//! integer — so no telephone number, URI, storage key, provider identifier, credential,
//! E2EE material or free text can reach a customer endpoint, whatever a producer put in
//! its internal payload.

use serde_json::{Map, Value};

/// Producers (authenticated service identities) and the envelope type prefix each owns.
pub const PRODUCER_MEETING: &str = "cfmeet.meeting-service";
pub const PRODUCER_CALLING: &str = "cfmeet.calling";
pub const PRODUCER_PRIVACY_RELAY: &str = "cfmeet.privacy-relay";
pub const PRODUCER_MESSAGING: &str = "cfmeet.messaging";
pub const PRODUCER_NUMBERING: &str = "cfmeet.numbering";
pub const PRODUCER_SMS_GATEWAY: &str = "cfmeet.sms-gateway";
pub const PRODUCER_DEVELOPER_PLATFORM: &str = "cfmeet.developer-platform";

/// `(producer, envelope type prefix, public event families it may publish)`.
///
/// Calling keeps `usage.*` / `limit.*` (its usage facts and limit notices are public
/// events today; refusing them would lose events at the cut-over).
pub const PRODUCERS: &[(&str, &str, &[&str])] = &[
    (
        PRODUCER_MEETING,
        "meeting-service",
        &["meeting", "participant", "recording", "artifact"],
    ),
    (
        PRODUCER_CALLING,
        "calling",
        &["call", "recording", "voicemail", "usage", "limit"],
    ),
    (PRODUCER_PRIVACY_RELAY, "privacy-relay", &["relay"]),
    (
        PRODUCER_MESSAGING,
        "messaging",
        &["message", "conversation"],
    ),
    (PRODUCER_NUMBERING, "number", &["number"]),
    (PRODUCER_SMS_GATEWAY, "sms-gateway", &["sms"]),
    (
        PRODUCER_DEVELOPER_PLATFORM,
        "developer-platform",
        &["credential", "production_access", "usage", "limit"],
    ),
];

/// Platform Access scope names a subscribed event type may require.
pub mod scopes {
    pub const CALLS_READ: &str = "calls.read";
    pub const ENTERPRISE_VOICEMAIL_READ: &str = "enterprise.voicemail.read";
    pub const ENTERPRISE_RECORDING_READ: &str = "enterprise.recording.read";
    pub const PRIVACY_SESSIONS_READ: &str = "privacy.sessions.read";
    pub const USAGE_READ: &str = "usage.read";
    pub const SMS_READ: &str = "sms.read";
    pub const MESSAGES_READ: &str = "messages.read";
    pub const NUMBERS_READ: &str = "numbers.read";
    pub const CREDENTIALS_MANAGE: &str = "credentials.manage";
    pub const PRODUCTION_ACCESS_REQUEST: &str = "production_access.request";
}

/// One public event type and the domain read scope a subscribing machine credential must
/// hold besides `webhooks.manage` (`None`: Platform Access defines no domain read scope
/// for the family yet — `webhooks.manage` alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventTypeDef {
    pub name: &'static str,
    pub scope: Option<&'static str>,
}

macro_rules! catalog {
    ($($name:literal => $scope:expr),* $(,)?) => {
        pub const CATALOG: &[EventTypeDef] = &[$(EventTypeDef { name: $name, scope: $scope }),*];
    };
}

catalog! {
    "call.requested" => Some(scopes::CALLS_READ),
    "call.incoming" => Some(scopes::CALLS_READ),
    "call.ringing" => Some(scopes::CALLS_READ),
    "call.ring_cancelled" => Some(scopes::CALLS_READ),
    "call.accepted" => Some(scopes::CALLS_READ),
    "call.declined" => Some(scopes::CALLS_READ),
    "call.cancelled" => Some(scopes::CALLS_READ),
    "call.answered" => Some(scopes::CALLS_READ),
    "call.connected" => Some(scopes::CALLS_READ),
    "call.route_changed" => Some(scopes::CALLS_READ),
    "call.held" => Some(scopes::CALLS_READ),
    "call.resumed" => Some(scopes::CALLS_READ),
    "call.transfer_started" => Some(scopes::CALLS_READ),
    "call.transferred" => Some(scopes::CALLS_READ),
    "call.transfer_failed" => Some(scopes::CALLS_READ),
    "call.ended" => Some(scopes::CALLS_READ),
    "call.failed" => Some(scopes::CALLS_READ),
    "call.no_answer" => Some(scopes::CALLS_READ),
    "call.busy" => Some(scopes::CALLS_READ),
    "call.answered_elsewhere" => Some(scopes::CALLS_READ),
    "call.invitation_expired" => Some(scopes::CALLS_READ),
    "voicemail.received" => Some(scopes::ENTERPRISE_VOICEMAIL_READ),
    "voicemail.ready" => Some(scopes::ENTERPRISE_VOICEMAIL_READ),
    "voicemail.deleted" => Some(scopes::ENTERPRISE_VOICEMAIL_READ),
    "recording.ready" => Some(scopes::ENTERPRISE_RECORDING_READ),
    "recording.deleted" => Some(scopes::ENTERPRISE_RECORDING_READ),
    "relay.created" => Some(scopes::PRIVACY_SESSIONS_READ),
    "relay.updated" => Some(scopes::PRIVACY_SESSIONS_READ),
    "relay.revoked" => Some(scopes::PRIVACY_SESSIONS_READ),
    "relay.expired" => Some(scopes::PRIVACY_SESSIONS_READ),
    "relay.participant_changed" => Some(scopes::PRIVACY_SESSIONS_READ),
    "usage.recorded" => Some(scopes::USAGE_READ),
    "limit.warning" => Some(scopes::USAGE_READ),
    "limit.exceeded" => Some(scopes::USAGE_READ),
    "sms.received" => Some(scopes::SMS_READ),
    "sms.requested" => Some(scopes::SMS_READ),
    "sms.submitted" => Some(scopes::SMS_READ),
    "sms.delivered" => Some(scopes::SMS_READ),
    "sms.failed" => Some(scopes::SMS_READ),
    "sms.unknown" => Some(scopes::SMS_READ),
    "sms.blocked" => Some(scopes::SMS_READ),
    "conversation.created" => Some(scopes::MESSAGES_READ),
    "conversation.updated" => Some(scopes::MESSAGES_READ),
    "conversation.member_added" => Some(scopes::MESSAGES_READ),
    "conversation.member_removed" => Some(scopes::MESSAGES_READ),
    "message.created" => Some(scopes::MESSAGES_READ),
    "message.deleted" => Some(scopes::MESSAGES_READ),
    "message.moderated" => Some(scopes::MESSAGES_READ),
    "message.expired" => Some(scopes::MESSAGES_READ),
    // Section 53 families.
    "meeting.started" => None,
    "meeting.ended" => None,
    "participant.joined" => None,
    "participant.left" => None,
    "artifact.ready" => Some(scopes::ENTERPRISE_RECORDING_READ),
    "artifact.deleted" => Some(scopes::ENTERPRISE_RECORDING_READ),
    "number.reservation_changed" => Some(scopes::NUMBERS_READ),
    "number.assignment_changed" => Some(scopes::NUMBERS_READ),
    "number.binding_changed" => Some(scopes::NUMBERS_READ),
    "number.port_changed" => Some(scopes::NUMBERS_READ),
    "number.release_changed" => Some(scopes::NUMBERS_READ),
    "number.lifecycle_changed" => Some(scopes::NUMBERS_READ),
    "number.suspended" => Some(scopes::NUMBERS_READ),
    "number.restored" => Some(scopes::NUMBERS_READ),
    "credential.created" => Some(scopes::CREDENTIALS_MANAGE),
    "credential.rotated" => Some(scopes::CREDENTIALS_MANAGE),
    "credential.revoked" => Some(scopes::CREDENTIALS_MANAGE),
    "production_access.requested" => Some(scopes::PRODUCTION_ACCESS_REQUEST),
    "production_access.approved" => Some(scopes::PRODUCTION_ACCESS_REQUEST),
    "production_access.rejected" => Some(scopes::PRODUCTION_ACCESS_REQUEST),
}

pub fn definition(name: &str) -> Option<&'static EventTypeDef> {
    CATALOG.iter().find(|d| d.name == name)
}

/// The family of a public type (`call` of `call.ended`).
pub fn family(event_type: &str) -> &str {
    event_type.split('.').next().unwrap_or("")
}

/// A projected public event.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicEvent {
    pub event_type: &'static str,
    pub data: Map<String, Value>,
}

/// Why an envelope cannot be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The authenticated service is not a webhook event producer.
    UnknownProducer,
    /// The envelope type is outside the producer's own domain (unknown family).
    ForeignFamily,
}

/// Projects `envelope` of `producer`. `Ok(None)`: a fact of the producer's own domain that
/// is internal-only (acknowledged, never public).
pub fn project(producer: &str, envelope: &Value) -> Result<Option<PublicEvent>, Refusal> {
    let (_, prefix, families) = PRODUCERS
        .iter()
        .find(|(p, _, _)| *p == producer)
        .ok_or(Refusal::UnknownProducer)?;
    let envelope_type = envelope
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    if envelope_type.split('.').next() != Some(prefix) {
        return Err(Refusal::ForeignFamily);
    }
    let projected = match producer {
        PRODUCER_CALLING => project_calling(envelope),
        PRODUCER_PRIVACY_RELAY => project_privacy_relay(envelope),
        PRODUCER_SMS_GATEWAY => project_sms_gateway(envelope),
        PRODUCER_MESSAGING => project_messaging(envelope),
        PRODUCER_NUMBERING => project_numbering(envelope),
        PRODUCER_MEETING => project_meeting(envelope),
        PRODUCER_DEVELOPER_PLATFORM => project_developer_platform(envelope),
        _ => return Err(Refusal::UnknownProducer),
    };
    match projected {
        // Defence in depth: a producer only ever publishes its own public families.
        Some(e) if !families.contains(&family(e.event_type)) => Err(Refusal::ForeignFamily),
        other => Ok(other),
    }
}

/// Keys that may cross into public `data` (internal snake_case → public camelCase) and
/// whether an integer value is acceptable for them (Calling, Privacy Relay, usage and
/// limit facts).
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
        Value::String(s) if s.len() == 36 && uuid::Uuid::parse_str(s).is_ok() => {
            Some(Value::String(s.to_ascii_lowercase()))
        }
        Value::String(s) if is_code(s) => Some(v.clone()),
        Value::Number(n) if numeric && n.as_u64().is_some() => Some(v.clone()),
        _ => None,
    }
}

/// Copies the allow-listed (`keys`), safe fields of `sources` (later sources win).
fn copy_fields(keys: &[(&str, &str, bool)], sources: &[&Value], out: &mut Map<String, Value>) {
    for source in sources {
        let Some(obj) = source.as_object() else {
            continue;
        };
        for (internal, public, numeric) in keys {
            if let Some(v) = obj.get(*internal).and_then(|v| safe_value(v, *numeric)) {
                out.insert((*public).to_string(), v);
            }
        }
    }
}

/// Copies the allow-listed, safe Calling / Privacy Relay / usage / limit fields.
pub fn safe_fields(sources: &[&Value], out: &mut Map<String, Value>) {
    copy_fields(ALLOWED_KEYS, sources, out);
}

/// Projects a Calling envelope (`cfmeet.calling.call-lifecycle-event` payload).
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

/// Projects a Privacy Relay envelope (`privacy-relay.<aggregate>.<fact>`).
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
    let payload = envelope.get("payload")?;
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
    copy_fields(SMS_KEYS, &[payload], &mut data);
    data.get("messageId")?;
    Some(PublicEvent { event_type, data })
}

/// Messaging keys that may cross. Deliberately absent: text, ciphertext, attachment keys,
/// device keys, MLS data, policy digests, display names, telephone numbers, free-text
/// reasons.
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
/// messages are public; the data are ids, sequences and bounded codes — whatever the
/// security profile, no content can cross.
pub fn project_messaging(envelope: &Value) -> Option<PublicEvent> {
    let internal = envelope.get("event_type")?.as_str()?;
    let payload = envelope.get("payload")?;
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
    copy_fields(MESSAGING_KEYS, &[payload], &mut data);
    data.get("conversationId")?;
    Some(PublicEvent { event_type, data })
}

/// Number keys that may cross. Deliberately absent: the number itself (E.164 or masked),
/// provider references, KYC evidence, routing targets and grants.
const NUMBER_KEYS: &[(&str, &str, bool)] = &[
    ("number_resource_id", "numberResourceId", false),
    ("assignment_id", "assignmentId", false),
    ("reservation_id", "reservationId", false),
    ("suspension_id", "suspensionId", false),
    ("binding_id", "bindingId", false),
    ("port_request_id", "portRequestId", false),
    ("release_request_id", "releaseRequestId", false),
    ("to_status", "status", false),
    ("status", "status", false),
    ("from_status", "previousStatus", false),
    ("event_kind", "change", false),
    ("direction", "direction", false),
    ("policy", "policy", false),
    ("source", "source", false),
    ("reason_code", "reasonCode", false),
    ("revision", "revision", true),
];

/// Projects a Numbering envelope (`number.<aggregate>.<change>`). Provider, KYC, risk,
/// pool, catalog, routing and usage facts are internal.
pub fn project_numbering(envelope: &Value) -> Option<PublicEvent> {
    let internal = envelope.get("event_type")?.as_str()?;
    let payload = envelope.get("payload")?;
    let event_type = match internal {
        "number.reservation.changed" => "number.reservation_changed",
        "number.assignment.changed" => "number.assignment_changed",
        "number.binding.changed" => "number.binding_changed",
        "number.port.changed" => "number.port_changed",
        "number.release.changed" => "number.release_changed",
        "number.resource.lifecycle-changed" => "number.lifecycle_changed",
        "number.suspended" => "number.suspended",
        "number.restored" => "number.restored",
        _ => return None,
    };
    let mut data = Map::new();
    copy_fields(NUMBER_KEYS, &[payload], &mut data);
    data.get("numberResourceId")?;
    Some(PublicEvent { event_type, data })
}

/// Meeting keys that may cross. Deliberately absent: display names, emails, join links,
/// storage keys and URLs, transcripts, chat, media / E2EE keys.
const MEETING_KEYS: &[(&str, &str, bool)] = &[
    ("meeting_id", "meetingId", false),
    ("participant_id", "participantId", false),
    ("recording_id", "recordingId", false),
    ("artifact_id", "artifactId", false),
    ("artifact_kind", "artifactKind", false),
    ("role", "role", false),
    ("status", "status", false),
    ("reason", "reason", false),
    ("ended_reason", "endedReason", false),
    ("duration_ms", "durationMs", true),
    ("participant_count", "participantCount", true),
];

/// Projects a Meeting Service envelope (`meeting-service.<aggregate>.<fact>`).
pub fn project_meeting(envelope: &Value) -> Option<PublicEvent> {
    let internal = envelope.get("event_type")?.as_str()?;
    let payload = envelope.get("payload")?;
    let event_type = match internal {
        "meeting-service.meeting.started" => "meeting.started",
        "meeting-service.meeting.ended" => "meeting.ended",
        "meeting-service.participant.joined" => "participant.joined",
        "meeting-service.participant.left" => "participant.left",
        "meeting-service.recording.ready" => "recording.ready",
        "meeting-service.recording.deleted" => "recording.deleted",
        "meeting-service.artifact.ready" => "artifact.ready",
        "meeting-service.artifact.deleted" => "artifact.deleted",
        _ => return None,
    };
    let mut data = Map::new();
    copy_fields(MEETING_KEYS, &[payload], &mut data);
    data.get("meetingId")?;
    Some(PublicEvent { event_type, data })
}

/// Developer Platform keys that may cross (its own credential lifecycle — ids and status,
/// never a secret, prefix or verifier — production access, usage and limits).
const DEVELOPER_PLATFORM_KEYS: &[(&str, &str, bool)] = &[
    ("credential_id", "credentialId", false),
    ("service_account_id", "serviceAccountId", false),
    ("request_id", "requestId", false),
    ("environment_id", "environmentId", false),
    ("status", "status", false),
];

/// Projects the Developer Platform's own facts (`developer-platform.<aggregate>.<fact>`).
pub fn project_developer_platform(envelope: &Value) -> Option<PublicEvent> {
    let internal = envelope.get("event_type")?.as_str()?;
    let payload = envelope.get("payload")?;
    let event_type = match internal {
        "developer-platform.credential.created" => "credential.created",
        "developer-platform.credential.rotated" => "credential.rotated",
        "developer-platform.credential.revoked" => "credential.revoked",
        "developer-platform.production-access.requested" => "production_access.requested",
        "developer-platform.production-access.approved" => "production_access.approved",
        "developer-platform.production-access.rejected" => "production_access.rejected",
        "developer-platform.usage.recorded" => "usage.recorded",
        "developer-platform.limit.warning" => "limit.warning",
        "developer-platform.limit.exceeded" => "limit.exceeded",
        _ => return None,
    };
    let mut data = Map::new();
    copy_fields(DEVELOPER_PLATFORM_KEYS, &[payload], &mut data);
    safe_fields(&[payload], &mut data);
    Some(PublicEvent { event_type, data })
}

/// Every public `data` key any projection can emit (the event contract's vocabulary).
pub fn public_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = [
        ALLOWED_KEYS,
        SMS_KEYS,
        MESSAGING_KEYS,
        NUMBER_KEYS,
        MEETING_KEYS,
        DEVELOPER_PLATFORM_KEYS,
    ]
    .iter()
    .flat_map(|keys| keys.iter().map(|(_, public, _)| *public))
    .chain(["state", "resource"])
    .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_names_are_unique_and_every_family_has_a_producer() {
        for (i, d) in CATALOG.iter().enumerate() {
            assert!(
                CATALOG[i + 1..].iter().all(|o| o.name != d.name),
                "{}",
                d.name
            );
            assert!(
                PRODUCERS
                    .iter()
                    .any(|(_, _, f)| f.contains(&family(d.name))),
                "{}",
                d.name
            );
        }
        assert!(definition("call.ended").is_some());
        assert!(definition("calling.call-state-changed").is_none());
        assert!(definition("*").is_none());
    }

    #[test]
    fn producers_publish_only_their_own_families() {
        let calling = json!({ "event_type": "calling.call-state-changed", "payload": {
            "event_type": "CALL_STATE_CHANGED", "to_state": "ENDED",
            "call_id": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a" } });
        assert_eq!(
            project(PRODUCER_CALLING, &calling)
                .unwrap()
                .unwrap()
                .event_type,
            "call.ended"
        );
        // Messaging cannot publish a Calling envelope (unknown family for it).
        assert_eq!(
            project(PRODUCER_MESSAGING, &calling),
            Err(Refusal::ForeignFamily)
        );
        assert_eq!(
            project("cfmeet.telephony", &calling),
            Err(Refusal::UnknownProducer)
        );
        // An internal fact of the producer's own domain is acknowledged, not public.
        let internal = json!({ "event_type": "calling.media-credential-issued",
            "payload": { "event_type": "CALL_MEDIA_CREDENTIAL_ISSUED" } });
        assert_eq!(project(PRODUCER_CALLING, &internal), Ok(None));
    }

    #[test]
    fn numbers_and_meetings_carry_ids_and_codes_only() {
        let n = json!({ "event_type": "number.assignment.changed", "payload": {
            "number_resource_id": "22222222-2222-4222-8222-222222222222",
            "assignment_id": "11111111-1111-4111-8111-111111111111",
            "to_status": "ACTIVE", "from_status": "PENDING", "revision": 3,
            "e164": "+15550100123", "provider_ref": "prov-1", "routing_target": "sip:a@b" } });
        let e = project(PRODUCER_NUMBERING, &n).unwrap().unwrap();
        assert_eq!(e.event_type, "number.assignment_changed");
        assert_eq!(
            Value::Object(e.data),
            json!({ "numberResourceId": "22222222-2222-4222-8222-222222222222",
                    "assignmentId": "11111111-1111-4111-8111-111111111111",
                    "status": "ACTIVE", "previousStatus": "PENDING", "revision": 3 })
        );
        let kyc = json!({ "event_type": "number.kyc-case.changed", "payload": {} });
        assert_eq!(project(PRODUCER_NUMBERING, &kyc), Ok(None));
        let m = json!({ "event_type": "meeting-service.recording.ready", "payload": {
            "meeting_id": "33333333-3333-4333-8333-333333333333",
            "recording_id": "44444444-4444-4444-8444-444444444444", "duration_ms": 1200,
            "storage_key": "recordings/a/b.mp4", "download_url": "https://s3.example/x",
            "display_name": "Alice", "media_key": "c2VjcmV0" } });
        let e = project(PRODUCER_MEETING, &m).unwrap().unwrap();
        assert_eq!(e.event_type, "recording.ready");
        let text = Value::Object(e.data).to_string();
        for leaked in ["recordings/", "https", "Alice", "c2VjcmV0"] {
            assert!(!text.contains(leaked), "{leaked}");
        }
    }

    #[test]
    fn messaging_and_sms_never_carry_content() {
        let c = "11111111-1111-4111-8111-111111111111";
        let env = json!({ "event_type": "messaging.message.created", "payload": {
            "conversation_id": c, "message_id": "22222222-2222-4222-8222-222222222222",
            "sequence": 7, "message_kind": "APPLICATION",
            "text": "the launch code is 1234", "ciphertext": "AAEC", "phone": "+15550100123" } });
        let e = project_messaging(&env).unwrap();
        assert_eq!(
            Value::Object(e.data),
            json!({ "conversationId": c, "messageId": "22222222-2222-4222-8222-222222222222",
                    "sequence": 7, "messageKind": "APPLICATION" })
        );
        let sms = json!({ "event_type": "sms-gateway.message.received", "payload": {
            "message_id": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "content": "hello",
            "from": "+15550100123", "status": "RECEIVED" } });
        let e = project_sms_gateway(&sms).unwrap();
        assert!(!Value::Object(e.data).to_string().contains("hello"));
    }

    #[test]
    fn state_changes_map_to_public_types_with_safe_data_only() {
        let env = json!({ "payload": {
            "event_type": "CALL_STATE_CHANGED", "from_state": "HELD", "to_state": "CONNECTED",
            "call_id": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "call_version": 7,
            "data": { "reason": "RESUMED", "caller_number": "+2348012345678",
                      "provider_call_id": "CA123", "sip_uri": "sip:a@b", "note": "free text" } } });
        let e = project_calling(&env).unwrap();
        assert_eq!(e.event_type, "call.resumed");
        assert_eq!(
            Value::Object(e.data),
            json!({ "callId": "8d3e2a1c-0b6f-4c1e-9a49-6f0e3f2d1c7a", "callVersion": 7,
                    "reason": "RESUMED", "state": "CONNECTED" })
        );
    }
}

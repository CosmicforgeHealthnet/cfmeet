//! Closed result schemas of the account and meeting reads served from the Meeting Service
//! (A2.5). Identifiers, codes, counts and timestamps, plus the display names and meeting
//! titles that R1 detail reads may show. Never an email address (search matches on it but
//! never returns it; the detail shows `email_masked` only), a password hash, token, session
//! label, device key or digest, MFA secret or recovery code, IP address, location, message
//! content or media. No property name is on HQ's personal-data list.

use serde_json::{json, Map, Value};

use super::model::CURSOR_PATTERN;

pub fn insert(m: &mut Map<String, Value>) {
    let uuid = json!({"type": "string", "format": "uuid"});
    let nullable_uuid = json!({"type": ["string", "null"], "format": "uuid"});
    let ts = json!({"type": "string", "format": "date-time"});
    let nullable_ts = json!({"type": ["string", "null"], "format": "date-time"});
    let code = json!({"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"});
    let nullable_code = json!({"type": ["string", "null"], "pattern": "^[A-Z][A-Z0-9_]{0,63}$"});
    let count = json!({"type": "integer", "minimum": 0});
    let version = json!({"type": "integer", "minimum": 0});
    let name = |max: u32| json!({"type": "string", "minLength": 1, "maxLength": max});
    let nullable_name = |max: u32| json!({"type": ["string", "null"], "maxLength": max});
    let page = |item: &str| {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "next_cursor"],
            "properties": {
                "items": {"type": "array", "maxItems": 100, "items": {"$ref": format!("#/components/schemas/{item}")}},
                "next_cursor": {"type": ["string", "null"], "pattern": CURSOR_PATTERN}
            }
        })
    };
    let object = |description: &str, props: Value| {
        let required: Vec<String> = props.as_object().expect("object").keys().cloned().collect();
        json!({"description": description, "type": "object", "additionalProperties": false, "required": required, "properties": props})
    };

    let user_props = json!({
        "user_id": uuid, "username": name(50), "display_name": nullable_name(200), "role": code, "status": code,
        "created_at": ts, "last_login_at": nullable_ts
    });
    m.insert(
        "AccountUser".into(),
        object(
            "One CF Meet account. Search matches on the email address but never returns it.",
            user_props.clone(),
        ),
    );
    m.insert("AccountUserPage".into(), page("AccountUser"));
    let mut detail = user_props.as_object().cloned().unwrap_or_default();
    for (k, v) in [
        (
            "email_masked",
            json!({"type": "string", "maxLength": 120, "description": "First character of the local part and of the domain label, the top-level domain kept (a***@e***.com). Never the address."}),
        ),
        ("email_verified", json!({"type": "boolean"})),
        ("auth_type", code.clone()),
        ("locale", nullable_name(35)),
        ("timezone", nullable_name(64)),
    ] {
        detail.insert(k.into(), v);
    }
    m.insert(
        "AccountUserDetail".into(),
        object(
            "One account in detail (R1, every read audited). The email address only masked.",
            Value::Object(detail),
        ),
    );

    m.insert(
        "AccountSession".into(),
        object(
            "One account session: method, times and revocation. No client label, no token.",
            json!({
                "session_id": uuid, "auth_method": code, "authenticated_at": ts, "created_at": ts,
                "last_refreshed_at": nullable_ts, "expires_at": ts, "revoked_at": nullable_ts, "revoke_reason": nullable_code
            }),
        ),
    );
    m.insert("AccountSessionPage".into(), page("AccountSession"));

    m.insert(
        "AccountDevice".into(),
        object(
            "One Account device: class, platform, trust, state and times. No label, key or digest.",
            json!({
                "device_id": uuid, "device_class": {"enum": ["WEB", "MOBILE"]}, "platform": {"enum": ["WEB", "IOS", "ANDROID"]},
                "trust": {"enum": ["UNTRUSTED", "TRUSTED"]}, "trusted_via": nullable_code, "trusted_at": nullable_ts,
                "state": code, "created_at": ts, "last_seen_at": ts, "revoked_at": nullable_ts, "revoke_reason": nullable_code
            }),
        ),
    );
    m.insert("AccountDevicePage".into(), page("AccountDevice"));

    m.insert(
        "AccountMfa".into(),
        object(
            "MFA status: TOTP enrolment state and the count of unused recovery codes. Never a secret or a code.",
            json!({
                "user_id": uuid,
                "totp_state": {"enum": ["NOT_ENROLLED", "PENDING_CONFIRMATION", "ENABLED"]},
                "totp_enrolled_at": nullable_ts,
                "totp_confirmed_at": nullable_ts,
                "recovery_codes_remaining": count
            }),
        ),
    );

    m.insert(
        "AccountSecurityEvent".into(),
        object(
            "One account security event (sign-ins and changes): type, outcome, actor kind and time. No detail, IP address or location.",
            json!({
                "event_id": uuid, "sequence": {"type": "integer", "minimum": 1}, "event_type": code,
                "outcome": {"enum": ["SUCCESS", "FAILURE", "DENIED"]}, "actor_kind": {"enum": ["USER", "ADMIN", "SYSTEM", "ANONYMOUS"]},
                "actor_user_id": nullable_uuid, "session_id": nullable_uuid, "occurred_at": ts
            }),
        ),
    );
    m.insert(
        "AccountSecurityEventPage".into(),
        page("AccountSecurityEvent"),
    );

    m.insert(
        "MeetingSpace".into(),
        object(
            "One meeting space. No description, passcode or policy documents.",
            json!({
                "space_id": uuid, "meeting_code": name(64), "title": name(300), "meeting_type": code, "status": code,
                "owner_user_id": nullable_uuid, "organization_id": nullable_uuid, "created_at": ts, "updated_at": ts,
                "archived_at": nullable_ts, "version": version
            }),
        ),
    );
    m.insert("MeetingSpacePage".into(), page("MeetingSpace"));

    m.insert(
        "MeetingConference".into(),
        object(
            "One meeting (conference): state, times and participant counts. No room names or provider ids.",
            json!({
                "meeting_id": uuid, "space_id": uuid, "title": name(300), "kind": code, "status": code,
                "scheduled_start_at": nullable_ts, "scheduled_end_at": nullable_ts, "started_at": nullable_ts,
                "ended_at": nullable_ts, "termination_reason": nullable_code, "locked": {"type": "boolean"},
                "recording_state": code, "transcription_state": code, "participants_admitted": count,
                "participants_total": count, "created_at": ts, "version": version
            }),
        ),
    );
    m.insert("MeetingConferencePage".into(), page("MeetingConference"));

    m.insert(
        "MeetingParticipant".into(),
        object(
            "One participant. No guest identity, external reference, credential or denial note.",
            json!({
                "participant_id": uuid, "identity_type": code, "user_id": nullable_uuid, "display_name": name(200),
                "role": code, "admission_state": code, "requested_at": ts, "admitted_at": nullable_ts,
                "removed_at": nullable_ts, "banned_at": nullable_ts
            }),
        ),
    );
    m.insert("MeetingParticipantPage".into(), page("MeetingParticipant"));

    m.insert(
        "MeetingAuditEvent".into(),
        object(
            "One conference audit event. No actor or target reference, metadata or request id.",
            json!({
                "event_id": uuid, "space_id": uuid, "meeting_id": nullable_uuid, "actor_type": code,
                "action": code, "target_type": nullable_code, "occurred_at": ts
            }),
        ),
    );
    m.insert("MeetingAuditEventPage".into(), page("MeetingAuditEvent"));
}

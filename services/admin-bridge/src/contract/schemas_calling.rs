//! Closed result schemas of the Calling reads (A2.7). Every read is scoped to the request's
//! tenant. Ids, codes, states, counts, durations, sizes and times only: never a telephone
//! number, SIP URI, contact reference, display name, media URL, object key, key material,
//! free text or restricted incident evidence. Money is in minor units with its own ISO 4217
//! currency, never summed. No property name is on HQ's personal-data list.

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
    let nullable_count = json!({"type": ["integer", "null"], "minimum": 0});
    let nullable_int = json!({"type": ["integer", "null"]});
    let nullable_bool = json!({"type": ["boolean", "null"]});
    let boolean = json!({"type": "boolean"});
    let currency = json!({"type": "string", "pattern": "^[A-Z]{3}$"});
    let scope = json!({"enum": ["PLATFORM", "TENANT"]});
    let codes = |max: u32| json!({"type": "array", "maxItems": max, "items": {"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"}});
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
    let bounded = |item: &str, key: &str, description: &str| {
        json!({
            "description": description,
            "type": "object",
            "additionalProperties": false,
            "required": [key, "truncated"],
            "properties": {
                key: {"type": "array", "maxItems": 100, "items": {"$ref": format!("#/components/schemas/{item}")}},
                "truncated": {"type": "boolean"}
            }
        })
    };

    let call = json!({
        "call_id": uuid, "application_id": uuid, "environment_id": uuid, "call_kind": code,
        "state": code, "origin": code, "participant_count": count,
        "terminal_reason": nullable_code, "failure_category": nullable_code,
        "requested_at": nullable_ts, "answered_at": nullable_ts, "connected_at": nullable_ts,
        "ended_at": nullable_ts, "created_at": ts, "version": count
    });
    m.insert(
        "CallingCall".into(),
        object("One call of the tenant: ids, kind, state, origin, counts and times. Never a telephone number, SIP URI or display name.", call.clone()),
    );
    m.insert("CallingCallPage".into(), page("CallingCall"));
    m.insert(
        "CallingCallParticipant".into(),
        object(
            "One participant: principal id, kind, role and times only.",
            json!({"principal_id": uuid, "principal_kind": code, "role": code, "is_host": boolean, "joined_at": ts, "left_at": nullable_ts}),
        ),
    );
    let mut detail = call.as_object().cloned().expect("object");
    detail.insert(
        "participants".into(),
        json!({"type": "array", "maxItems": 500, "items": {"$ref": "#/components/schemas/CallingCallParticipant"}}),
    );
    m.insert(
        "CallingCallDetail".into(),
        object(
            "One call with its participants (at most 500).",
            Value::Object(detail),
        ),
    );

    m.insert(
        "CallingAbuseReport".into(),
        object(
            "One OPEN abuse report of the tenant: bounded codes and opaque ids, never free text.",
            json!({
                "abuse_report_id": uuid, "application_id": uuid, "environment_id": uuid, "call_id": uuid,
                "reporting_principal_id": uuid, "reported_principal_id": nullable_uuid, "category": code,
                "comment_code": nullable_code, "status": code, "created_at": ts, "reviewed_at": nullable_ts,
                "version": count
            }),
        ),
    );
    m.insert("CallingAbuseReportPage".into(), page("CallingAbuseReport"));
    m.insert(
        "CallingBlockRule".into(),
        object(
            "One block rule. A blocked contact is only reported as present, never named.",
            json!({
                "block_rule_id": uuid, "application_id": uuid, "environment_id": uuid,
                "blocker_kind": code, "blocker_principal_id": nullable_uuid, "blocked_kind": code,
                "blocked_principal_id": nullable_uuid, "has_contact_reference": boolean,
                "reason_code": nullable_code, "status": code, "created_at": ts, "released_at": nullable_ts,
                "version": count
            }),
        ),
    );
    m.insert("CallingBlockRulePage".into(), page("CallingBlockRule"));

    let limit = json!({
        "requests_per_minute": nullable_int, "max_concurrent_communications": nullable_int,
        "telecom_spend_daily_limit_minor": nullable_int, "telecom_spend_monthly_limit_minor": nullable_int,
        "updated_at": ts
    });
    m.insert(
        "CallingLimitPolicy".into(),
        object(
            "A limit policy; null fields are not configured.",
            limit.clone(),
        ),
    );
    let mut app_limit = Map::new();
    app_limit.insert("application_id".into(), uuid.clone());
    app_limit.extend(limit.as_object().cloned().expect("object"));
    m.insert(
        "CallingApplicationLimitPolicy".into(),
        object("An application's limit policy.", Value::Object(app_limit)),
    );
    m.insert(
        "CallingLimits".into(),
        object(
            "The tenant's limit policies (at most 100 applications; truncated beyond). Spend limits are in minor units of the tenant's billing currency.",
            json!({
                "tenant_id": uuid, "currency": currency,
                "tenant": {"anyOf": [{"type": "null"}, {"$ref": "#/components/schemas/CallingLimitPolicy"}]},
                "applications": {"type": "array", "maxItems": 100, "items": {"$ref": "#/components/schemas/CallingApplicationLimitPolicy"}},
                "truncated": boolean
            }),
        ),
    );

    m.insert(
        "CallingRoutingPolicy".into(),
        object(
            "An environment's routing policy summary. Destination and duplicate-risk sub-policies are not relayed.",
            json!({
                "environment_id": uuid, "application_id": uuid, "revision": count, "updated_at": ts,
                "allowed_route_classes": codes(32), "app_first": nullable_bool,
                "public_calling_enabled": nullable_bool, "callback_relay_enabled": nullable_bool,
                "require_verified_contacts": nullable_bool, "max_failover_attempts": nullable_count,
                "compliance_required_countries": {"type": "array", "maxItems": 300, "items": {"type": "string", "pattern": "^([A-Z]{2}|\\*)$"}}
            }),
        ),
    );
    m.insert(
        "CallingRoutingPolicies".into(),
        bounded(
            "CallingRoutingPolicy",
            "environments",
            "The tenant's routing policies (at most 100; truncated beyond).",
        ),
    );
    m.insert(
        "CallingEnterprisePolicy".into(),
        object(
            "An enterprise calling policy: feature switches and operator-granted administrative capabilities.",
            json!({
                "policy_id": uuid, "environment_id": uuid, "application_id": uuid,
                "name": {"type": "string", "minLength": 1, "maxLength": 100}, "is_default": boolean,
                "status": code, "internal_calling_enabled": boolean, "external_public_calling_allowed": boolean,
                "public_destination_countries": {"type": "array", "maxItems": 300, "items": {"type": "string", "pattern": "^([A-Z]{2}|\\*)$"}},
                "forwarding_allowed": boolean, "public_forwarding_allowed": boolean,
                "voicemail_allowed": boolean, "transfer_allowed": boolean, "admin_capabilities": codes(32),
                "version": count, "updated_at": ts, "disabled_at": nullable_ts
            }),
        ),
    );
    m.insert(
        "CallingEnterprisePolicies".into(),
        bounded(
            "CallingEnterprisePolicy",
            "policies",
            "The tenant's enterprise calling policies (at most 100; truncated beyond).",
        ),
    );
    m.insert(
        "CallingSpendPolicy".into(),
        object(
            "A telecom spend / velocity policy; amounts in minor units of its own currency, never summed across currencies.",
            json!({
                "environment_id": uuid, "application_id": uuid, "currency": currency, "enabled": boolean,
                "leg_allowance_minor": nullable_int, "max_leg_seconds": nullable_int,
                "daily_limit_minor": nullable_int, "monthly_limit_minor": nullable_int,
                "max_concurrent_legs": nullable_int, "max_originations_per_minute": nullable_int,
                "revision": count, "updated_at": ts
            }),
        ),
    );
    m.insert(
        "CallingSpendPolicies".into(),
        bounded(
            "CallingSpendPolicy",
            "environments",
            "The tenant's spend policies (at most 100; truncated beyond).",
        ),
    );

    m.insert(
        "CallingRecording".into(),
        object(
            "Recording metadata only: never an object key or media URL.",
            json!({
                "recording_id": uuid, "call_id": uuid, "environment_id": uuid, "state": code,
                "capture_source": code, "duration_ms": nullable_count, "size_bytes": nullable_count,
                "retention_until": nullable_ts, "created_at": ts, "deleted_at": nullable_ts
            }),
        ),
    );
    m.insert("CallingRecordingPage".into(), page("CallingRecording"));
    m.insert(
        "CallingVoicemail".into(),
        object(
            "Voicemail metadata only: never an object key, media URL or caller identity.",
            json!({
                "message_id": uuid, "call_id": nullable_uuid, "mailbox_id": uuid, "environment_id": uuid,
                "state": code, "read_state": code, "duration_ms": nullable_count, "size_bytes": nullable_count,
                "retention_until": nullable_ts, "received_at": nullable_ts, "deleted_at": nullable_ts
            }),
        ),
    );
    m.insert("CallingVoicemailPage".into(), page("CallingVoicemail"));
    m.insert(
        "CallingRetentionHold".into(),
        object(
            "A retention hold of the tenant or the platform. The placing operator is not relayed.",
            json!({
                "hold_id": uuid, "scope": scope, "data_class": {"type": "string", "minLength": 1, "maxLength": 64},
                "resource_id": nullable_uuid, "environment_id": nullable_uuid, "reason_code": code,
                "active": boolean, "created_at": ts, "released_at": nullable_ts
            }),
        ),
    );
    m.insert(
        "CallingRetentionHoldPage".into(),
        page("CallingRetentionHold"),
    );
    m.insert(
        "CallingIncident".into(),
        object(
            "An operational incident of the tenant or the platform. Restricted evidence and dedupe keys are never relayed.",
            json!({
                "incident_id": uuid, "scope": scope, "category": code, "severity": code, "status": code,
                "call_id": nullable_uuid, "resource_type": nullable_code, "resource_id": nullable_uuid,
                "occurrences": count, "first_seen_at": ts, "last_seen_at": ts, "resolved_at": nullable_ts
            }),
        ),
    );
    m.insert("CallingIncidentPage".into(), page("CallingIncident"));
    m.insert(
        "CallingWebhookKey".into(),
        object(
            "Webhook subscriptions per signing-secret sealing key id (counts only, never a secret or sealed value).",
            json!({
                "key_id": {"type": ["string", "null"], "maxLength": 32},
                "subscriptions": count, "active_subscriptions": count, "last_updated_at": nullable_ts
            }),
        ),
    );
    m.insert("CallingWebhookKeyPage".into(), page("CallingWebhookKey"));
    m.insert(
        "CallingAuditEntry".into(),
        object(
            "One operator audit entry of the tenant. The detail document is never relayed.",
            json!({
                "event_id": uuid, "actor": {"type": "string", "minLength": 1, "maxLength": 100},
                "action": {"type": "string", "minLength": 1, "maxLength": 64},
                "target_kind": {"type": "string", "minLength": 1, "maxLength": 32},
                "target_id": nullable_uuid, "application_id": nullable_uuid,
                "environment_id": nullable_uuid, "occurred_at": ts
            }),
        ),
    );
    m.insert("CallingAuditEntryPage".into(), page("CallingAuditEntry"));
}

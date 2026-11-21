//! Closed result schemas of the Messaging reads (A2.6). Identifiers, categories, states,
//! counts and timestamps only: never message content, ciphertext, MLS state, keys,
//! attachments, report evidence, audit details, outbox payloads, export object keys,
//! legal case references or free text. No property name is on HQ's personal-data list.

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
    let hex = json!({"type": "string", "pattern": "^[0-9a-f]{64}$"});
    let closed = |desc: &str, props: Value| {
        let required: Vec<String> = props
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        json!({"description": desc, "type": "object", "additionalProperties": false, "required": required, "properties": props})
    };
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

    let report_props = json!({
        "report_id": uuid, "tenant_id": uuid, "application_id": uuid, "environment_id": uuid,
        "conversation_id": uuid,
        "security_profile": {"enum": ["PRIVATE_E2EE", "MANAGED"]},
        "reporter_principal_id": uuid, "reported_principal_id": nullable_uuid,
        "message_ids": {"type": "array", "maxItems": 20, "items": uuid},
        "category": {"enum": ["SPAM", "HARASSMENT", "HATE", "VIOLENCE", "SEXUAL_CONTENT", "CHILD_SAFETY", "SELF_HARM", "ILLEGAL_CONTENT", "IMPERSONATION", "OTHER"]},
        "evidence_kind": {"enum": ["NONE", "MANAGED_REFERENCE", "REPORTER_DISCLOSURE"]},
        "status": {"enum": ["OPEN", "ACTIONED", "DISMISSED"]},
        "resolution": nullable_code, "resolved_at": nullable_ts, "created_at": ts
    });
    let mut listed = report_props.as_object().cloned().unwrap_or_default();
    listed.insert(
        "version".into(),
        json!({"type": "integer", "minimum": 1, "description": "The report's row version (expected_version of its moderation commands)."}),
    );
    m.insert(
        "MessagingReport".into(),
        closed(
            "One chat report: ids, category, states and times. The evidence itself (a sealed reporter disclosure or a MANAGED message reference) is never relayed; evidence_kind only says which kind was given.",
            Value::Object(listed),
        ),
    );
    m.insert("MessagingReportPage".into(), page("MessagingReport"));
    let action = closed(
        "One moderation action taken on the report.",
        json!({
            "action_id": uuid,
            "kind": {"enum": ["REMOVE_MESSAGE", "MUTE_MEMBER", "UNMUTE_MEMBER", "REMOVE_MEMBER", "RESTRICT_PRINCIPAL", "LIFT_RESTRICTION", "DISMISS_REPORT"]},
            "reason": code, "conversation_id": nullable_uuid, "message_id": nullable_uuid,
            "principal_id": nullable_uuid, "effective_until": nullable_ts, "created_at": ts
        }),
    );
    let mut detail_props = report_props.as_object().cloned().unwrap_or_default();
    detail_props.insert(
        "actions".into(),
        json!({"type": "array", "maxItems": 100, "items": action}),
    );
    m.insert(
        "MessagingReportDetail".into(),
        closed(
            "One chat report and its moderation actions (no evidence).",
            Value::Object(detail_props),
        ),
    );
    m.insert(
        "MessagingHold".into(),
        closed(
            "One legal hold. The case reference is not relayed: reason_category says why.",
            json!({
                "hold_id": uuid, "tenant_id": uuid, "application_id": uuid, "environment_id": uuid,
                "scope_kind": {"enum": ["CONVERSATION", "PRINCIPAL", "ENVIRONMENT"]},
                "conversation_id": nullable_uuid, "principal_id": nullable_uuid,
                "reason_category": {"enum": ["LITIGATION", "REGULATORY", "INVESTIGATION", "INTERNAL_POLICY"]},
                "status": {"enum": ["ACTIVE", "RELEASED"]},
                "created_at": ts, "expires_at": nullable_ts, "released_at": nullable_ts, "release_reason": nullable_code,
                "version": {"type": "integer", "minimum": 1}
            }),
        ),
    );
    m.insert("MessagingHoldPage".into(), page("MessagingHold"));
    m.insert(
        "MessagingExport".into(),
        closed(
            "One MANAGED conversation export (encrypted chats are never exported). The object key and digest are not relayed.",
            json!({
                "export_id": uuid, "tenant_id": uuid, "application_id": uuid, "environment_id": uuid,
                "conversation_id": uuid, "status": {"enum": ["PENDING", "READY", "FAILED", "EXPIRED"]},
                "size_bytes": nullable_count, "message_count": nullable_count, "failure": nullable_code,
                "attempts": count, "created_at": ts, "completed_at": nullable_ts, "expires_at": ts
            }),
        ),
    );
    m.insert("MessagingExportPage".into(), page("MessagingExport"));
    m.insert(
        "MessagingDeletionRequest".into(),
        closed(
            "One principal erasure or conversation purge request and its counts.",
            json!({
                "request_id": uuid, "tenant_id": uuid, "application_id": uuid, "environment_id": uuid,
                "kind": {"enum": ["PRINCIPAL_ERASURE", "CONVERSATION_PURGE"]},
                "principal_id": nullable_uuid, "conversation_id": nullable_uuid,
                "status": {"enum": ["PENDING", "COMPLETED", "PARTIALLY_HELD"]},
                "messages_purged": count, "attachments_deleted": count, "held_items": count,
                "created_at": ts, "completed_at": nullable_ts
            }),
        ),
    );
    m.insert(
        "MessagingDeletionRequestPage".into(),
        page("MessagingDeletionRequest"),
    );
    m.insert(
        "MessagingAuditEntry".into(),
        closed(
            "One chat audit entry. Its detail object is not relayed.",
            json!({
                "entry_id": uuid, "occurred_at": ts, "tenant_id": uuid, "application_id": uuid,
                "environment_id": uuid, "credential_id": nullable_uuid, "principal_id": nullable_uuid,
                "action": {"type": "string", "pattern": "^[A-Z][A-Z_]{2,47}$"},
                "target_kind": {"type": "string", "pattern": "^[A-Z][A-Z_]{2,31}$"},
                "target_id": uuid,
                "correlation_id": {"type": "string", "pattern": "^[A-Za-z0-9_-]{8,64}$"}
            }),
        ),
    );
    m.insert("MessagingAuditPage".into(), page("MessagingAuditEntry"));
    m.insert(
        "MessagingSwitch".into(),
        closed(
            "One emergency off-switch (global or per environment). A switch only stops a capability.",
            json!({
                "switch_name": {"enum": ["MESSAGE_SEND", "CONVERSATION_CREATE", "E2EE_ONBOARDING", "ATTACHMENT_UPLOAD", "SMS_BRIDGE", "WEBHOOK_RELAY"]},
                "scope_key": {"type": "string", "maxLength": 64},
                "engaged": {"type": "boolean"},
                "reason": {"type": "string", "maxLength": 64},
                "changed_by": {"type": "string", "maxLength": 64},
                "changed_at": ts, "version": count
            }),
        ),
    );
    m.insert("MessagingSwitchPage".into(), page("MessagingSwitch"));
    m.insert(
        "MessagingIncident".into(),
        closed(
            "One operational incident. Its summary text is not relayed.",
            json!({
                "incident_id": uuid, "category": code,
                "severity": {"enum": ["LOW", "MEDIUM", "HIGH", "CRITICAL"]},
                "status": {"enum": ["OPEN", "RESOLVED"]},
                "occurrences": {"type": "integer", "minimum": 1},
                "first_seen_at": ts, "last_seen_at": ts, "resolved_at": nullable_ts,
                "resolved_by": {"type": ["string", "null"], "maxLength": 64}
            }),
        ),
    );
    m.insert("MessagingIncidentPage".into(), page("MessagingIncident"));
    m.insert(
        "MessagingFailedMessage".into(),
        closed(
            "One outbox event in DEAD_LETTER. Its payload and error string are not relayed.",
            json!({
                "event_id": uuid, "sequence": {"type": "integer", "minimum": 1},
                "event_type": {"type": "string", "pattern": "^messaging(\\.[a-z][a-z0-9-]*){2,}$", "maxLength": 80},
                "aggregate_type": {"type": "string", "pattern": "^[A-Z][A-Z_]{1,39}$"},
                "aggregate_id": uuid, "tenant_id": uuid, "environment_id": uuid,
                "attempts": count, "occurred_at": ts
            }),
        ),
    );
    m.insert(
        "MessagingFailedMessagePage".into(),
        page("MessagingFailedMessage"),
    );
    let head = closed(
        "The key-transparency tree head.",
        json!({"tree_size": count, "root_hash": hex, "updated_at": ts}),
    );
    let witness = closed(
        "One witness's last verified state.",
        json!({
            "witness_id": {"type": "string", "pattern": "^[a-z0-9][a-z0-9.:-]{2,63}$"},
            "tree_size": count, "root_hash": hex, "updated_at": ts, "last_cosigned_size": nullable_count
        }),
    );
    m.insert(
        "MessagingKeyTransparency".into(),
        closed(
            "Key-transparency status: the tree head (null before the first leaf) and each witness state. No key material.",
            json!({
                "head": {"anyOf": [{"type": "null"}, head]},
                "witnesses": {"type": "array", "maxItems": 32, "items": witness}
            }),
        ),
    );
}

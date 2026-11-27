//! Closed result schemas of the Messaging reads (A2.6). Identifiers, categories, states,
//! counts and timestamps only: never message content, ciphertext, MLS state, keys,
//! attachments, report evidence, audit details, outbox payloads, export object keys,
//! legal case references or free text. No property name is on HQ's personal-data list.

use serde_json::{json, Map, Value};

use super::model::CURSOR_PATTERN;
use super::owner_commands::{o, Out, OutField};

// ---- The effective chat policy of an environment (flags, limits, enums) ----------------
// Described as data: the translation (`routes::messaging`) and the schema come from the
// same tables, so they cannot drift apart. The owner's override document is not relayed.

/// One context class's effective policy (`context_class` is added by the translation).
pub const POLICY_CONTEXT: &[OutField] = &[
    o("enabled", "enabled", Out::Bool, false),
    o("allowed_profiles", "allowedProfiles", Out::StrList, false),
    o("default_profile", "defaultProfile", Out::Code, true),
    o("max_members", "maxMembers", Out::Int, true),
    o("max_lifetime_seconds", "maxLifetimeSeconds", Out::Int, true),
    o("reopen_allowed", "reopenAllowed", Out::Bool, false),
    o("legal_hold_eligible", "legalHoldEligible", Out::Bool, false),
    o("sms_bridge_eligible", "smsBridgeEligible", Out::Bool, true),
    o("regulated", "regulated", Out::Bool, false),
    o(
        "central_archive_required",
        "centralArchiveRequired",
        Out::Bool,
        false,
    ),
    o("reporting_enabled", "reportingEnabled", Out::Bool, false),
    o("exports_enabled", "exportsEnabled", Out::Bool, false),
    o("attachments_enabled", "attachmentsEnabled", Out::Bool, true),
    o("retention_days", "retentionDays", Out::Int, true),
    o(
        "requests_require_acceptance",
        "requestsRequireAcceptance",
        Out::Bool,
        true,
    ),
];
const POLICY_MESSAGES: &[OutField] = &[
    o("replies", "replies", Out::Bool, false),
    o("threads", "threads", Out::Bool, false),
    o("reactions", "reactions", Out::Bool, false),
    o("pins", "pins", Out::Bool, false),
    o("forwarding", "forwarding", Out::Bool, true),
    o(
        "forward_shows_original_sender",
        "forwardShowsOriginalSender",
        Out::Bool,
        true,
    ),
];
const POLICY_ATTACHMENTS: &[OutField] = &[
    o("enabled", "enabled", Out::Bool, false),
    o("max_bytes", "maxBytes", Out::Int, false),
    o("max_per_message", "maxPerMessage", Out::Int, false),
    o("allowed_kinds", "allowedKinds", Out::StrList, false),
    o(
        "managed_scan_required",
        "managedScanRequired",
        Out::Bool,
        false,
    ),
    o(
        "max_voice_note_seconds",
        "maxVoiceNoteSeconds",
        Out::Int,
        false,
    ),
    o("max_video_seconds", "maxVideoSeconds", Out::Int, false),
    o("storage_budget_bytes", "storageBudgetBytes", Out::Int, true),
    o("daily_upload_bytes", "dailyUploadBytes", Out::Int, true),
    o(
        "max_concurrent_uploads",
        "maxConcurrentUploads",
        Out::Int,
        false,
    ),
];
const POLICY_PRIVACY: &[OutField] = &[
    o("presence", "presence", Out::Bool, false),
    o("typing_indicators", "typingIndicators", Out::Bool, false),
    o("read_receipts", "readReceipts", Out::Bool, false),
    o("last_seen", "lastSeen", Out::Bool, false),
    o("delivery_receipts", "deliveryReceipts", Out::Bool, false),
];
const POLICY_RETENTION: &[OutField] = &[
    o("message_days", "messageDays", Out::Int, true),
    o(
        "deleted_purge_grace_seconds",
        "deletedPurgeGraceSeconds",
        Out::Int,
        false,
    ),
];
const POLICY_MODERATION: &[OutField] = &[
    o("enabled", "enabled", Out::Bool, false),
    o("reports_enabled", "reportsEnabled", Out::Bool, false),
    o(
        "managed_evidence_allowed",
        "managedEvidenceAllowed",
        Out::Bool,
        false,
    ),
];
const POLICY_EDITING: &[OutField] = &[
    o("edit_window_seconds", "editWindowSeconds", Out::Int, true),
    o(
        "delete_window_seconds",
        "deleteWindowSeconds",
        Out::Int,
        true,
    ),
    o(
        "revision_history_visible",
        "revisionHistoryVisible",
        Out::Bool,
        false,
    ),
];
const POLICY_ABUSE: &[OutField] = &[
    o("messages_per_minute", "messagesPerMinute", Out::Int, false),
    o(
        "new_direct_contacts_per_hour",
        "newDirectContactsPerHour",
        Out::Int,
        false,
    ),
    o("reports_per_hour", "reportsPerHour", Out::Int, false),
    o("uploads_per_hour", "uploadsPerHour", Out::Int, false),
    o(
        "managed_links_per_hour",
        "managedLinksPerHour",
        Out::Int,
        false,
    ),
];
const POLICY_BACKUPS: &[OutField] = &[
    o("enabled", "enabled", Out::Bool, false),
    o("max_bytes", "maxBytes", Out::Int, false),
    o("retain_count", "retainCount", Out::Int, false),
];
/// The effective policy without its contexts (translated separately into a list).
pub const EFFECTIVE_POLICY: &[OutField] = &[
    o("schema_version", "schemaVersion", Out::Int, false),
    o(
        "platform_policy_version",
        "platformPolicyVersion",
        Out::Int,
        false,
    ),
    o(
        "application_policy_version",
        "applicationPolicyVersion",
        Out::Int,
        false,
    ),
    o("sending_enabled", "sendingEnabled", Out::Bool, false),
    o("messages", "messages", Out::Obj(POLICY_MESSAGES), true),
    o(
        "attachments",
        "attachments",
        Out::Obj(POLICY_ATTACHMENTS),
        true,
    ),
    o("privacy", "privacy", Out::Obj(POLICY_PRIVACY), true),
    o("retention", "retention", Out::Obj(POLICY_RETENTION), true),
    o(
        "moderation",
        "moderation",
        Out::Obj(POLICY_MODERATION),
        true,
    ),
    o("editing", "editing", Out::Obj(POLICY_EDITING), true),
    o("abuse", "abuse", Out::Obj(POLICY_ABUSE), true),
    o("backups", "backups", Out::Obj(POLICY_BACKUPS), false),
];
/// Context classes (the keys of the owner's `contexts` map).
pub const CONTEXT_CLASSES: &[&str] = &[
    "DIRECT",
    "GROUP",
    "MEETING",
    "TRANSACTION",
    "CONSULTATION",
    "SUPPORT",
    "ENTERPRISE",
    "DEVELOPER_DEFINED",
];

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

    m.insert(
        "MessagingConversation".into(),
        closed(
            "One conversation: ids, context class, security profile, lifecycle state, version and times. No title, no members, no content.",
            json!({
                "conversation_id": uuid, "tenant_id": uuid, "application_id": uuid, "environment_id": uuid,
                "context_class": {"enum": CONTEXT_CLASSES},
                "security_profile": {"enum": ["MANAGED", "PRIVATE_E2EE"]},
                "lifecycle_state": {"enum": ["ACTIVE", "LOCKED", "CLOSED", "ARCHIVED", "EXPIRED", "DELETED"]},
                "version": {"type": "integer", "minimum": 1, "description": "The conversation's version (expected_version of messaging.conversation.restrict)."},
                "created_at": ts, "state_changed_at": ts
            }),
        ),
    );
    m.insert(
        "MessagingMember".into(),
        closed(
            "One membership: role, state, send restriction, version and time.",
            json!({
                "conversation_id": uuid, "principal_id": uuid, "role": code,
                "member_state": {"enum": ["ACTIVE", "REMOVED", "LEFT"]},
                "send_restricted_until": nullable_ts,
                "version": {"type": "integer", "minimum": 1, "description": "The membership's version (expected_version of messaging.member.mute / remove)."},
                "updated_at": ts
            }),
        ),
    );
    let mut context = super::schemas_commands::object(POLICY_CONTEXT);
    context["properties"]["context_class"] = json!({"enum": CONTEXT_CLASSES});
    context["required"]
        .as_array_mut()
        .expect("required")
        .insert(0, json!("context_class"));
    let mut effective = super::schemas_commands::object(EFFECTIVE_POLICY);
    effective["properties"]["contexts"] = json!({"type": "array", "maxItems": 8, "items": context});
    effective["required"]
        .as_array_mut()
        .expect("required")
        .push(json!("contexts"));
    effective["description"] =
        json!("The policy in force (flags, limits, enums; no personal data).");
    m.insert(
        "MessagingEnvironmentPolicy".into(),
        closed(
            "An environment's chat policy as it applies: the override version (expected_version of messaging.policy.change; 0 while none is stored), the effective policy and its digest. The override document itself is not relayed.",
            json!({
                "tenant_id": uuid, "application_id": uuid, "environment_id": uuid,
                "version": count, "digest": hex, "platform_policy_version": count, "effective": effective
            }),
        ),
    );
}

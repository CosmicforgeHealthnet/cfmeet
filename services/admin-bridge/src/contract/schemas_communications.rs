//! Closed result schemas of the Communications reads (A2.4). Identifiers, display name (R1
//! detail only), reasons, states, counts and timestamps: never a phone number, email
//! address, account reference or report text. No property name is on HQ's personal-data
//! list.
//!
//! `Summary` and `Scope` are HQ Track B's components, published VERBATIM (copied from
//! centralize-admin `services/cfmeet-admin/contract-draft/.../openapi.json`): HQ's report
//! code consumes them as-is. Never edit them here; agree a change with HQ first.

use serde_json::{json, Map, Value};

use super::model::CURSOR_PATTERN;

/// HQ's `Summary` component, exactly as HQ publishes it.
pub const HQ_SUMMARY: &str = r##"{"type":"object","additionalProperties":false,"required":["contract_version","scope","as_of","coverage","window","cells"],"properties":{"contract_version":{"const":"cfmeet.admin-summary.v1"},"scope":{"$ref":"#/components/schemas/Scope"},"as_of":{"type":"string","format":"date-time"},"coverage":{"enum":["COMPLETE","PARTIAL"]},"window":{"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,"required":["start","end"],"properties":{"start":{"type":"string","format":"date-time"},"end":{"type":"string","format":"date-time"}}}]},"cells":{"type":"array","maxItems":200,"items":{"type":"object","additionalProperties":false,"required":["key","value","unit","currency","suppression"],"properties":{"key":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$","maxLength":128},"value":{"type":["integer","null"]},"unit":{"enum":["count","minor_currency","seconds"]},"currency":{"type":["string","null"],"pattern":"^[A-Z]{3}$"},"suppression":{"enum":["NONE","SMALL_CELL","PARTIAL_COVERAGE"]}}}}}}"##;
/// HQ's `Scope` component, exactly as HQ publishes it.
pub const HQ_SCOPE: &str = r##"{"type":"object","additionalProperties":false,"required":["product","market","organization","zone","tenant"],"properties":{"product":{"const":"cfmeet"},"market":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$","maxLength":128},"organization":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$","maxLength":128},"zone":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$","maxLength":128},"tenant":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$","maxLength":128}}}"##;

pub fn insert(m: &mut Map<String, Value>) {
    m.insert(
        "Summary".into(),
        serde_json::from_str(HQ_SUMMARY).expect("HQ Summary is JSON"),
    );
    m.insert(
        "Scope".into(),
        serde_json::from_str(HQ_SCOPE).expect("HQ Scope is JSON"),
    );

    let uuid = json!({"type": "string", "format": "uuid"});
    let nullable_uuid = json!({"type": ["string", "null"], "format": "uuid"});
    let ts = json!({"type": "string", "format": "date-time"});
    let nullable_ts = json!({"type": ["string", "null"], "format": "date-time"});
    let cfmeet_id = json!({"type": "string", "pattern": "^CF-[A-Z2-7]{6,26}$"});
    let nullable_cfmeet_id = json!({"type": ["string", "null"], "pattern": "^CF-[A-Z2-7]{6,26}$"});
    let count = json!({"type": "integer", "minimum": 0});
    let hold_reason = json!({"enum": ["ABUSE", "SECURITY", "LEGAL", "OTHER"]});
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
    let bare = |props: Value| {
        let required: Vec<String> = props.as_object().expect("object").keys().cloned().collect();
        json!({"type": "object", "additionalProperties": false, "required": required, "properties": props})
    };

    m.insert(
        "CommunicationProfile".into(),
        object(
            "One Communications profile by CF Meet ID: display name (R1 detail read), activation state, operator hold and times. Never a phone number, email address or account reference.",
            json!({
                "cfmeet_id": cfmeet_id,
                "display_name": {"type": "string", "maxLength": 160},
                "activation_state": {"enum": ["ACCOUNT_CREATED", "ACTIVE", "SUSPENDED", "DEACTIVATED"]},
                "hold": {"anyOf": [{"type": "null"}, bare(json!({"reason": hold_reason, "active": {"type": "boolean"}, "updated_at": ts}))]},
                "created_at": ts,
                "updated_at": ts,
                "suspended_at": nullable_ts,
                "deactivated_at": nullable_ts,
                "version": {"type": "integer", "minimum": 1}
            }),
        ),
    );
    m.insert(
        "CommunicationReport".into(),
        object(
            "One profile report (safety evidence): reason, both parties' current CF Meet IDs, the target's report count and the review. Reports carry no free text.",
            json!({
                "report_id": uuid,
                "reason": {"enum": ["SPAM", "HARASSMENT", "IMPERSONATION", "OTHER"]},
                "reporter_cfmeet_id": nullable_cfmeet_id,
                "target_cfmeet_id": nullable_cfmeet_id,
                "target_report_count": count,
                "version": {"type": "integer", "minimum": 1, "description": "1 while open, 2 once reviewed: communications.report.review's expected_version."},
                "review": {"anyOf": [{"type": "null"}, bare(json!({
                    "decision": {"enum": ["DISMISSED", "ACTIONED"]},
                    "reason": {"enum": ["NO_VIOLATION", "ABUSE", "SECURITY", "LEGAL", "OTHER"]},
                    "held": {"type": "boolean"},
                    "reviewed_at": ts
                }))]},
                "created_at": ts
            }),
        ),
    );
    m.insert(
        "CommunicationReportPage".into(),
        page("CommunicationReport"),
    );
    m.insert(
        "CommunicationHold".into(),
        object(
            "One operator hold on a profile.",
            json!({"cfmeet_id": nullable_cfmeet_id, "reason": hold_reason, "active": {"type": "boolean"}, "version": {"type": "integer", "minimum": 1}, "updated_at": ts}),
        ),
    );
    m.insert("CommunicationHoldPage".into(), page("CommunicationHold"));
    m.insert(
        "CommunicationAuditEvent".into(),
        object(
            "One Communications Hub audit event: operation, actor and resource ids, time. The free-form details are not relayed.",
            json!({
                "sequence": {"type": "integer", "minimum": 1},
                "operation": {"type": "string", "pattern": "^[A-Za-z][A-Za-z0-9_.:-]{0,79}$"},
                "actor_profile_id": nullable_uuid,
                "resource_id": nullable_uuid,
                "occurred_at": ts
            }),
        ),
    );
    m.insert(
        "CommunicationAuditPage".into(),
        page("CommunicationAuditEvent"),
    );
}

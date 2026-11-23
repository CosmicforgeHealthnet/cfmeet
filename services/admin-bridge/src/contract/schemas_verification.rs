//! Closed schemas of verification (A3): HQ's shared case shape
//! (`cfmeet-admin-parallel-workplan.md` §3.6) plus `approvals_required` and
//! `partner_tenant_id`, its detail, history, the command bodies and the command result.
//! Evidence is hashes only; capability names are product ids. No property name is on HQ's
//! personal-data list.

use serde_json::{json, Map, Value};

use super::model::CURSOR_PATTERN;

pub const CASE_ID_PATTERN: &str =
    "^cfmeet:(PARTNER_GO_LIVE|APP_REVIEW):[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

pub fn insert(m: &mut Map<String, Value>) {
    let uuid = json!({"type": "string", "format": "uuid"});
    let ts = json!({"type": "string", "format": "date-time"});
    let state = json!({"enum": ["OPEN", "CLAIMED", "APPROVED", "REJECTED", "PUBLISHED", "SUSPENDED", "CANCELLED"]});
    let nullable_state = json!({"enum": ["OPEN", "CLAIMED", "APPROVED", "REJECTED", "PUBLISHED", "SUSPENDED", "CANCELLED", null]});
    let staff = json!({"type": ["string", "null"], "pattern": "^(hq:[A-Za-z0-9._:@-]{1,128}|operator:[A-Za-z0-9][A-Za-z0-9._@-]{0,99})$", "maxLength": 132});
    let capability = json!({"type": "string", "pattern": "^[A-Z][A-Z0-9_]{1,63}$"});
    let capabilities = |min: u32| json!({"type": "array", "minItems": min, "maxItems": 16, "uniqueItems": true, "items": capability});
    let cursor = json!({"type": ["string", "null"], "pattern": CURSOR_PATTERN});
    let case_props = json!({
        "case_id": {"type": "string", "pattern": CASE_ID_PATTERN, "maxLength": 128},
        "product": {"const": "cfmeet"},
        "kind": {"enum": ["PARTNER_GO_LIVE", "APP_REVIEW"]},
        "subject_ref": uuid,
        "state": state,
        "version": {"type": "integer", "minimum": 1},
        "assignee_ref": staff,
        "submitted_at": ts,
        "updated_at": ts,
        "evidence": {
            "type": "array",
            "maxItems": 4,
            "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "ref", "sha256"],
                "properties": {
                    "kind": {"enum": ["USE_CASE_SUMMARY", "APP_MANIFEST"]},
                    "ref": {"type": "string", "pattern": "^[0-9a-f-]{36}:[a-z0-9-]{1,16}$", "maxLength": 53},
                    "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"}
                }
            }
        },
        "approvals_required": {"const": 2},
        "partner_tenant_id": uuid,
    });
    let case_required: Vec<&str> = vec![
        "case_id",
        "product",
        "kind",
        "subject_ref",
        "state",
        "version",
        "assignee_ref",
        "submitted_at",
        "updated_at",
        "evidence",
        "approvals_required",
        "partner_tenant_id",
    ];
    m.insert(
        "VerificationCase".into(),
        json!({
            "description": "One verification case (HQ's shared shape). PARTNER_GO_LIVE: a production-access request (subject_ref: the project); APP_REVIEW: an app, published from the same case. OPEN, CLAIMED, then APPROVED / REJECTED (PUBLISHED, SUSPENDED); CANCELLED: withdrawn by the partner (never listed, still readable). version increases on every transition and is the commands' expected_version. evidence: hashes only.",
            "type": "object",
            "additionalProperties": false,
            "required": case_required,
            "properties": case_props,
        }),
    );
    m.insert(
        "VerificationCasePage".into(),
        json!({
            "description": "The verification queue, most recently changed first (CANCELLED cases are not listed).",
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "next_cursor"],
            "properties": {
                "items": {"type": "array", "maxItems": 100, "items": {"$ref": "#/components/schemas/VerificationCase"}},
                "next_cursor": cursor,
            }
        }),
    );
    let mut detail_props = case_props.as_object().cloned().unwrap_or_default();
    detail_props.insert(
        "details".into(),
        json!({
            "oneOf": [
                {
                    "description": "PARTNER_GO_LIVE: what is asked for and what was approved (product capability ids).",
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["requested_capabilities", "approved_capabilities"],
                    "properties": {
                        "requested_capabilities": capabilities(0),
                        "approved_capabilities": capabilities(0)
                    }
                },
                {
                    "description": "APP_REVIEW: the manifest version under review and the published one.",
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["app_version", "published_version"],
                    "properties": {
                        "app_version": {"type": "integer", "minimum": 1},
                        "published_version": {"type": ["integer", "null"], "minimum": 1}
                    }
                }
            ]
        }),
    );
    let mut detail_required = case_required.clone();
    detail_required.push("details");
    m.insert(
        "VerificationCaseDetail".into(),
        json!({
            "description": "One verification case with its decision details (verification.cases.read only).",
            "type": "object",
            "additionalProperties": false,
            "required": detail_required,
            "properties": detail_props,
        }),
    );
    m.insert(
        "VerificationCaseHistory".into(),
        json!({
            "description": "A case's transitions, oldest first. STAFF actions name the staff member (hq:<sub>, or operator:<name> for the operator console); a partner's own actions carry no account id.",
            "type": "object",
            "additionalProperties": false,
            "required": ["case_id", "items", "next_cursor"],
            "properties": {
                "case_id": {"type": "string", "pattern": CASE_ID_PATTERN, "maxLength": 128},
                "items": {
                    "type": "array",
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["sequence", "from_state", "to_state", "actor_kind", "actor_ref", "reason_code", "occurred_at"],
                        "properties": {
                            "sequence": {"type": "integer", "minimum": 1},
                            "from_state": nullable_state,
                            "to_state": nullable_state,
                            "actor_kind": {"enum": ["STAFF", "PARTNER", "SYSTEM"]},
                            "actor_ref": staff,
                            "reason_code": {"type": ["string", "null"], "pattern": "^[A-Z][A-Z0-9_]{0,63}$"},
                            "occurred_at": ts
                        }
                    }
                },
                "next_cursor": cursor,
            }
        }),
    );

    // Command bodies: the command core's base fields, expected_version as the case's
    // integer version, plus the family's own fields.
    let base = |extra: Value, required_extra: &[&str]| -> Value {
        let mut props = json!({
            "operation_id": {"type": "string", "format": "uuid", "description": "HQ's command id; equal to the Idempotency-Key header."},
            "expected_version": {"type": "integer", "minimum": 1, "description": "The case version the command applies to (VERSION_CONFLICT otherwise)."},
            "reason_code": {"type": "string", "pattern": "^[A-Z][A-Z0-9_]{1,63}$"},
            "note": {"type": "string", "minLength": 1, "maxLength": 500}
        });
        if let (Some(p), Some(e)) = (props.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                p.insert(k.clone(), v.clone());
            }
        }
        let mut required = vec!["operation_id", "expected_version"];
        required.extend_from_slice(required_extra);
        json!({"type": "object", "additionalProperties": false, "required": required, "properties": props})
    };
    let mut claim = base(json!({}), &[]);
    claim["description"] = json!("Claim a case for review (OPEN to CLAIMED). R2, no approval.");
    m.insert("VerificationClaimRequest".into(), claim);
    let mut approve = base(json!({"approved_capabilities": capabilities(1)}), &[]);
    approve["description"] = json!("Approve a partner's go-live: approved_capabilities is a subset of the requested ones (default: all). Regulated capabilities need the case CLAIMED first. TWO_PERSON.");
    m.insert("PartnerGoLiveApproveRequest".into(), approve);
    let mut reject = base(json!({}), &["reason_code"]);
    reject["description"] = json!("Reject a case with a safe reason code. TWO_PERSON.");
    m.insert("VerificationRejectRequest".into(), reject);
    let mut decide = base(json!({}), &[]);
    decide["description"] = json!(
        "Approve or publish an app, or suspend a live partner (optional reason code). TWO_PERSON."
    );
    m.insert("VerificationDecisionRequest".into(), decide);
    m.insert(
        "VerificationCommandResult".into(),
        json!({
            "description": "A settled verification command: its operation (as bridge.operations.read shows it) and the case after the change.",
            "type": "object",
            "additionalProperties": false,
            "required": ["operation", "resource"],
            "properties": {
                "operation": {"$ref": "#/components/schemas/OperationReadback"},
                "resource": {"$ref": "#/components/schemas/VerificationCaseDetail"}
            }
        }),
    );
}

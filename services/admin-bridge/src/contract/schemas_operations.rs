//! Closed schemas of the HQ event feed (A7) and the operations switch-on requests and
//! release history (A8). Ids, states, versions, commits and times only: no personal data,
//! no logs, no secrets. No property name is on HQ's personal-data list.
//!
//! The release workflow's operations and the event types are published as patterns (the
//! values are checked by the bridge at runtime) so that a new workflow option or event type
//! never changes a published schema.

use serde_json::{json, Map, Value};

use crate::releases::{RUN_CONCLUSIONS, RUN_STATUSES};

pub fn insert(m: &mut Map<String, Value>) {
    let uuid = json!({"type": "string", "format": "uuid"});
    let ts = json!({"type": "string", "format": "date-time"});
    let nullable_ts = json!({"type": ["string", "null"], "format": "date-time"});
    let staff = json!({"type": "string", "pattern": "^hq:[A-Za-z0-9._:@-]{1,128}$", "maxLength": 132});
    let sha = json!({"type": "string", "pattern": "^[0-9a-f]{40}$"});
    let operation = json!({"type": "string", "pattern": "^[a-z][a-z0-9-]{0,63}$"});
    let run_id = json!({"type": ["integer", "null"], "minimum": 1});
    let mut statuses: Vec<Value> = RUN_STATUSES.iter().map(|s| json!(s)).collect();
    statuses.push(Value::Null);
    let mut conclusions: Vec<Value> = RUN_CONCLUSIONS.iter().map(|s| json!(s)).collect();
    conclusions.push(Value::Null);
    let sequence_cursor = json!({"type": ["string", "null"], "pattern": "^[0-9]{1,19}$"});

    m.insert(
        "HqEvent".into(),
        json!({
            "description": "One owner event on the HQ feed: the authenticated producer, the event type (one of verification.case.changed.v1, abuse.report.changed.v1, partner.workspace.changed.v1, app.review.changed.v1, incident.changed.v1, pricing.version.changed.v1, from the producers in event-adapters.v1.json), the subject by kind and opaque id, its state and version (strictly increasing per producer and subject), when it happened at the owner and when the bridge appended it.",
            "type": "object",
            "additionalProperties": false,
            "required": ["sequence", "event_id", "type", "producer", "subject", "state", "version", "occurred_at", "appended_at"],
            "properties": {
                "sequence": {"type": "integer", "minimum": 1},
                "event_id": uuid,
                "type": {"type": "string", "pattern": "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*\\.v[0-9]{1,3}$", "maxLength": 96},
                "producer": {"type": "string", "pattern": "^[a-z][a-z0-9-]{0,31}$"},
                "subject": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "id"],
                    "properties": {
                        "kind": {"type": "string", "pattern": "^[a-z][a-z0-9_]{0,47}$"},
                        "id": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$"}
                    }
                },
                "state": {"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"},
                "version": {"type": "integer", "minimum": 1},
                "occurred_at": ts,
                "appended_at": ts
            }
        }),
    );
    m.insert(
        "HqEventPage".into(),
        json!({
            "description": "The append-only feed in increasing sequence, appended within the window. next_cursor is the last sequence of this page when more follow; pass the last sequence seen as cursor.",
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "next_cursor"],
            "properties": {
                "items": {"type": "array", "maxItems": 100, "items": {"$ref": "#/components/schemas/HqEvent"}},
                "next_cursor": sequence_cursor
            }
        }),
    );
    m.insert(
        "HqEventAckRequest".into(),
        json!({
            "description": "Acknowledge the feed up to sequence for one consumer. Not a command: no Idempotency-Key; repeating it is harmless. A sequence beyond the last appended one is COMMAND_BODY_INVALID.",
            "type": "object",
            "additionalProperties": false,
            "required": ["consumer", "sequence"],
            "properties": {
                "consumer": {"type": "string", "pattern": "^[a-z][a-z0-9._-]{0,63}$"},
                "sequence": {"type": "integer", "minimum": 0}
            }
        }),
    );
    m.insert(
        "HqEventAck".into(),
        json!({
            "description": "The consumer's acknowledged sequence. It never moves backwards: a lower value answers the stored one.",
            "type": "object",
            "additionalProperties": false,
            "required": ["consumer", "acknowledged_sequence"],
            "properties": {
                "consumer": {"type": "string", "pattern": "^[a-z][a-z0-9._-]{0,63}$"},
                "acknowledged_sequence": {"type": "integer", "minimum": 0}
            }
        }),
    );

    m.insert(
        "OperationsActivationRequest".into(),
        json!({
            "description": "Request a feature switch-on: the bridge starts cfmeet-coordinated-release.yml on main with these inputs, once (expected_version 0: a new request; its request_id is the operation_id). operation must be one of the release workflow's options; confirm carries the owner confirmation words where the workflow requires them. TWO_PERSON. The workflow's own gates (exact main head, green CI, confirmations) still apply.",
            "type": "object",
            "additionalProperties": false,
            "required": ["operation_id", "expected_version", "operation", "release_sha"],
            "properties": {
                "operation_id": uuid,
                "expected_version": {"const": 0},
                "operation": operation,
                "release_sha": sha,
                "confirm": {"type": ["string", "null"], "pattern": "^[a-z0-9][a-z0-9=,._:-]{0,127}$"},
                "reason_code": {"type": "string", "pattern": "^[A-Z][A-Z0-9_]{1,63}$"},
                "note": {"type": "string", "minLength": 1, "maxLength": 500}
            }
        }),
    );
    m.insert(
        "OperationsActivation".into(),
        json!({
            "description": "One feature switch-on request. state: REQUESTED (dispatch sent, outcome not yet known), DISPATCHED (GitHub started the workflow), FAILED (no run was started). run_id links the workflow run once it is listed; run_status, run_conclusion and run_updated_at are GitHub's latest view of that run (null when unknown).",
            "type": "object",
            "additionalProperties": false,
            "required": ["request_id", "operation", "release_sha", "confirm", "requested_by", "approver", "state", "created_at", "dispatched_at", "run_id", "run_status", "run_conclusion", "run_updated_at"],
            "properties": {
                "request_id": uuid,
                "operation": operation,
                "release_sha": sha,
                "confirm": {"type": ["string", "null"], "pattern": "^[a-z0-9][a-z0-9=,._:-]{0,127}$"},
                "requested_by": staff,
                "approver": staff,
                "state": {"enum": ["REQUESTED", "DISPATCHED", "FAILED"]},
                "created_at": ts,
                "dispatched_at": nullable_ts,
                "run_id": run_id,
                "run_status": {"enum": statuses},
                "run_conclusion": {"enum": conclusions},
                "run_updated_at": nullable_ts
            }
        }),
    );
    m.insert(
        "OperationsActivationPage".into(),
        json!({
            "description": "Switch-on requests, newest first.",
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "next_cursor"],
            "properties": {
                "items": {"type": "array", "maxItems": 100, "items": {"$ref": "#/components/schemas/OperationsActivation"}},
                "next_cursor": {"type": ["string", "null"], "pattern": "^[0-9]{1,19}\\.[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"}
            }
        }),
    );
    m.insert(
        "OperationsActivationCommandResult".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["operation", "resource"],
            "properties": {
                "operation": {"$ref": "#/components/schemas/OperationReadback"},
                "resource": {"$ref": "#/components/schemas/OperationsActivation"}
            }
        }),
    );
    let run_statuses: Vec<Value> = RUN_STATUSES.iter().map(|s| json!(s)).collect();
    m.insert(
        "OperationsRelease".into(),
        json!({
            "description": "One run of the release workflow: GitHub's run id, the triggering event, the operation when the bridge requested the run (null otherwise: GitHub does not list workflow inputs), the commit, status, conclusion and times. Never logs, inputs, artifacts or anything the workflow printed.",
            "type": "object",
            "additionalProperties": false,
            "required": ["run_id", "event", "operation", "head_sha", "status", "conclusion", "created_at", "updated_at"],
            "properties": {
                "run_id": {"type": "integer", "minimum": 1},
                "event": {"type": "string", "pattern": "^[a-z][a-z_]{0,63}$"},
                "operation": {"type": ["string", "null"], "pattern": "^[a-z][a-z0-9-]{0,63}$"},
                "head_sha": sha,
                "status": {"enum": run_statuses},
                "conclusion": {"enum": conclusions},
                "created_at": ts,
                "updated_at": ts
            }
        }),
    );
    m.insert(
        "OperationsReleasePage".into(),
        json!({
            "description": "Runs of cfmeet-coordinated-release.yml created within the window, newest first. Freshness UNAVAILABLE with no items when the bridge has no GitHub access configured (never read as \"no releases\").",
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "next_cursor"],
            "properties": {
                "items": {"type": "array", "maxItems": 100, "items": {"$ref": "#/components/schemas/OperationsRelease"}},
                "next_cursor": {"type": ["string", "null"], "pattern": "^[0-9]{1,4}$"}
            }
        }),
    );
}

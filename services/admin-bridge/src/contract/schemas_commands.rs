//! Request and result schemas of the owner commands, derived from their rows
//! (`owner_commands::COMMANDS`): the request is the command core's base fields plus the
//! row's body fields; the result is `{operation: OperationReadback, resource}` with the
//! row's closed resource.

use serde_json::{json, Map, Value};

use super::owner_commands::{In, Out, OutField, Place, Reason, COMMANDS};

fn in_schema(kind: In) -> Value {
    match kind {
        In::Enum(values) => json!({"enum": values}),
        In::Bool => json!({"type": "boolean"}),
        In::Int(min, max) => json!({"type": "integer", "minimum": min, "maximum": max}),
        In::Uuid => json!({"type": "string", "format": "uuid"}),
        In::NullableUuid => json!({"type": ["string", "null"], "format": "uuid"}),
        In::Code => json!({"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"}),
        In::Str(min, max) => json!({"type": "string", "minLength": min, "maxLength": max}),
        In::NullableStr(min, max) => {
            json!({"type": ["string", "null"], "minLength": min, "maxLength": max})
        }
        In::UuidList(max) => {
            json!({"type": "array", "maxItems": max, "uniqueItems": true, "items": {"type": "string", "format": "uuid"}})
        }
        In::Object => json!({"type": "object", "description": "Validated by the owning service."}),
        In::EnumList(values, max) => {
            json!({"type": "array", "maxItems": max, "uniqueItems": true, "items": {"enum": values}})
        }
        In::Time => {
            json!({"type": "string", "format": "date-time", "maxLength": 40, "pattern": "Z$"})
        }
        In::Date => {
            json!({"type": "string", "format": "date", "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"})
        }
        In::NullableObject => {
            json!({"type": ["object", "null"], "description": "Validated by the owning service; null clears it."})
        }
    }
}

fn out_schema(f: &OutField) -> Value {
    let mut s = match f.kind {
        Out::Str => json!({"type": "string", "pattern": "^[A-Za-z0-9._:@-]{1,128}$"}),
        Out::Code => json!({"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"}),
        Out::Uuid => json!({"type": "string", "format": "uuid"}),
        Out::Int => json!({"type": "integer"}),
        Out::Bool => json!({"type": "boolean"}),
        Out::Time => json!({"type": "string", "format": "date-time"}),
        Out::Text => json!({"type": "string", "maxLength": 500}),
        Out::StrList => {
            json!({"type": "array", "maxItems": 64, "items": {"type": "string", "pattern": "^[A-Za-z0-9._:@-]{1,128}$"}})
        }
        Out::Obj(inner) => object(inner),
        Out::ObjList(inner) => json!({"type": "array", "maxItems": 100, "items": object(inner)}),
    };
    if f.nullable {
        if let Some(t) = s.get("type").cloned() {
            s["type"] = json!([t, "null"]);
        } else {
            s = json!({"oneOf": [s, {"type": "null"}]});
        }
    }
    s
}

fn object(fields: &[OutField]) -> Value {
    let props: Map<String, Value> = fields
        .iter()
        .map(|f| (f.hq.to_string(), out_schema(f)))
        .collect();
    let required: Vec<&str> = fields.iter().map(|f| f.hq).collect();
    json!({"type": "object", "additionalProperties": false, "required": required, "properties": props})
}

pub fn insert(m: &mut Map<String, Value>) {
    for c in COMMANDS {
        let mut props = json!({
            "operation_id": {"type": "string", "format": "uuid", "description": "HQ's command id; equal to the Idempotency-Key header."},
            "reason_code": {"type": "string", "pattern": "^[A-Z][A-Z0-9_]{1,63}$"},
            "note": {"type": "string", "minLength": 1, "maxLength": 500}
        });
        let mut required = vec!["operation_id"];
        match c.reason {
            Reason::Forward => {}
            Reason::Required => required.push("reason_code"),
            Reason::Ledger => {
                props["reason_code"]["description"] =
                    json!("Kept in the bridge's operation record; not sent to the owning service.");
            }
        }
        if let Some(min) = c.version {
            props["expected_version"] = json!({"type": "integer", "minimum": min, "description": "The resource version the command applies to (VERSION_CONFLICT otherwise); 0 means none yet where allowed."});
            required.push("expected_version");
        }
        for f in c
            .body
            .iter()
            .filter(|f| !matches!(f.place, Place::Fixed(_) | Place::Tenant))
        {
            props[f.hq] = in_schema(f.kind);
            if f.required {
                required.push(f.hq);
            }
        }
        let tenant_note = if c.body.iter().any(|f| f.place == Place::Tenant) {
            " The owner receives the request's tenant scope as its tenant; the body never names a tenant."
        } else {
            ""
        };
        m.insert(
            c.body_schema.into(),
            json!({
                "description": format!("Request of {} ({}).{tenant_note}", c.id, c.source),
                "type": "object",
                "additionalProperties": false,
                "required": required,
                "properties": props,
            }),
        );
        let mut resource = object(c.resource);
        let statuses = if c.statuses.is_empty() {
            "any state code".to_string()
        } else {
            c.statuses.join(", ")
        };
        resource["description"] = json!(format!(
            "The resource after {}; status: {}.",
            c.id, statuses
        ));
        m.insert(
            c.result_schema.into(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["operation", "resource"],
                "properties": {
                    "operation": {"$ref": "#/components/schemas/OperationReadback"},
                    "resource": resource
                }
            }),
        );
    }
}

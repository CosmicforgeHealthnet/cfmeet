//! Closed JSON schemas (`components.schemas`). Only keywords HQ's strict validator
//! implements are used (karryam-admin `contracts/schema.js`): no `$defs`, no
//! `format` other than `date-time`, `date`, `uuid`.
//!
//! A published schema is immutable: a change needs a new component name and a new
//! capability version (`compat.rs` enforces it).

use serde_json::{json, Map, Value};

use super::datasets::DATASETS;
use super::model::{Purpose, State, CURSOR_PATTERN, DATASET_ID_PATTERN};

pub const FRESHNESS: &[&str] = &[
    "LIVE",
    "RECENT",
    "STALE",
    "PARTIAL",
    "UNKNOWN",
    "UNAVAILABLE",
];
pub const ERROR_CODE_PATTERN: &str = "^[A-Z][A-Z0-9_]{1,63}$";

fn names<T: Copy>(all: &[T], f: impl Fn(T) -> &'static str) -> Vec<&'static str> {
    all.iter().map(|v| f(*v)).collect()
}

/// Every component schema, by name.
pub fn components() -> Map<String, Value> {
    let mut m = Map::new();
    let date_time = json!({"type": "string", "format": "date-time"});
    let nullable_date_time = json!({"type": ["string", "null"], "format": "date-time"});
    let nullable_count = json!({"type": ["integer", "null"], "minimum": 0});

    m.insert(
        "Error".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["code", "message"],
            "properties": {
                "code": {"type": "string", "pattern": ERROR_CODE_PATTERN},
                "message": {"type": "string", "maxLength": 256},
                "operation_id": {"type": "string", "format": "uuid"}
            }
        }),
    );

    m.insert(
        "Discovery".into(),
        json!({
            "description": "Runtime capability states of the running bridge. `state` is the artifact state lowered by runtime switches (a served capability whose module or write switch is off is DISABLED with a blocker). openapi_sha256 is the sha256 of the exact openapi.json bytes this build renders; HQ trusts runtime states only when it equals the pinned hash. policy_revision is the pinned, verified access-policy revision.",
            "type": "object",
            "additionalProperties": false,
            "required": ["contract_version", "openapi_sha256", "policy_revision", "capabilities"],
            "properties": {
                "contract_version": {"type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+$"},
                "openapi_sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "policy_revision": {"type": "integer", "minimum": 1},
                "capabilities": {
                    "type": "array",
                    "maxItems": 512,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["id", "state", "blocker"],
                        "properties": {
                            "id": {"type": "string", "pattern": "^[a-z]+(\\.[a-z0-9-]+){1,4}$"},
                            "state": {"enum": names(State::ALL, State::as_str)},
                            "blocker": {"type": ["string", "null"], "maxLength": 256}
                        }
                    }
                }
            }
        }),
    );

    m.insert(
        "Health".into(),
        json!({
            "description": "Aggregate health (HQ shape). services lists the bridge and every owner it is configured to probe; an owner without a probe is omitted (never shown as OK). Overall: DOWN when the bridge's own database is down, DEGRADED when a listed service is not OK, UNKNOWN when no owner probe is configured, otherwise OK.",
            "type": "object",
            "additionalProperties": false,
            "required": ["as_of", "status", "services"],
            "properties": {
                "as_of": {"type": "string", "format": "date-time"},
                "status": {"enum": ["OK", "DEGRADED", "DOWN", "UNKNOWN"]},
                "services": {
                    "type": "array",
                    "maxItems": 64,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["service", "status", "checked_at"],
                        "properties": {
                            "service": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$", "maxLength": 128},
                            "status": {"enum": ["OK", "DEGRADED", "DOWN", "UNKNOWN"]},
                            "checked_at": {"type": "string", "format": "date-time"}
                        }
                    }
                }
            }
        }),
    );

    let metric_names: Vec<&str> = {
        let mut v: Vec<&str> = DATASETS
            .iter()
            .flat_map(|d| d.metrics.iter().copied())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    m.insert(
        "Dataset".into(),
        json!({
            "description": "One certified HQ dataset over a UTC day window. Metric names are fixed per dataset (datasets.v1.json); a metric the bridge could not establish is null, never zero. coverage is COMPLETE only when every source reported every day of the window.",
            "type": "object",
            "additionalProperties": false,
            "required": ["dataset_id", "classification", "coverage", "window", "series", "next_cursor"],
            "properties": {
                "dataset_id": {"type": "string", "pattern": DATASET_ID_PATTERN},
                "classification": {"const": "INTERNAL"},
                "coverage": {"enum": ["COMPLETE", "PARTIAL", "NONE"]},
                "window": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["start", "end"],
                    "properties": {"start": date_time, "end": date_time}
                },
                "series": {
                    "type": "array",
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["date", "metrics"],
                        "properties": {
                            "date": {"type": "string", "format": "date"},
                            "metrics": {
                                "type": "object",
                                "maxProperties": 16,
                                "propertyNames": {"enum": metric_names},
                                "additionalProperties": nullable_count
                            }
                        }
                    }
                },
                "next_cursor": {"type": ["string", "null"], "pattern": CURSOR_PATTERN}
            }
        }),
    );

    m.insert(
        "ProjectionStatus".into(),
        json!({
            "description": "Freshness and coverage of every bridge projection (owner events -> summaries). Until owner events are wired (phase A7) every projection reports UNAVAILABLE.",
            "type": "object",
            "additionalProperties": false,
            "required": ["projections"],
            "properties": {
                "projections": {
                    "type": "array",
                    "maxItems": 64,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["name", "freshness", "coverage", "last_event_at", "lag_seconds", "open_gaps", "quarantined"],
                        "properties": {
                            "name": {"type": "string", "pattern": "^[a-z][a-z0-9_.]{0,63}$"},
                            "freshness": {"enum": FRESHNESS},
                            "coverage": {"enum": ["COMPLETE", "PARTIAL", "NONE"]},
                            "last_event_at": nullable_date_time,
                            "lag_seconds": nullable_count,
                            "open_gaps": nullable_count,
                            "quarantined": nullable_count
                        }
                    }
                }
            }
        }),
    );

    m.insert(
        "Purpose".into(),
        json!({"enum": names(Purpose::ALL, Purpose::as_str)}),
    );
    m
}

/// `result` schema of a capability: its component, or `{"not": {}}` (no 2xx possible
/// in this contract version).
pub fn result_schema(result: Option<&str>) -> Value {
    match result {
        Some(name) => json!({"$ref": format!("#/components/schemas/{name}")}),
        None => json!({"not": {}}),
    }
}

pub fn success_envelope(result: Option<&str>) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["contract_version", "product", "correlation_id", "as_of", "freshness", "result", "error"],
        "properties": {
            "contract_version": {"const": super::model::ENVELOPE_VERSION},
            "product": {"const": super::model::PRODUCT},
            "correlation_id": {"type": "string", "format": "uuid"},
            "as_of": {"type": "string", "format": "date-time"},
            "freshness": {"enum": FRESHNESS},
            "result": result_schema(result),
            "error": {"type": "null"}
        }
    })
}

pub fn error_envelope() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["contract_version", "product", "correlation_id", "as_of", "freshness", "error"],
        "properties": {
            "contract_version": {"const": super::model::ENVELOPE_VERSION},
            "product": {"const": super::model::PRODUCT},
            "correlation_id": {"type": "string", "format": "uuid"},
            "as_of": {"type": "string", "format": "date-time"},
            "freshness": {"enum": FRESHNESS},
            "error": {"$ref": "#/components/schemas/Error"}
        }
    })
}

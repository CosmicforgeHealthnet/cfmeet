//! Offline validation against the registered contracts (packages/contracts).

use std::path::PathBuf;

use jsonschema::{Registry, Resource};
use serde_json::{json, Value};

const BASE: &str = "https://schemas.cfmeet.com/";

fn read(rel: &str) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
    serde_json::from_str(&std::fs::read_to_string(root.join(rel)).unwrap()).unwrap()
}

/// Asserts `value` validates against `reference` (relative to the schema base URI,
/// e.g. `telephony/commands/v1.schema.json#/$defs/OriginateCommand`).
pub fn check(reference: &str, value: &Value) {
    let manifest = read("manifest.json");
    let mut builder = Registry::new();
    for c in manifest["contracts"].as_array().unwrap() {
        let path = c["path"].as_str().unwrap();
        let doc = read(path);
        let id = if c["kind"] == "json-schema" {
            doc["$id"].as_str().unwrap().to_string()
        } else {
            format!("{BASE}{path}")
        };
        builder = builder.add(id, Resource::from_contents(doc)).unwrap();
    }
    let registry = builder.prepare().unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("{BASE}{reference}"),
        }))
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{reference}: {errors:?}\n{value:#}");
}

/// Asserts `value` validates against the Calling HTTP contract's component `name`.
#[allow(dead_code)]
pub fn check_openapi_component(name: &str, value: &Value) {
    check(
        &format!("calling/http/v1.openapi.json#/components/schemas/{name}"),
        value,
    );
}

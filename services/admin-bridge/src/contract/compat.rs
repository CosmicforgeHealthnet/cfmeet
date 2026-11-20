//! Breaking-change gate: compares a newly rendered contract with a base (the copy on
//! `main`). Allowed without a major version bump: new capabilities, state / blocker /
//! tests / source_endpoint changes, a result or body schema defined for the first time
//! (from `{"not":{}}`), new component schemas. Everything else is breaking.

use serde_json::Value;

fn major(v: &Value) -> Option<u64> {
    v.get("contract_version")?
        .as_str()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn not_defined(v: &Value) -> bool {
    v.is_null() || *v == serde_json::json!({"not": {}})
}

/// Fields of a capability that HQ builds requests or authorization decisions from.
const FROZEN: &[&str] = &[
    "version",
    "module",
    "kind",
    "method",
    "path",
    "path_params",
    "query_shape",
    "query_required",
    "permission",
    "risk",
    "approval",
    "source_version_required",
    "idempotency_required",
    "success_status",
    "success_media",
];

/// Returns every breaking difference (empty: compatible). `base_*` / `new_*` are the
/// parsed `capabilities.v1.json` and `bridge-schemas.v1.json`.
pub fn breaking_changes(
    base_caps: &Value,
    base_schemas: &Value,
    new_caps: &Value,
    new_schemas: &Value,
) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(b), Some(n)) = (major(base_caps), major(new_caps)) {
        if n > b {
            return out; // explicit major bump: breaking changes are announced
        }
    }
    let list = |v: &Value| {
        v.get("capabilities")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let new_list = list(new_caps);
    for b in list(base_caps) {
        let id = b["id"].as_str().unwrap_or_default().to_string();
        let Some(n) = new_list.iter().find(|n| n["id"] == b["id"]) else {
            out.push(format!("{id}: capability removed"));
            continue;
        };
        for field in FROZEN {
            if b.get(*field) != n.get(*field) {
                out.push(format!("{id}: `{field}` changed"));
            }
        }
        if b["purposes"].as_array().is_some_and(|bp| {
            bp.iter()
                .any(|p| !n["purposes"].as_array().is_some_and(|np| np.contains(p)))
        }) {
            out.push(format!("{id}: a purpose was removed"));
        }
        for field in ["result_schema", "request_body"] {
            if !not_defined(&b[field]) && b[field] != n[field] {
                out.push(format!("{id}: published `{field}` changed"));
            }
        }
    }
    let comps = |v: &Value| {
        v.pointer("/components/schemas")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let new_comps = comps(new_schemas);
    for (name, schema) in comps(base_schemas) {
        match new_comps.get(&name) {
            None => out.push(format!("schema {name}: removed")),
            Some(s) if *s != schema => out.push(format!(
                "schema {name}: published schema changed (add a new name instead)"
            )),
            _ => {}
        }
    }
    out
}

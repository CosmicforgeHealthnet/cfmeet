//! Part J contract helpers: validate runtime JSON against the generated OpenAPI
//! document, and raw requests that expose response headers (request IDs).
#![allow(dead_code)]

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt;

use super::TestApp;

pub fn openapi() -> Value {
    serde_json::from_str(&backend::swagger::openapi_json()).unwrap()
}

/// Validate `value` against `#/components/schemas/<name>`: types, enums, required
/// fields, nullability, and — strictly — that the runtime JSON carries no property the
/// contract does not document. Returns every violation (empty = conforms).
pub fn validate(doc: &Value, name: &str, value: &Value) -> Vec<String> {
    let mut errors = vec![];
    let schema = json_ref(doc, &format!("#/components/schemas/{name}"));
    check(doc, &schema, value, name, &mut errors);
    errors
}

pub fn assert_conforms(doc: &Value, name: &str, value: &Value) {
    let errors = validate(doc, name, value);
    assert!(
        errors.is_empty(),
        "runtime JSON does not match OpenAPI schema {name}: {errors:#?}\n{value:#}"
    );
}

fn json_ref(doc: &Value, r: &str) -> Value {
    let name = r
        .strip_prefix("#/components/schemas/")
        .unwrap_or_else(|| panic!("unsupported ref {r}"));
    doc["components"]["schemas"]
        .get(name)
        .unwrap_or_else(|| panic!("dangling ref {r}"))
        .clone()
}

fn check(doc: &Value, schema: &Value, value: &Value, at: &str, errors: &mut Vec<String>) {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        return check(doc, &json_ref(doc, r), value, at, errors);
    }
    if value.is_null() {
        let nullable = schema.get("nullable").and_then(Value::as_bool) == Some(true)
            || schema
                .get("oneOf")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .any(|s| s.get("type") == Some(&Value::from("null")))
                })
                .unwrap_or(false);
        if !nullable {
            errors.push(format!("{at}: null but not nullable"));
        }
        return;
    }
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        for s in all {
            check(doc, s, value, at, errors);
        }
        return;
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(options) = schema.get(key).and_then(Value::as_array) {
            let ok = options.iter().any(|s| {
                let mut e = vec![];
                check(doc, s, value, at, &mut e);
                e.is_empty()
            });
            if !ok {
                errors.push(format!("{at}: matches no {key} option"));
            }
            return;
        }
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(value) {
            errors.push(format!("{at}: {value} not in enum {values:?}"));
        }
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") if !value.is_string() => errors.push(format!("{at}: expected string")),
        Some("boolean") if !value.is_boolean() => errors.push(format!("{at}: expected boolean")),
        Some("integer") if !(value.is_i64() || value.is_u64()) => {
            errors.push(format!("{at}: expected integer, got {value}"))
        }
        Some("number") if !value.is_number() => errors.push(format!("{at}: expected number")),
        Some("array") => match value.as_array() {
            None => errors.push(format!("{at}: expected array")),
            Some(items) => {
                if let Some(item_schema) = schema.get("items") {
                    for (i, item) in items.iter().enumerate() {
                        check(doc, item_schema, item, &format!("{at}[{i}]"), errors);
                    }
                }
            }
        },
        Some("object") | None if schema.get("properties").is_some() => {
            let Some(obj) = value.as_object() else {
                errors.push(format!("{at}: expected object"));
                return;
            };
            let props = schema["properties"].as_object().unwrap();
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for r in required {
                    let r = r.as_str().unwrap();
                    if !obj.contains_key(r) {
                        errors.push(format!("{at}.{r}: required but missing"));
                    }
                }
            }
            for (k, v) in obj {
                match props.get(k) {
                    Some(s) => check(doc, s, v, &format!("{at}.{k}"), errors),
                    None if schema.get("additionalProperties").is_some() => {}
                    None => errors.push(format!("{at}.{k}: undocumented property")),
                }
            }
        }
        _ => {}
    }
}

/// Raw request returning status, headers and JSON.
pub async fn raw(
    app: &TestApp,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }))
    };
    (status, headers, value)
}

/// The standard public error contract: `{ success:false, error:{ code, message,
/// requestId } }`, code from the published `ErrorCode` enum, requestId equal to the
/// server-generated `x-request-id` header.
pub fn assert_error_contract(doc: &Value, status: StatusCode, headers: &HeaderMap, v: &Value) {
    assert!(
        status.is_client_error() || status.is_server_error(),
        "not an error: {status} {v}"
    );
    assert_conforms(doc, "ErrorEnvelope", v);
    assert_eq!(v["success"], false, "{v}");
    let codes = doc["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .unwrap();
    assert!(
        codes.contains(&v["error"]["code"]),
        "code {} is not a published ErrorCode",
        v["error"]["code"]
    );
    let rid = v["error"]["requestId"].as_str().expect("requestId present");
    assert!(rid.starts_with("req_"), "server-generated requestId: {rid}");
    assert_eq!(
        headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default(),
        rid,
        "body requestId equals x-request-id header"
    );
}

/// Substrings that must never reach a public response, event or contract.
pub fn secret_needles(app: &TestApp) -> Vec<String> {
    let mut needles = vec![
        "test-jwt-secret".to_string(),
        super::livekit_mock::API_SECRET.to_string(),
        super::artifacts::TRANSCRIPTION_SECRET.to_string(),
        "postgres://".to_string(),
        "redis://".to_string(),
        "SELECT ".to_string(),
        "INSERT INTO".to_string(),
        "violates".to_string(),
        "/home/".to_string(),
        "src/".to_string(),
        "panicked".to_string(),
        "stack backtrace".to_string(),
        "twirp".to_string(),
    ];
    if let Some((_, db)) = app.db.url.rsplit_once('/') {
        needles.push(db.to_string());
    }
    needles
}

pub fn assert_no_secrets(app: &TestApp, context: &str, v: &Value) {
    let text = v.to_string();
    for n in secret_needles(app) {
        assert!(
            !text.contains(&n),
            "{context}: public output leaks `{n}`: {text}"
        );
    }
}

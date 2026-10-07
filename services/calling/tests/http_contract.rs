//! Section 12: the Calling service conforms to its canonical HTTP contracts in
//! `packages/contracts` (the authority):
//!
//! * `calling/http/v1.openapi.json` lists exactly the routes the service registers — no
//!   undocumented route, no documented route that does not exist;
//! * real responses of the production router (health, 404, 405) validate against the
//!   contract's schemas, including the request-id header;
//! * the public and internal error-code enumerations equal the Rust ones;
//! * internal authentication failures validate against `platform/internal-http`.

mod common;

use std::{collections::BTreeSet, path::PathBuf};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use calling::{error::ErrorCode, routes};
use common::Harness;
use http_body_util::BodyExt;
use jsonschema::{Registry, Resource, Validator};
use serde_json::{json, Value};
use tower::ServiceExt;

const SCHEMA_BASE_URI: &str = "https://schemas.cfmeet.com/";
const CALLING_HTTP: &str = "calling/http/v1.openapi.json";
const INTERNAL_HTTP: &str = "platform/internal-http/v1.openapi.json";

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts")
}

fn read(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(contracts().join(rel)).unwrap()).unwrap()
}

/// Offline registry: every registered JSON Schema contract by `$id`, plus the two
/// OpenAPI documents at their canonical URIs.
fn registry() -> Registry<'static> {
    let manifest = read("manifest.json");
    let mut builder = Registry::new();
    for c in manifest["contracts"].as_array().unwrap() {
        if c["kind"] == "json-schema" {
            let doc = read(c["path"].as_str().unwrap());
            let id = doc["$id"].as_str().unwrap().to_string();
            builder = builder.add(id, Resource::from_contents(doc)).unwrap();
        }
    }
    for openapi in [CALLING_HTTP, INTERNAL_HTTP] {
        builder = builder
            .add(
                format!("{SCHEMA_BASE_URI}{openapi}"),
                Resource::from_contents(read(openapi)),
            )
            .unwrap();
    }
    builder.prepare().unwrap()
}

fn component(openapi: &str, pointer: &str) -> Validator {
    let registry = registry();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("{SCHEMA_BASE_URI}{openapi}#{pointer}"),
        }))
        .unwrap()
}

fn assert_valid(validator: &Validator, value: &Value, what: &str) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{what} violates the contract: {errors:?}\n{value:#}"
    );
}

fn string_enum(openapi: &str, pointer: &str) -> Vec<String> {
    read(openapi)
        .pointer(pointer)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// The router exactly as `main` builds it: no customer business routes and no internal
/// routes are registered yet.
fn production_router(h: &Harness) -> Router {
    routes::app(h.state.clone(), Router::new(), Router::new())
}

async fn send(
    router: Router,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let response = router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn rust_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn contract_paths_equal_the_registered_routes() {
    let mut files = Vec::new();
    rust_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut registered = BTreeSet::new();
    for file in files {
        let code = std::fs::read_to_string(&file).unwrap();
        for (i, _) in code.match_indices(".route(") {
            let rest = code[i + ".route(".len()..].trim_start();
            let literal = rest
                .strip_prefix('"')
                .and_then(|r| r.split('"').next())
                .unwrap_or_else(|| panic!("{}: non-literal route path", file.display()));
            registered.insert(literal.to_string());
        }
        assert!(
            !code.contains(".route_service(") && !code.contains(".nest_service("),
            "{}: service routes bypass the contract check",
            file.display()
        );
    }
    let documented: BTreeSet<String> = read(CALLING_HTTP)["paths"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        registered, documented,
        "Calling routes and its OpenAPI contract disagree"
    );
}

#[tokio::test]
async fn real_responses_validate_against_the_calling_contract() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let health = component(CALLING_HTTP, "/components/schemas/HealthStatus");
    let envelope = component(CALLING_HTTP, "/components/schemas/ErrorEnvelope");
    let request_id = component(CALLING_HTTP, "/components/headers/XRequestId/schema");
    let client_request_id = component(CALLING_HTTP, "/components/headers/XClientRequestId/schema");

    let (status, headers, body) = send(
        production_router(&h),
        Method::GET,
        "/health",
        &[("x-request-id", "client-7")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_valid(&health, &body, "GET /health");
    assert_valid(
        &request_id,
        &json!(headers["x-request-id"].to_str().unwrap()),
        "x-request-id",
    );
    assert_valid(
        &client_request_id,
        &json!(headers["x-client-request-id"].to_str().unwrap()),
        "x-client-request-id",
    );

    for (method, path, expected, code) in [
        (
            Method::GET,
            "/does-not-exist",
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
        ),
        (
            Method::GET,
            "/api/v1/calls",
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
        ),
        (
            Method::POST,
            "/health",
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
        ),
        (
            Method::DELETE,
            "/health",
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
        ),
    ] {
        let (status, headers, body) = send(production_router(&h), method.clone(), path, &[]).await;
        assert_eq!(status, expected, "{method} {path}");
        assert_valid(&envelope, &body, &format!("{method} {path}"));
        assert_eq!(body["error"]["code"], code);
        assert_eq!(
            headers["x-request-id"].to_str().unwrap(),
            body["error"]["requestId"]
        );
    }

    // The production router has no internal routes; unknown internal paths still require
    // a service token and answer with the internal contract's envelope.
    let internal = component(INTERNAL_HTTP, "/components/schemas/InternalErrorEnvelope");
    let (status, _, body) = send(
        production_router(&h),
        Method::GET,
        "/internal/v1/anything",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_valid(&internal, &body, "unauthenticated internal request");
}

#[test]
fn error_code_enumerations_equal_the_rust_codes() {
    let public: Vec<&str> = ErrorCode::ALL
        .iter()
        .filter(|c| !c.is_internal())
        .map(|c| c.as_str())
        .collect();
    assert_eq!(
        public,
        string_enum(CALLING_HTTP, "/components/schemas/ErrorCode/enum")
    );
    let internal: Vec<&str> = ErrorCode::ALL
        .iter()
        .filter(|c| c.is_internal())
        .map(|c| c.as_str())
        .collect();
    assert_eq!(
        internal,
        string_enum(INTERNAL_HTTP, "/components/schemas/InternalErrorCode/enum")
    );
}

#[test]
fn contract_versions_match_the_manifest() {
    let manifest = read("manifest.json");
    for (id, path) in [
        ("cfmeet.calling.http-api", CALLING_HTTP),
        ("cfmeet.platform.internal-http", INTERNAL_HTTP),
    ] {
        let entry = manifest["contracts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("{id} is not registered"));
        assert_eq!(entry["path"], path);
        assert_eq!(read(path)["info"]["version"], entry["version"]);
    }
}

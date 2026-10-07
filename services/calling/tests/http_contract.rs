//! Section 12: the Calling service conforms to its canonical HTTP contracts in
//! `packages/contracts` (the authority):
//!
//! * `calling/http/v1.openapi.json` lists exactly the routes the service registers — no
//!   undocumented route, no documented route that does not exist;
//! * real responses of the production router (health, the call API, 401 / 403 / 404 /
//!   400 / 405) validate against the contract's schemas, including the request-id header;
//! * the public and internal error-code enumerations equal the Rust ones;
//! * the call-domain vocabularies (`calling/common`) equal the Rust enumerations;
//! * every outbox row is a valid `cfmeet.platform.event-envelope` whose payload is a valid
//!   `cfmeet.calling.call-lifecycle-event`;
//! * internal authentication failures validate against `platform/internal-http`.

mod common;

use std::{collections::BTreeSet, path::PathBuf};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use calling::{
    calls::{
        attributes::FORBIDDEN_KEY_FRAGMENTS,
        events::CallEventType,
        kinds::{
            CallKind, DisconnectReason, EndpointCategory, FailureCategory, LegDirection, LegKind,
            LegState, ParticipantRole, SessionState, TerminalReason, UsageType, UsageUnit,
        },
        legs::NewLeg,
        quality::{NewQualityRecord, QualityMetrics},
        usage::{IdempotencyKey, NewUsage},
        CallAction, CallState, EndedEvidence,
    },
    domain::{Clock, EnvironmentKind},
    error::ErrorCode,
    platform::scopes::names,
    routes,
};
use common::{calls::CallWorld, Harness};
use http_body_util::BodyExt;
use jsonschema::{Registry, Resource, Validator};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use tower::ServiceExt;

const SCHEMA_BASE_URI: &str = "https://schemas.cfmeet.com/";
const CALLING_HTTP: &str = "calling/http/v1.openapi.json";
const CALLING_COMMON: &str = "calling/common/v1.schema.json";
const CALL_LIFECYCLE: &str = "calling/events/call-lifecycle/v1.schema.json";
const EVENT_ENVELOPE: &str = "platform/events/envelope/v1.schema.json";
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

/// A validator for a registered JSON Schema contract (offline registry).
fn schema(rel: &str) -> Validator {
    let registry = registry();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("{SCHEMA_BASE_URI}{rel}"),
        }))
        .unwrap()
}

fn common_enum(name: &str) -> Vec<String> {
    read(CALLING_COMMON)["$defs"][name]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{name} is not an enum"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// The router exactly as `main` builds it: the public call API, no internal routes yet.
fn production_router(h: &Harness) -> Router {
    routes::app(h.state.clone(), routes::public_api(), Router::new())
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
            // axum `/:param` segments are OpenAPI `/{param}` templates.
            let templated: Vec<String> = literal
                .split('/')
                .map(|seg| match seg.strip_prefix(':') {
                    Some(name) => format!("{{{name}}}"),
                    None => seg.to_string(),
                })
                .collect();
            registered.insert(templated.join("/"));
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

async fn send_authenticated(
    router: Router,
    path: &str,
    secret: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let headers: Vec<(String, String)> = secret
        .map(|s| vec![("authorization".to_string(), format!("Bearer {s}"))])
        .unwrap_or_default();
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    send(router, Method::GET, path, &refs).await
}

#[tokio::test]
async fn call_api_responses_validate_against_the_calling_contract() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call_schema = component(CALLING_HTTP, "/components/schemas/Call");
    let list_schema = component(CALLING_HTTP, "/components/schemas/CallList");
    let envelope = component(CALLING_HTTP, "/components/schemas/ErrorEnvelope");
    let request_id = component(CALLING_HTTP, "/components/headers/XRequestId/schema");
    let no_store = component(
        CALLING_HTTP,
        "/components/headers/CacheControlNoStore/schema",
    );

    let mut ids = Vec::new();
    for state in [
        CallState::Requested,
        CallState::Connected,
        CallState::Ended,
        CallState::Failed,
        CallState::Busy,
    ] {
        ids.push(w.call_in(state).await.id);
    }
    for id in &ids {
        let (status, headers, body) = send_authenticated(
            production_router(&w.h),
            &format!("/v1/calls/{id}"),
            Some(w.secret()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_valid(&call_schema, &body, "GET /v1/calls/{call_id}");
        assert_valid(
            &request_id,
            &json!(headers["x-request-id"].to_str().unwrap()),
            "x-request-id",
        );
        assert_valid(
            &no_store,
            &json!(headers["cache-control"].to_str().unwrap()),
            "cache-control",
        );
    }
    for path in [
        "/v1/calls",
        "/v1/calls?limit=2",
        "/v1/calls?state=CONNECTED",
    ] {
        let (status, headers, body) =
            send_authenticated(production_router(&w.h), path, Some(w.secret())).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_valid(&list_schema, &body, path);
        assert_valid(
            &no_store,
            &json!(headers["cache-control"].to_str().unwrap()),
            "cache-control",
        );
    }
    // Every documented error response, as really produced.
    let no_scope =
        w.h.issue(
            w.consumer.sa(EnvironmentKind::Production),
            &[names::PRINCIPALS_READ],
        )
        .await;
    for (path, secret, expected, code) in [
        (
            format!("/v1/calls/{}", ids[0]),
            None,
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_FAILED",
        ),
        (
            format!("/v1/calls/{}", ids[0]),
            Some(no_scope.secret.expose_once()),
            StatusCode::FORBIDDEN,
            "INSUFFICIENT_SCOPE",
        ),
        (
            format!("/v1/calls/{}", uuid::Uuid::new_v4()),
            Some(w.secret()),
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
        ),
        (
            "/v1/calls?limit=1000".to_string(),
            Some(w.secret()),
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
        ),
        (
            "/v1/calls".to_string(),
            None,
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_FAILED",
        ),
    ] {
        let (status, headers, body) =
            send_authenticated(production_router(&w.h), &path, secret).await;
        assert_eq!(status, expected, "{path}");
        assert_valid(&envelope, &body, &path);
        assert_eq!(body["error"]["code"], code);
        assert_eq!(
            headers["x-request-id"].to_str().unwrap(),
            body["error"]["requestId"]
        );
        let documented = read(CALLING_HTTP);
        let operation = if path.starts_with("/v1/calls/") {
            &documented["paths"]["/v1/calls/{call_id}"]["get"]
        } else {
            &documented["paths"]["/v1/calls"]["get"]
        };
        assert!(
            operation["responses"][expected.as_str()].is_object(),
            "{path}: {expected} is not documented"
        );
    }
    // The documented security requirement is the scope the handlers enforce.
    let documented = read(CALLING_HTTP);
    for path in ["/v1/calls", "/v1/calls/{call_id}"] {
        assert_eq!(
            documented["paths"][path]["get"]["security"],
            json!([{ "platformCredential": [names::CALLS_READ] }])
        );
        assert_eq!(
            documented["paths"][path]["get"]["x-cfmeet-required-scope"],
            names::CALLS_READ
        );
        for method in ["post", "put", "patch", "delete"] {
            assert!(
                documented["paths"][path].get(method).is_none(),
                "{method} {path}"
            );
        }
    }
}

#[test]
fn call_vocabularies_equal_the_shared_contract() {
    fn names<T: Copy>(all: &[T], f: impl Fn(T) -> &'static str) -> Vec<String> {
        all.iter().map(|v| f(*v).to_string()).collect()
    }
    assert_eq!(
        names(CallState::ALL, |v| v.as_str()),
        common_enum("CallState")
    );
    assert_eq!(
        names(CallKind::ALL, |v| v.as_str()),
        common_enum("CallKind")
    );
    assert_eq!(
        names(FailureCategory::ALL, |v| v.as_str()),
        common_enum("CallFailureCategory")
    );
    assert_eq!(
        names(TerminalReason::ALL, |v| v.as_str()),
        common_enum("CallTerminalReason")
    );
    assert_eq!(
        names(ParticipantRole::ALL, |v| v.as_str()),
        common_enum("CallParticipantRole")
    );
    assert_eq!(
        names(SessionState::ALL, |v| v.as_str()),
        common_enum("CallSessionState")
    );
    assert_eq!(
        names(EndpointCategory::ALL, |v| v.as_str()),
        common_enum("CallEndpointCategory")
    );
    assert_eq!(
        names(CallEventType::ALL, |v| v.as_str()),
        common_enum("CallEventType")
    );
    // The safe-data key rule is the same in Rust and in the contract.
    let pattern = read(CALLING_COMMON)["$defs"]["SafeEventData"]["propertyNames"]["not"]["pattern"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(pattern, format!("({})", FORBIDDEN_KEY_FRAGMENTS.join("|")));
    // Canonical states are defined once: the HTTP contract and the event contract refer
    // to the shared definition instead of repeating it.
    for rel in [CALLING_HTTP, CALL_LIFECYCLE] {
        let text = std::fs::read_to_string(contracts().join(rel)).unwrap();
        assert!(
            !text.contains("\"TRANSFERRING\""),
            "{rel} redefines CallState"
        );
        assert!(
            text.contains("calling/common/v1.schema.json#/$defs/CallState"),
            "{rel}"
        );
    }
}

#[tokio::test]
async fn every_outbox_row_is_a_valid_envelope_with_a_valid_lifecycle_payload() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let envelope = schema(EVENT_ENVELOPE);
    let lifecycle = schema(CALL_LIFECYCLE);
    // Exercise every kind of event the domain produces.
    let call = w.call_in(CallState::Connected).await;
    let sessions = w.calls().sessions(w.scope(), call.id).await.unwrap();
    w.calls()
        .disconnect_session(
            &w.cmd,
            call.id,
            sessions[0].id,
            DisconnectReason::ConnectionLost,
        )
        .await
        .unwrap();
    w.calls()
        .reconnect_session(&w.cmd, call.id, sessions[0].id)
        .await
        .unwrap();
    w.calls()
        .replace_session(&w.cmd, call.id, sessions[1].id, EndpointCategory::Web)
        .await
        .unwrap();
    let participant = w.calls().participants(w.scope(), call.id).await.unwrap()[1].id;
    w.calls()
        .set_participant_display_name(
            &w.cmd,
            call.id,
            participant,
            calling::calls::participants::DisplayName::parse("Ada").ok(),
        )
        .await
        .unwrap();
    let route = w
        .calls()
        .record_route(
            &w.cmd,
            call.id,
            calling::calls::routes::NewRoute {
                route_class: calling::calls::kinds::RouteClass::Mixed,
                policy_version: calling::calls::routes::PolicyVersion::parse("v1").unwrap(),
                decision_snapshot: calling::calls::attributes::SafeAttributes::new(),
            },
        )
        .await
        .unwrap();
    w.calls()
        .update_route_state(
            &w.cmd,
            call.id,
            route.id,
            calling::calls::kinds::RouteState::Active,
        )
        .await
        .unwrap();
    let leg = w
        .calls()
        .record_leg(
            &w.cmd,
            call.id,
            NewLeg {
                route_id: Some(route.id),
                participant_id: None,
                direction: LegDirection::Outbound,
                kind: LegKind::PublicNetwork,
            },
        )
        .await
        .unwrap();
    w.calls()
        .update_leg_state(
            &w.cmd,
            call.id,
            leg.id,
            LegState::Failed,
            Some(FailureCategory::Network),
        )
        .await
        .unwrap();
    w.calls()
        .assign_leg_execution_binding(&w.cmd, call.id, leg.id, uuid::Uuid::new_v4())
        .await
        .unwrap();
    let attempt = w
        .calls()
        .record_attempt(&w.cmd, call.id, Default::default())
        .await
        .unwrap();
    w.calls()
        .finish_attempt(
            &w.cmd,
            call.id,
            attempt.id,
            calling::calls::kinds::AttemptOutcome::Unknown,
            None,
        )
        .await
        .unwrap();
    let now = w.h.clock.now();
    w.calls()
        .record_quality(
            &w.cmd,
            call.id,
            NewQualityRecord {
                session_id: Some(sessions[0].id),
                leg_id: None,
                observed_from: now,
                observed_to: now,
                metrics: QualityMetrics::new().with("jitter_ms", 3.5).unwrap(),
            },
        )
        .await
        .unwrap();
    let (usage, _) = w
        .calls()
        .record_usage(
            &w.cmd,
            call.id,
            NewUsage {
                usage_type: UsageType::CallConnectedTime,
                participant_id: None,
                session_id: None,
                leg_id: None,
                quantity: 1,
                unit: UsageUnit::Milliseconds,
                usage_started_at: None,
                usage_ended_at: None,
                idempotency_key: IdempotencyKey::parse("k1").unwrap(),
            },
        )
        .await
        .unwrap();
    w.calls()
        .finalize_usage(&w.cmd, call.id, usage.id)
        .await
        .unwrap();
    w.act(call.id, CallAction::RequestEnd).await.unwrap();
    w.act(
        call.id,
        CallAction::ConfirmEnded {
            evidence: EndedEvidence::CleanupConfirmed,
        },
    )
    .await
    .unwrap();
    let denied = w.create().await.call;
    let decision = w
        .authorize_decision(
            denied.id,
            calling::calls::kinds::AuthorizationReason::PolicyDenied,
        )
        .await;
    w.act(
        denied.id,
        CallAction::DenyByPolicy {
            authorization_id: decision.id,
        },
    )
    .await
    .unwrap();

    let rows = w
        .h
        .conn()
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT envelope::text FROM calling_outbox_events ORDER BY created_at, id".to_string(),
        ))
        .await
        .unwrap();
    let mut types = BTreeSet::new();
    for row in rows {
        let text: String = row.try_get_by_index(0).unwrap();
        let env: Value = serde_json::from_str(&text).unwrap();
        assert_valid(&envelope, &env, "outbox envelope");
        let payload = &env["payload"];
        assert_valid(&lifecycle, payload, "call lifecycle payload");
        assert_eq!(payload["event_id"], env["event_id"]);
        assert_eq!(payload["correlation_id"], env["correlation_id"]);
        assert_eq!(env["producer_service"], "cfmeet.calling");
        assert_eq!(env["runtime_environment"], "test");
        assert_eq!(env["ownership"]["tenant_id"], payload["tenant_id"]);
        let event_type = payload["event_type"].as_str().unwrap();
        assert_eq!(
            env["event_type"],
            format!("calling.{}", event_type.to_lowercase().replace('_', "-"))
        );
        types.insert(event_type.to_string());
    }
    let all: BTreeSet<String> = CallEventType::ALL
        .iter()
        .map(|t| t.as_str().to_string())
        .collect();
    assert_eq!(types, all, "every event type was produced and validated");
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
    for (id, path) in [
        ("cfmeet.calling.call-common", CALLING_COMMON),
        ("cfmeet.calling.call-lifecycle-event", CALL_LIFECYCLE),
        ("cfmeet.platform.event-envelope", EVENT_ENVELOPE),
    ] {
        let entry = manifest["contracts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("{id} is not registered"));
        assert_eq!(entry["path"], path);
        assert_eq!(read(path)["$id"], format!("{SCHEMA_BASE_URI}{path}"));
    }
}

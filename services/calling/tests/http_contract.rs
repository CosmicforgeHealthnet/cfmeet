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
const PRIVACY_RELAY_HTTP: &str = "privacy-relay/http/v1.openapi.json";

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
    // Calling's own routes (its contract) and the Privacy Relay ingress routes, whose
    // contract is the Privacy Relay API (one contract, never defined twice).
    let mut registered = BTreeSet::new();
    let mut ingress = BTreeSet::new();
    for file in files {
        let code = std::fs::read_to_string(&file).unwrap();
        let path = file.to_string_lossy().replace('\\', "/");
        let is_ingress = path.ends_with("src/privacy_relay/http.rs");
        // Internal routers are nested under /internal/v1 by routes::app.
        let prefix = if path.ends_with("src/privacy_relay/enforcement.rs") {
            "/internal/v1"
        } else {
            ""
        };
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
            let route = format!("{prefix}{}", templated.join("/"));
            if is_ingress {
                ingress.insert(route);
            } else {
                registered.insert(route);
            }
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
    let privacy: BTreeSet<String> = read(PRIVACY_RELAY_HTTP)["paths"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        ingress, privacy,
        "the Privacy Relay ingress and the Privacy Relay API contract disagree"
    );
    assert!(registered.is_disjoint(&privacy));
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
    // The documented security requirement is the scope the handlers enforce; reads
    // never mutate and no route sets a call's state generically.
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
        for method in ["put", "patch", "delete"] {
            assert!(
                documented["paths"][path].get(method).is_none(),
                "{method} {path}"
            );
        }
    }
    assert!(documented["paths"]["/v1/calls/{call_id}"]
        .get("post")
        .is_none());
    assert_eq!(
        documented["paths"]["/v1/calls"]["post"]["security"],
        json!([{ "platformCredential": [names::CALLS_WRITE] }])
    );
}

/// Every documented operation names exactly one authentication: the platform credential
/// with the scope its handler enforces, the endpoint's signaling credential, or the
/// media provider's signature (callback only); `/health` alone is public.
#[test]
fn every_operation_declares_its_authentication() {
    let documented = read(CALLING_HTTP);
    for (path, item) in documented["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let security = op["security"]
                .as_array()
                .unwrap_or_else(|| panic!("{method} {path}"));
            if path == "/health" {
                assert!(security.is_empty());
                continue;
            }
            assert_eq!(security.len(), 1, "{method} {path}");
            let scheme = security[0]
                .as_object()
                .unwrap()
                .keys()
                .next()
                .unwrap()
                .as_str();
            let expected = if path.starts_with("/internal/v1/") {
                // Internal routes: a service token and a receiver-granted permission.
                assert_eq!(
                    security[0]["cfServiceAuth"][0], "communication-authority.enforce",
                    "{method} {path}"
                );
                "cfServiceAuth"
            } else if path.starts_with("/internal/provider/") {
                "mediaProviderSignature"
            } else if path.starts_with("/v1/calling/media/") {
                "mediaCredential"
            } else if path == "/v1/calling/signaling"
                || path.starts_with("/v1/calling/invitations")
                || path.starts_with("/v1/calling/sessions/")
            {
                "signalingCredential"
            } else {
                "platformCredential"
            };
            assert_eq!(scheme, expected, "{method} {path}");
            if scheme == "platformCredential" {
                let scope = security[0][scheme][0].as_str().unwrap();
                let expected_scope = if method == "get" {
                    names::CALLS_READ
                } else {
                    names::CALLS_WRITE
                };
                assert_eq!(scope, expected_scope, "{method} {path}");
                assert_eq!(op["x-cfmeet-required-scope"], scope, "{method} {path}");
            }
            assert!(
                op["responses"]["401"].is_object(),
                "{method} {path}: authentication failure is not documented"
            );
        }
    }
    // The Privacy Relay API served by this ingress: platform credential, privacy scopes.
    for (path, item) in read(PRIVACY_RELAY_HTTP)["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let scope = op["security"][0]["platformCredential"][0].as_str().unwrap();
            let family = if path.starts_with("/v1/privacy-relay/sessions")
                || (path.ends_with("/sessions") && method == "post")
            {
                "privacy.sessions"
            } else {
                "privacy.contexts"
            };
            let expected = format!(
                "{family}.{}",
                if method == "get" { "read" } else { "write" }
            );
            assert_eq!(scope, expected, "{method} {path}");
            assert_eq!(op["x-cfmeet-required-scope"], scope, "{method} {path}");
            assert!(op["responses"]["401"].is_object(), "{method} {path}");
        }
    }
    // No generic state mutation and no provider vocabulary in the public contracts.
    // (Forbidden-key patterns name such terms on purpose and are skipped.)
    fn texts(value: &Value, out: &mut String) {
        match value {
            Value::String(s) => {
                out.push_str(&s.to_lowercase());
                out.push('\n');
            }
            Value::Array(items) => items.iter().for_each(|v| texts(v, out)),
            Value::Object(map) => {
                for (k, v) in map {
                    out.push_str(&k.to_lowercase());
                    out.push('\n');
                    if k != "pattern" {
                        texts(v, out);
                    }
                }
            }
            _ => {}
        }
    }
    for rel in [
        CALLING_HTTP,
        CALL_CONTROL,
        REALTIME,
        PUSH_INVITATION,
        CALLING_COMMON,
    ] {
        let mut text = String::new();
        texts(&read(rel), &mut text);
        for term in [
            "livekit",
            "twilio",
            "vonage",
            "roomsid",
            "room_sid",
            "participantsid",
            "apisecret",
            "api_secret",
        ] {
            assert!(!text.contains(term), "{rel} mentions {term}");
        }
    }
    let paths: Vec<&String> = documented["paths"].as_object().unwrap().keys().collect();
    assert!(paths.iter().all(|p| !p.ends_with("/state")), "{paths:?}");
    for (_, item) in documented["paths"].as_object().unwrap() {
        assert!(item.get("patch").is_none() && item.get("put").is_none());
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
    {
        use calling::calls::kinds::{
            InvitationPurpose, InvitationStatus, InvitationTerminalReason, MediaAccess,
            TransferFailureReason, TransferState,
        };
        use calling::endpoints::{EndpointKind, EndpointStatus, PresenceState};
        assert_eq!(
            names(InvitationPurpose::ALL, |v| v.as_str()),
            common_enum("CallInvitationPurpose")
        );
        assert_eq!(
            names(InvitationStatus::ALL, |v| v.as_str()),
            common_enum("CallInvitationStatus")
        );
        assert_eq!(
            names(InvitationTerminalReason::ALL, |v| v.as_str()),
            common_enum("CallInvitationTerminalReason")
        );
        assert_eq!(
            names(MediaAccess::ALL, |v| v.as_str()),
            common_enum("CallMediaAccess")
        );
        assert_eq!(
            names(TransferState::ALL, |v| v.as_str()),
            common_enum("CallTransferState")
        );
        assert_eq!(
            names(TransferFailureReason::ALL, |v| v.as_str()),
            common_enum("CallTransferFailureReason")
        );
        assert_eq!(
            names(EndpointKind::ALL, |v| v.as_str()),
            common_enum("CallEndpointKind")
        );
        assert_eq!(
            names(EndpointStatus::ALL, |v| v.as_str()),
            common_enum("CallEndpointStatus")
        );
        assert_eq!(
            names(PresenceState::ALL, |v| v.as_str()),
            common_enum("CallEndpointPresence")
        );
        // Realtime signal types and the server-authorized media capabilities.
        let signal_types: Vec<String> = read(REALTIME)["$defs"]["SignalType"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names(calling::signaling::signals::SignalType::ALL, |v| v.as_str()),
            signal_types
        );
        let capabilities = serde_json::to_value(calling::media::MediaPermissions::NONE).unwrap();
        let mut keys: Vec<&String> = capabilities.as_object().unwrap().keys().collect();
        keys.sort();
        let mut documented: Vec<String> = read(CALLING_COMMON)["$defs"]["CallMediaCapabilities"]
            ["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        documented.sort();
        assert_eq!(keys, documented.iter().collect::<Vec<_>>());
    }
    // The safe-data key rule is the same in Rust and in the contract.
    let pattern = read(CALLING_COMMON)["$defs"]["SafeEventData"]["propertyNames"]["not"]["pattern"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(pattern, format!("({})", FORBIDDEN_KEY_FRAGMENTS.join("|")));
    // Canonical states are defined once: the HTTP contract and the event contract refer
    // to the shared definition instead of repeating it.
    for rel in [CALLING_HTTP, CALL_LIFECYCLE, CALL_CONTROL, REALTIME] {
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

/// Application calling flows producing every ring-offer, transfer and media fact.
async fn exercise_app_calling(a: &common::app::AppWorld) {
    use calling::media::{provider::ProviderParticipant, MediaPermissions};
    use common::fake_media::FakeOp;

    let alice = a.w.initiator.clone();
    let bob = a.w.recipient.clone();
    let carol = a.principal("events-carol").await;
    let dave = a.principal("events-dave").await;
    let erin = a.principal("events-erin").await;
    let frank = a.principal("events-frank").await;
    let alice_d = a.device(&alice, "events-alice").await;
    let bob_1 = a.device(&bob, "events-bob-1").await;
    let bob_2 = a.device(&bob, "events-bob-2").await;
    let carol_d = a.device(&carol, "events-carol").await;
    let dave_d = a.device(&dave, "events-dave").await;
    let erin_d = a.device(&erin, "events-erin").await;
    a.device(&frank, "events-frank").await;
    let r = a
        .create_call("VOICE", &alice, &alice_d, &[&bob, &carol], "events-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: uuid::Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let room = common::app::AppWorld::room(call);
    let invitation = a.invitation_for(call, bob_1.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, invitation, &bob_1, version).await.status,
        StatusCode::OK
    );
    let invitation = a.invitation_for(call, carol_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.decline(call, invitation, &carol_d, version).await.status,
        StatusCode::OK
    );
    assert!(a.invitation_for(call, bob_2.endpoint).await.is_none());
    a.connect(call, a.session_on(call, alice_d.endpoint).await.unwrap())
        .await;
    let bob_p = a
        .connect(call, a.session_on(call, bob_1.endpoint).await.unwrap())
        .await;
    assert_eq!(a.state(call).await.0, CallState::Connected);
    for action in ["hold", "resume"] {
        let (_, version) = a.state(call).await;
        assert_eq!(
            a.command(call, action, version).await.status,
            StatusCode::OK
        );
    }
    // An unknown connection is quarantined and evicted; drift is reconciled.
    let intruder = ProviderParticipant {
        identity: "intruder".into(),
        generation: None,
        permissions: MediaPermissions::NONE,
        active: true,
        published_audio: false,
        published_video: false,
        joined_at: None,
    };
    a.media().inject(&room, intruder.clone());
    let (auth, body) = a
        .media()
        .signed_event("participant_joined", &room, Some(&intruder));
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    let mut drift = bob_p.permissions;
    drift.can_publish_video = true;
    a.media().force_permissions(&room, &bob_p.identity, drift);
    a.h().clock.advance(chrono::Duration::seconds(30));
    a.h().state.app.reconcile_due(100).await.unwrap();
    // A failed transfer (target declines), then a completed one.
    let bob_participant = a.participant_of(call, bob.id().as_uuid()).await;
    let (_, version) = a.state(call).await;
    let r = a
        .transfer(call, bob_participant, &erin, version, "events-t1")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let offer = a.invitation_for(call, erin_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.decline(call, offer, &erin_d, version).await.status,
        StatusCode::OK
    );
    assert_eq!(a.state(call).await.0, CallState::Connected);
    let (_, version) = a.state(call).await;
    let r = a
        .transfer(call, bob_participant, &dave, version, "events-t2")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let offer = a.invitation_for(call, dave_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &dave_d, version).await.status,
        StatusCode::OK
    );
    let dave_session = a.session_on(call, dave_d.endpoint).await.unwrap();
    a.connect(call, dave_session).await;
    assert_eq!(a.state(call).await.0, CallState::Connected);
    // Provider removal times out: enforcement stays pending (reconciled later).
    a.media().fail_next(
        FakeOp::Remove,
        calling::media::provider::MediaProviderError::Timeout,
        1,
    );
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{dave_session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // A transfer cut short by END is cancelled; the media resource is terminated.
    let alice_participant = a.participant_of(call, alice.id().as_uuid()).await;
    let (_, version) = a.state(call).await;
    let r = a
        .transfer(call, alice_participant, &frank, version, "events-t3")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (_, version) = a.state(call).await;
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // An offer nobody answers expires.
    let r = a
        .create_call("VIDEO", &alice, &alice_d, &[&carol], "events-2")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    a.h().clock.advance(chrono::Duration::seconds(120));
    a.h().state.app.expire_due(100).await.unwrap();
}

#[tokio::test]
async fn every_outbox_row_is_a_valid_envelope_with_a_valid_lifecycle_payload() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let a = common::app::AppWorld { w };
    let w = &a.w;
    let envelope = schema(EVENT_ENVELOPE);
    let lifecycle = schema(CALL_LIFECYCLE);
    exercise_app_calling(&a).await;
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
        ("cfmeet.calling.call-control", CALL_CONTROL),
        ("cfmeet.calling.realtime-signal", REALTIME),
        ("cfmeet.calling.push-invitation", PUSH_INVITATION),
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

const CALL_CONTROL: &str = "calling/control/v1.schema.json";
const REALTIME: &str = "calling/realtime/v1.schema.json";
const PUSH_INVITATION: &str = "calling/push/invitation/v1.schema.json";

/// A JSON-pointer segment, percent-encoded for use in a URI fragment.
fn escape(segment: &str) -> String {
    segment
        .replace('~', "~0")
        .replace('/', "~1")
        .replace('{', "%7B")
        .replace('}', "%7D")
}

/// Asserts that `response` is a documented response of `method template` and that its
/// body (and request id / cache headers) validate against the documented schema.
fn conforms(response: &common::TestResponse, template: &str, method: &str) {
    let doc = read(CALLING_HTTP);
    let status = response.status.as_u16().to_string();
    let documented = &doc["paths"][template][method]["responses"][&status];
    assert!(
        documented.is_object(),
        "{method} {template}: {status} is not documented ({})",
        response.text
    );
    let pointer = match documented.get("$ref").and_then(Value::as_str) {
        Some(reference) => format!(
            "{}/content/application~1json/schema",
            reference.trim_start_matches('#')
        ),
        None => format!(
            "/paths/{}/{method}/responses/{status}/content/application~1json/schema",
            escape(template)
        ),
    };
    assert_valid(
        &component(CALLING_HTTP, &pointer),
        &response.json,
        &format!("{method} {template} {status}"),
    );
    assert_valid(
        &component(CALLING_HTTP, "/components/headers/XRequestId/schema"),
        &json!(response.headers["x-request-id"].to_str().unwrap()),
        "x-request-id",
    );
    if response.status.is_success() {
        assert_eq!(response.headers["cache-control"], "no-store", "{template}");
    }
    if let Some(replayed) = response.headers.get("idempotent-replayed") {
        assert!(
            doc["paths"][template][method]["responses"][&status]["headers"]
                .get("idempotent-replayed")
                .is_some(),
            "{method} {template}: Idempotent-Replayed is not documented"
        );
        assert_eq!(replayed, "true");
    }
}

#[tokio::test]
async fn app_calling_responses_signals_and_pushes_validate_against_the_contracts() {
    use calling::signaling::RealtimeSignalBus;
    use common::app::{AppWorld, Device};

    let Some(a) = AppWorld::new().await else {
        return;
    };
    let alice = a.w.initiator.clone();
    let bob = a.w.recipient.clone();
    let carol = a.principal("contract-carol").await;
    let dave = a.principal("contract-dave").await;
    let register = |principal: &calling::platform::principals::ResolvedPrincipal,
                    key: &str,
                    push: Option<uuid::Uuid>| {
        json!({
            "principalId": principal.id(),
            "endpointKey": key,
            "endpointKind": "MOBILE",
            "capabilities": { "audio": true, "video": true },
            "pushTargetRef": push,
        })
    };
    let mut devices = Vec::new();
    let push_target = uuid::Uuid::new_v4();
    for (principal, key, push) in [
        (&alice, "alice-1", None),
        (&bob, "bob-1", Some(push_target)),
        (&bob, "bob-2", None),
        (&carol, "carol-1", None),
        (&dave, "dave-1", None),
    ] {
        let r = a
            .post("/v1/calling/endpoints", register(principal, key, push))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        conforms(&r, "/v1/calling/endpoints", "post");
        devices.push(Device {
            principal: principal.id().as_uuid(),
            endpoint: r.json["endpointId"].as_str().unwrap().parse().unwrap(),
        });
    }
    let [alice_d, bob_1, bob_2, carol_d, dave_d] = devices.try_into().unwrap();
    let r = a
        .post("/v1/calling/endpoints", register(&alice, "alice-1", None))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    conforms(&r, "/v1/calling/endpoints", "post");
    let r = a
        .get(&format!("/v1/calling/endpoints?principalId={}", bob.id()))
        .await;
    conforms(&r, "/v1/calling/endpoints", "get");
    assert_eq!(r.json["data"].as_array().unwrap().len(), 2);
    let r = a
        .post(
            "/v1/calling/signaling-credentials",
            json!({ "principalId": bob.id(), "endpointId": bob_1.endpoint }),
        )
        .await;
    conforms(&r, "/v1/calling/signaling-credentials", "post");
    let bob_token = r.json["token"].as_str().unwrap().to_string();
    let subscriptions: Vec<_> = [&alice_d, &bob_1, &bob_2, &carol_d, &dave_d]
        .iter()
        .map(|d| (d.endpoint, d.principal, a.h().bus.subscribe(d.endpoint)))
        .collect();

    // Group call alice → bob (2 devices), carol; replayed creation.
    let r = a
        .create_call(
            "VOICE",
            &alice,
            &alice_d,
            &[&bob, &carol],
            "contract-create",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(&r, "/v1/calls", "post");
    let call: uuid::Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let replay = a
        .create_call(
            "VOICE",
            &alice,
            &alice_d,
            &[&bob, &carol],
            "contract-create",
        )
        .await;
    conforms(&replay, "/v1/calls", "post");
    assert_eq!(replay.json, r.json);
    let conflict = a
        .create_call("VIDEO", &alice, &alice_d, &[&bob], "contract-create")
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.json["error"]["code"], "IDEMPOTENCY_CONFLICT");
    conforms(&conflict, "/v1/calls", "post");
    let r = a.get(&format!("/v1/calls/{call}/actions")).await;
    conforms(&r, "/v1/calls/{call_id}/actions", "get");

    // Bob recovers and accepts on device 1 (signaling credential); device 2 lost.
    let r = a
        .as_endpoint(
            &bob_token,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{} {bob_token}", r.text);
    conforms(&r, "/v1/calling/invitations", "get");
    let invitation = a.invitation_for(call, bob_1.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a
        .as_endpoint(
            &bob_token,
            Method::POST,
            &format!("/v1/calling/invitations/{invitation}/accept"),
            Some(json!({ "expectedVersion": version })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(&r, "/v1/calling/invitations/{invitation_id}/accept", "post");
    let other = a.call_view(call).await["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["endpointId"] == bob_2.endpoint.to_string())
        .map(|i| {
            i["invitationId"]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap()
        })
        .unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, other, &bob_2, version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    conforms(&r, "/v1/calls/{call_id}/accept", "post");
    let carol_invitation = a.invitation_for(call, carol_d.endpoint).await.unwrap();
    let r = a.decline(call, carol_invitation, &carol_d, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(&r, "/v1/calls/{call_id}/decline", "post");
    let r = a.command(call, "hold", 1).await;
    assert_eq!(r.json["error"]["code"], "STALE_CALL_VERSION");
    conforms(&r, "/v1/calls/{call_id}/hold", "post");

    // Media credentials (both routes), verified joins, forged callback.
    let alice_session = a.session_on(call, alice_d.endpoint).await.unwrap();
    let bob_session = a.session_on(call, bob_1.endpoint).await.unwrap();
    let r = a.media_credential(call, alice_session).await;
    conforms(
        &r,
        "/v1/calls/{call_id}/sessions/{session_id}/media-credentials",
        "post",
    );
    a.join(call, r.json["token"].as_str().unwrap()).await;
    let r = a
        .as_endpoint(
            &bob_token,
            Method::POST,
            &format!("/v1/calling/sessions/{bob_session}/media-credentials"),
            None,
            &[],
        )
        .await;
    conforms(
        &r,
        "/v1/calling/sessions/{session_id}/media-credentials",
        "post",
    );
    let participant = a.media().join(r.json["token"].as_str().unwrap()).unwrap();
    let (auth, body) = a.media().signed_event(
        "participant_joined",
        &AppWorld::room(call),
        Some(&participant),
    );
    let r = a.callback(&auth, body.clone()).await;
    conforms(&r, "/internal/provider/media/events", "post");
    let r = a.callback("Bearer forged.token.value", body).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    conforms(&r, "/internal/provider/media/events", "post");
    assert_eq!(a.state(call).await.0, CallState::Connected);

    // Hold / resume / transfer bob → dave / removal / end.
    for action in ["hold", "resume"] {
        let (_, version) = a.state(call).await;
        let r = a.command(call, action, version).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        conforms(&r, &format!("/v1/calls/{{call_id}}/{action}"), "post");
    }
    let bob_participant = a.participant_of(call, bob.id().as_uuid()).await;
    let (_, version) = a.state(call).await;
    let r = a
        .transfer(call, bob_participant, &dave, version, "contract-transfer")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(&r, "/v1/calls/{call_id}/transfer", "post");
    let replay = a
        .transfer(call, bob_participant, &dave, version, "contract-transfer")
        .await;
    conforms(&replay, "/v1/calls/{call_id}/transfer", "post");
    assert_eq!(replay.json, r.json);
    let dave_invitation = a.invitation_for(call, dave_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, dave_invitation, &dave_d, version).await;
    conforms(&r, "/v1/calls/{call_id}/accept", "post");
    let dave_session = a.session_on(call, dave_d.endpoint).await.unwrap();
    a.connect(call, dave_session).await;
    let view = a.call_view(call).await;
    assert_eq!(view["state"], "CONNECTED", "{view:#}");
    assert_eq!(view["transfers"][0]["state"], "COMPLETED", "{view:#}");
    assert_valid(
        &component(CALLING_HTTP, "/components/schemas/Call"),
        &view,
        "call with invitations and a transfer",
    );
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{dave_session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        &r,
        "/v1/calls/{call_id}/sessions/{session_id}/remove",
        "post",
    );
    let (_, version) = a.state(call).await;
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(&r, "/v1/calls/{call_id}/end", "post");
    let r = a.media_credential(call, alice_session).await;
    conforms(
        &r,
        "/v1/calls/{call_id}/sessions/{session_id}/media-credentials",
        "post",
    );

    // Cancel; late accept; expired offer.
    let r = a
        .create_call("VIDEO", &alice, &alice_d, &[&bob], "contract-cancel")
        .await;
    let cancelled: uuid::Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let late = a.invitation_for(cancelled, bob_2.endpoint).await.unwrap();
    let (_, version) = a.state(cancelled).await;
    let r = a.command(cancelled, "cancel", version).await;
    conforms(&r, "/v1/calls/{call_id}/cancel", "post");
    let r = a.accept(cancelled, late, &bob_2, version + 1).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    conforms(&r, "/v1/calls/{call_id}/accept", "post");
    let r = a
        .create_call("VOICE", &alice, &alice_d, &[&carol], "contract-expire")
        .await;
    let expiring: uuid::Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let offer = a.invitation_for(expiring, carol_d.endpoint).await.unwrap();
    a.h().clock.advance(chrono::Duration::seconds(120));
    let (_, version) = a.state(expiring).await;
    let r = a.accept(expiring, offer, &carol_d, version).await;
    assert_eq!(r.status, StatusCode::GONE, "{}", r.text);
    conforms(&r, "/v1/calls/{call_id}/accept", "post");
    a.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(a.state(expiring).await.0, CallState::NoAnswer);

    // Every delivered signal is a valid realtime signal; every push a valid push offer.
    let realtime = schema(REALTIME);
    let mut kinds = BTreeSet::new();
    for (endpoint, principal, mut subscription) in subscriptions {
        while let Some(signal) = subscription.try_recv() {
            let delivered = calling::signaling::signals::for_target(signal, endpoint, principal);
            assert_valid(&realtime, &delivered, "realtime signal");
            kinds.insert(delivered["type"].as_str().unwrap().to_string());
        }
    }
    for expected in [
        "call.incoming",
        "call.ringing",
        "call.accepted",
        "call.answered",
        "call.answered_elsewhere",
        "call.declined",
        "call.connected",
        "call.held",
        "call.resumed",
        "call.transfer_started",
        "call.transferred",
        "call.ended",
        "call.cancelled",
        "call.ring_cancelled",
        "call.invitation_expired",
        "call.no_answer",
    ] {
        assert!(
            kinds.contains(expected),
            "{expected} not delivered: {kinds:?}"
        );
    }
    let pushes = a.h().push.sent();
    assert!(!pushes.is_empty(), "push-ready offers were produced");
    let push = schema(PUSH_INVITATION);
    for (target, payload) in pushes {
        assert_eq!(target, push_target);
        assert_valid(&push, &payload, "push-ready offer");
    }
    // Requests the service accepted are valid contract bodies, too.
    let control = schema(CALL_CONTROL);
    assert_valid(
        &control,
        &json!({ "kind": "VOICE", "initiatorPrincipalId": alice.id(), "initiatorEndpointId": alice_d.endpoint, "targetPrincipalIds": [bob.id(), carol.id()] }),
        "create request",
    );
}

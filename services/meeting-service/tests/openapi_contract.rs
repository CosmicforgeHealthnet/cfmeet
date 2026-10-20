//! OpenAPI is the authoritative consumer contract (spec §13, Part J §22–§23): the
//! committed export must match the code, cover every v2 route (both directions, derived
//! from the real route table), resolve every reference, and never describe secrets,
//! provider ids or internal-only callback schemas.

use std::collections::{BTreeSet, HashSet};

use serde_json::Value;

const EXPORT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packages/contracts/openapi/meeting-service.json"
);

fn generated() -> Value {
    serde_json::from_str(&backend::swagger::openapi_json()).unwrap()
}

#[test]
fn committed_contract_matches_code() {
    let committed: Value =
        serde_json::from_str(&std::fs::read_to_string(EXPORT).expect("exported contract exists"))
            .expect("valid JSON");
    assert_eq!(
        committed,
        generated(),
        "packages/contracts/openapi/meeting-service.json is stale; regenerate with \
         `cargo run --bin export_openapi > ../../packages/contracts/openapi/meeting-service.json`"
    );
}

#[test]
fn every_v2_route_is_documented() {
    let doc = generated();
    let paths = doc["paths"].as_object().unwrap();
    let expected = [
        ("post", "/api/v2/meeting-spaces"),
        ("get", "/api/v2/meeting-spaces"),
        ("get", "/api/v2/meeting-spaces/{id}"),
        ("patch", "/api/v2/meeting-spaces/{id}"),
        ("post", "/api/v2/meeting-spaces/{id}/archive"),
        ("post", "/api/v2/meeting-spaces/{id}/duplicate"),
        ("post", "/api/v2/meeting-spaces/{id}/owner-transfer"),
        ("put", "/api/v2/meeting-spaces/{id}/recurrence"),
        ("get", "/api/v2/meeting-spaces/{id}/recurrence"),
        ("delete", "/api/v2/meeting-spaces/{id}/recurrence"),
        ("get", "/api/v2/meeting-spaces/{id}/members"),
        ("post", "/api/v2/meeting-spaces/{id}/members"),
        ("patch", "/api/v2/meeting-spaces/{id}/members/{memberId}"),
        ("delete", "/api/v2/meeting-spaces/{id}/members/{memberId}"),
        ("post", "/api/v2/meeting-spaces/{id}/invitations"),
        ("get", "/api/v2/meeting-spaces/{id}/invitations"),
        (
            "post",
            "/api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke",
        ),
        ("post", "/api/v2/invitations/{token}/resolve"),
        ("post", "/api/v2/meeting-spaces/{id}/conferences"),
        ("get", "/api/v2/meeting-spaces/{id}/conferences"),
        ("post", "/api/v2/instant-meetings"),
        ("get", "/api/v2/resolve/{identifier}"),
        ("get", "/api/v2/conferences/{id}"),
        ("post", "/api/v2/conferences/{id}/prepare"),
        ("post", "/api/v2/conferences/{id}/start"),
        ("post", "/api/v2/conferences/{id}/join"),
        ("post", "/api/v2/conferences/{id}/leave"),
        ("post", "/api/v2/conferences/{id}/end"),
        ("post", "/api/v2/conferences/{id}/lock"),
        ("post", "/api/v2/conferences/{id}/unlock"),
        ("post", "/api/v2/conferences/{id}/cancel"),
        ("post", "/api/v2/conferences/{id}/reschedule"),
        ("get", "/api/v2/conferences/{id}/participants"),
        ("get", "/api/v2/conferences/{id}/participants/me"),
        ("get", "/api/v2/conferences/{id}/waiting-room"),
        ("post", "/api/v2/conferences/{id}/waiting-room/admit"),
        ("post", "/api/v2/conferences/{id}/waiting-room/deny"),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/return-to-waiting",
        ),
        (
            "patch",
            "/api/v2/conferences/{id}/participants/{participantId}/role",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/remove",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/ban",
        ),
        ("get", "/api/v2/admin/compat/legacy-meetings/{id}/shadow"),
        ("post", "/api/v2/admin/compat/backfill"),
        // Part V H — Unified Messaging identity bridge and chat migration
        ("post", "/api/v2/messaging/devices/registration-challenges"),
        ("post", "/api/v2/messaging/devices"),
        ("post", "/api/v2/messaging/devices/{deviceId}/key-packages"),
        ("post", "/api/v2/messaging/devices/{deviceId}/revoke"),
        ("post", "/api/v2/messaging/client-tokens"),
        ("post", "/api/v2/messaging/client-sessions/revoke-all"),
        ("get", "/api/v2/messaging/me"),
        ("get", "/api/v2/messaging/directory"),
        ("post", "/api/v2/messaging/directory/resolve"),
        ("post", "/api/v2/messaging/principals/names"),
        ("get", "/api/v2/messaging/relay/{path}"),
        ("post", "/api/v2/messaging/relay/{path}"),
        ("put", "/api/v2/messaging/relay/{path}"),
        ("delete", "/api/v2/messaging/relay/{path}"),
        ("get", "/api/v2/admin/chat-migration"),
        ("post", "/api/v2/admin/chat-migration/transitions"),
        ("post", "/api/v2/admin/chat-migration/parity-runs"),
        ("put", "/api/v2/meeting-spaces/{id}/recurrence/exceptions"),
        ("get", "/api/v2/meeting-spaces/{id}/recurrence/exceptions"),
        ("post", "/api/v2/admin/organizations"),
        ("patch", "/api/v2/admin/organizations/{id}"),
        ("get", "/api/v2/organizations/{id}/members"),
        ("put", "/api/v2/organizations/{id}/members/{userId}"),
        ("get", "/api/v2/me/organizations"),
        // Part E — provider media control & realtime
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/mute",
        ),
        ("post", "/api/v2/conferences/{id}/realtime/ticket"),
        ("get", "/api/v2/conferences/{id}/realtime"),
        ("get", "/api/v2/conferences/{id}/events"),
        // Part F — media controls & collaboration
        (
            "post",
            "/api/v2/conferences/{id}/screen-share/{participantId}/stop",
        ),
        ("post", "/api/v2/conferences/{id}/spotlights"),
        (
            "delete",
            "/api/v2/conferences/{id}/spotlights/{participantId}",
        ),
        ("patch", "/api/v2/conferences/{id}/collaboration-policy"),
        ("patch", "/api/v2/conferences/{id}/chat-policy"),
        ("get", "/api/v2/conferences/{id}/collaboration"),
        ("post", "/api/v2/conferences/{id}/media-credential"),
        ("get", "/api/v2/conferences/{id}/messages"),
        ("post", "/api/v2/conferences/{id}/messages"),
        ("delete", "/api/v2/conferences/{id}/messages/{messageId}"),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/chat-restriction",
        ),
        (
            "delete",
            "/api/v2/conferences/{id}/participants/{participantId}/chat-restriction",
        ),
        ("post", "/api/v2/conferences/{id}/reactions"),
        ("post", "/api/v2/conferences/{id}/hand-raise"),
        ("delete", "/api/v2/conferences/{id}/hand-raise"),
        (
            "delete",
            "/api/v2/conferences/{id}/participants/{participantId}/hand-raise",
        ),
        ("get", "/api/v2/conferences/{id}/hand-raise"),
        ("get", "/api/v2/conferences/{id}/pre-join"),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/audio-lock",
        ),
        (
            "delete",
            "/api/v2/conferences/{id}/participants/{participantId}/audio-lock",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/video-lock",
        ),
        (
            "delete",
            "/api/v2/conferences/{id}/participants/{participantId}/video-lock",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/screen-share-lock",
        ),
        (
            "delete",
            "/api/v2/conferences/{id}/participants/{participantId}/screen-share-lock",
        ),
        ("get", "/api/v2/conferences/{id}/breakouts"),
        ("post", "/api/v2/conferences/{id}/breakouts"),
        ("post", "/api/v2/conferences/{id}/breakouts/assign"),
        ("post", "/api/v2/conferences/{id}/breakouts/move"),
        (
            "patch",
            "/api/v2/conferences/{id}/breakouts/{breakoutId}/assignments",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/breakouts/{breakoutId}/open",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/breakouts/{breakoutId}/enter",
        ),
        ("post", "/api/v2/conferences/{id}/breakouts/leave"),
        ("post", "/api/v2/conferences/{id}/breakouts/announcement"),
        (
            "post",
            "/api/v2/conferences/{id}/breakouts/{breakoutId}/announcement",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/breakouts/{breakoutId}/close",
        ),
        ("post", "/api/v2/conferences/{id}/breakouts/close-all"),
        ("post", "/api/v2/conferences/{id}/polls"),
        ("get", "/api/v2/conferences/{id}/polls"),
        ("get", "/api/v2/conferences/{id}/polls/{pollId}"),
        ("post", "/api/v2/conferences/{id}/polls/{pollId}/responses"),
        ("get", "/api/v2/conferences/{id}/polls/{pollId}/results"),
        ("post", "/api/v2/conferences/{id}/questions"),
        ("get", "/api/v2/conferences/{id}/questions"),
        (
            "post",
            "/api/v2/conferences/{id}/questions/{questionId}/vote",
        ),
        (
            "delete",
            "/api/v2/conferences/{id}/questions/{questionId}/vote",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/questions/{questionId}/answer",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/questions/{questionId}/dismiss",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/questions/{questionId}/archive",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/questions/{questionId}/pin",
        ),
        (
            "delete",
            "/api/v2/conferences/{id}/questions/{questionId}/pin",
        ),
        ("post", "/api/v2/conferences/{id}/polls/{pollId}/open"),
        ("post", "/api/v2/conferences/{id}/polls/{pollId}/close"),
        ("post", "/api/v2/conferences/{id}/polls/{pollId}/archive"),
        // Parts G–I — recording, artefacts, consent, transcription, intelligence
        ("post", "/api/v2/conferences/{id}/recordings"),
        ("get", "/api/v2/conferences/{id}/recordings"),
        ("get", "/api/v2/conferences/{id}/recordings/{recordingId}"),
        (
            "post",
            "/api/v2/conferences/{id}/recordings/{recordingId}/stop",
        ),
        ("get", "/api/v2/conferences/{id}/artifacts"),
        ("post", "/api/v2/conferences/{id}/artifacts/reports"),
        // Part J — attendance contract
        ("get", "/api/v2/conferences/{id}/attendance"),
        ("get", "/api/v2/conferences/{id}/artifacts/{artifactId}"),
        ("delete", "/api/v2/conferences/{id}/artifacts/{artifactId}"),
        (
            "post",
            "/api/v2/conferences/{id}/artifacts/{artifactId}/access",
        ),
        ("get", "/api/v2/conferences/{id}/consents/requirements"),
        ("post", "/api/v2/conferences/{id}/consents"),
        (
            "post",
            "/api/v2/conferences/{id}/consents/{consentType}/withdraw",
        ),
        ("post", "/api/v2/conferences/{id}/transcription"),
        (
            "post",
            "/api/v2/conferences/{id}/transcription/{jobId}/stop",
        ),
        ("get", "/api/v2/conferences/{id}/transcripts"),
        ("get", "/api/v2/conferences/{id}/transcripts/{artifactId}"),
        (
            "post",
            "/api/v2/conferences/{id}/transcripts/{artifactId}/translations",
        ),
        ("post", "/api/v2/conferences/{id}/intelligence/requests"),
        // Part H — profiles, registration, backstage
        ("get", "/api/v2/meeting-spaces/{id}/profile-policy"),
        ("patch", "/api/v2/meeting-spaces/{id}/profile-policy"),
        ("post", "/api/v2/meeting-spaces/{id}/registrations"),
        ("get", "/api/v2/meeting-spaces/{id}/registrations"),
        (
            "get",
            "/api/v2/meeting-spaces/{id}/registrations/{registrationId}",
        ),
        (
            "delete",
            "/api/v2/meeting-spaces/{id}/registrations/{registrationId}",
        ),
        ("post", "/api/v2/conferences/{id}/backstage"),
        ("delete", "/api/v2/conferences/{id}/backstage"),
        // Part I — quality
        ("post", "/api/v2/conferences/{id}/quality/samples"),
        ("get", "/api/v2/conferences/{id}/quality"),
    ];
    for (method, path) in expected {
        assert!(
            paths.get(path).and_then(|p| p.get(method)).is_some(),
            "{method} {path} undocumented"
        );
    }
    let documented_v2 = paths
        .iter()
        .filter(|(p, _)| p.starts_with("/api/v2"))
        .map(|(_, item)| item.as_object().unwrap().len())
        .sum::<usize>();
    assert_eq!(
        documented_v2,
        expected.len(),
        "every documented v2 operation is in the route table"
    );
}

#[test]
fn contract_never_describes_secrets_or_provider_ids() {
    let doc = generated();
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    let forbidden = [
        "passcodeHash",
        "passcode_hash",
        "tokenHash",
        "token_hash",
        "credentialHash",
        "credential_hash",
        "rtcRoomName",
        "rtc_room_name",
        "rtcRoomId",
        "rtc_room_id",
        "guestIdentity",
        "denialInternalNote",
    ];
    for (name, schema) in schemas {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for f in forbidden {
                assert!(!props.contains_key(f), "schema {name} exposes {f}");
            }
        }
    }
}

#[test]
fn public_error_codes_are_published() {
    let doc = generated();
    let codes: Vec<&str> = doc["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for required in [
        "MEETING_NOT_FOUND",
        "MEETING_ACCESS_DENIED",
        "MEETING_PASSCODE_REQUIRED",
        "MEETING_PASSCODE_INVALID",
        "INVITATION_INVALID",
        "INVITATION_EXPIRED",
        "CONFERENCE_NOT_LIVE",
        "CONFERENCE_LOCKED",
        "WAITING_ROOM_REQUIRED",
        "PARTICIPANT_NOT_FOUND",
        "PARTICIPANT_BANNED",
        "PERMISSION_DENIED",
        "CONFERENCE_STATE_CONFLICT",
        "RTC_PROVIDER_UNAVAILABLE",
        "RECORDING_NOT_ALLOWED",
        "REVOCATION_CHECK_UNAVAILABLE",
    ] {
        assert!(codes.contains(&required), "{required} missing");
    }
}

// ---------------------------------------------------------------------------
// Part J — automated route ↔ OpenAPI parity and contract hygiene
// ---------------------------------------------------------------------------

/// `(method, normalized path)` with every path parameter replaced by `{}`.
fn normalize(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if seg.starts_with(':')
                || seg.starts_with('*')
                || (seg.starts_with('{') && seg.ends_with('}'))
            {
                "{}"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Every `(method, path)` registered in `src/routes/v2.rs`, read from the route table
/// itself (not from a hand-maintained list).
fn registered_v2_routes() -> BTreeSet<(String, String)> {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/routes/v2.rs"))
        .expect("route table");
    let route_re = regex::Regex::new(r#"\.route\(\s*"([^"]+)",\s*"#).unwrap();
    let method_re = regex::Regex::new(r"(?:^|[^A-Za-z_:])(get|post|put|patch|delete)\(").unwrap();
    let mut out = BTreeSet::new();
    let starts: Vec<_> = route_re.captures_iter(&src).collect();
    for cap in &starts {
        let whole = cap.get(0).unwrap();
        // The method router expression runs until the parenthesis closing `.route(`.
        let mut depth = 1i32;
        let mut end = whole.end();
        for (i, ch) in src[whole.end()..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = whole.end() + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let expr = &src[whole.end()..end];
        let path = format!("/api/v2{}", &cap[1]);
        let methods: Vec<_> = method_re
            .captures_iter(expr)
            .map(|m| m[1].to_string())
            .collect();
        assert!(!methods.is_empty(), "no method parsed for {path}: {expr}");
        for m in methods {
            out.insert((m, normalize(&path)));
        }
    }
    assert!(
        out.len() > 100,
        "route table parse looks wrong: {}",
        out.len()
    );
    out
}

fn documented_v2_operations(doc: &Value) -> BTreeSet<(String, String)> {
    doc["paths"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(p, _)| p.starts_with("/api/v2"))
        .flat_map(|(p, item)| {
            item.as_object()
                .unwrap()
                .keys()
                .map(move |m| (m.clone(), normalize(p)))
        })
        .collect()
}

/// J-E2E-06: every registered public v2 operation is documented and every documented
/// v2 operation is registered (no phantom contract, no undocumented route).
#[test]
fn j_e2e_06_route_table_and_openapi_are_identical() {
    let doc = generated();
    let routes = registered_v2_routes();
    let documented = documented_v2_operations(&doc);
    let undocumented: Vec<_> = routes.difference(&documented).collect();
    let phantom: Vec<_> = documented.difference(&routes).collect();
    assert!(
        undocumented.is_empty(),
        "registered but undocumented: {undocumented:?}"
    );
    assert!(phantom.is_empty(), "documented but not routed: {phantom:?}");
}

fn collect_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "$ref" {
                    out.push(x.as_str().unwrap_or_default().to_string());
                } else {
                    collect_refs(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_refs(x, out)),
        _ => {}
    }
}

/// J-E2E-07: no dangling `$ref`, unique operation ids, every v2 operation declares a
/// security requirement, JSON content types and at least one standard error response.
#[test]
fn j_e2e_07_contract_is_resolvable_and_consistent() {
    let doc = generated();
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    let mut refs = vec![];
    collect_refs(&doc, &mut refs);
    for r in &refs {
        let name = r
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("unexpected ref target {r}"));
        assert!(schemas.contains_key(name), "dangling ref {r}");
    }
    let mut ids = HashSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let id = op["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no operationId"));
            assert!(ids.insert(id.to_string()), "duplicate operationId {id}");
            if !path.starts_with("/api/v2") {
                continue;
            }
            assert!(
                id.starts_with("v2_"),
                "{method} {path}: v2 operationId {id}"
            );
            assert!(
                op["security"]
                    .as_array()
                    .map(|a| !a.is_empty())
                    .unwrap_or(false),
                "{method} {path} declares no security requirement"
            );
            if let Some(body) = op.get("requestBody") {
                let types: Vec<_> = body["content"].as_object().unwrap().keys().collect();
                assert_eq!(
                    types,
                    ["application/json"],
                    "{method} {path} request content"
                );
                assert!(
                    method != "get" && method != "delete",
                    "{method} {path} must not take a body"
                );
            }
            let responses = op["responses"].as_object().unwrap();
            for (status, r) in responses {
                if let Some(content) = r.get("content").and_then(Value::as_object) {
                    let types: Vec<_> = content.keys().collect();
                    assert_eq!(types, ["application/json"], "{method} {path} {status}");
                }
            }
            let has_error = responses.iter().any(|(status, r)| {
                (status.starts_with('4') || status.starts_with('5'))
                    && r.to_string().contains("#/components/schemas/ErrorEnvelope")
            });
            assert!(
                has_error,
                "{method} {path} documents no standard error response"
            );
        }
    }
}

/// J-E2E-07 (enums): every published enum is a non-empty set of unique, stable
/// SCREAMING_SNAKE_CASE (or documented lowercase legacy) values; the error envelope
/// references the published `ErrorCode` enum.
#[test]
fn j_enum_values_are_valid_and_error_codes_are_typed() {
    let doc = generated();
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    let stable = regex::Regex::new(r"^[A-Z][A-Z0-9_]*$|^[a-z][a-z0-9_]*$").unwrap();
    let mut enums = 0;
    let mut stack: Vec<&Value> = schemas.values().collect();
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(m) => {
                if let Some(values) = m.get("enum").and_then(Value::as_array) {
                    enums += 1;
                    assert!(!values.is_empty(), "empty enum");
                    let mut seen = HashSet::new();
                    for x in values {
                        let x = x
                            .as_str()
                            .unwrap_or_else(|| panic!("non-string enum value {x}"));
                        assert!(stable.is_match(x), "unstable enum value {x}");
                        assert!(seen.insert(x), "duplicate enum value {x}");
                    }
                }
                stack.extend(m.values());
            }
            Value::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    assert!(enums > 20, "enums found: {enums}");
    let code = &schemas["ErrorBody"]["properties"]["code"];
    assert!(
        code.to_string().contains("#/components/schemas/ErrorCode"),
        "ErrorBody.code must reference ErrorCode: {code}"
    );
    let required: Vec<_> = schemas["ErrorBody"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for r in ["code", "message"] {
        assert!(required.contains(&r), "ErrorBody.{r} required");
    }
    let codes: Vec<&str> = schemas["ErrorCode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for c in [
        "ENDPOINT_NOT_FOUND",
        "METHOD_NOT_ALLOWED",
        "LEGACY_WRITE_DISABLED",
        "SERVICE_UNAVAILABLE",
        "DATABASE_ERROR",
        "INTERNAL_ERROR",
        "VALIDATION_ERROR",
    ] {
        assert!(codes.contains(&c), "{c} missing from ErrorCode");
    }
}

/// J-E2E-10: no provider / storage / internal secret appears anywhere in the public
/// contract (schemas, descriptions, examples), and internal-only routes and provider
/// callback schemas are not published as consumer API.
#[test]
fn j_e2e_10_no_secret_or_internal_surface_in_public_contract() {
    let doc = generated();
    for path in doc["paths"].as_object().unwrap().keys() {
        assert!(
            !path.starts_with("/internal"),
            "internal route published: {path}"
        );
    }
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    for internal in [
        "ProviderTranscriptionEvent",
        "CallbackAck",
        "IncidentView",
        "IncidentListResponse",
        "ConferenceHealthSummary",
        "ConferenceHealthListResponse",
        "OutboxStatus",
    ] {
        assert!(
            !schemas.contains_key(internal),
            "internal schema {internal} is public"
        );
    }
    let forbidden_props = [
        "apiSecret",
        "api_secret",
        "apiKey",
        "secretKey",
        "accessKey",
        "access_key",
        "secretAccessKey",
        "bucket",
        "storageKey",
        "storage_key",
        "objectKey",
        "providerRecordingId",
        "provider_ref",
        "providerRef",
        "egressId",
        "webhookSecret",
        "databaseUrl",
        "dsn",
        "passwordHash",
        "password_hash",
        "requestDigest",
    ];
    let mut stack: Vec<&Value> = schemas.values().collect();
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(m) => {
                if let Some(props) = m.get("properties").and_then(Value::as_object) {
                    for f in forbidden_props {
                        assert!(!props.contains_key(f), "contract exposes {f}");
                    }
                }
                stack.extend(m.values());
            }
            Value::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    // Literal secret material never appears in descriptions or examples.
    let text = doc.to_string();
    for needle in [
        "test-jwt-secret",
        "mock-secret",
        "AKIA",
        "postgres://",
        "redis://",
        "BEGIN PRIVATE KEY",
    ] {
        assert!(!text.contains(needle), "contract text contains {needle}");
    }
}

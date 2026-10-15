//! §84: Privacy Relay's metrics never carry an identifier, a number, a reference or a
//! secret — on ANY series, not just the ones a feature test happens to look at.
//!
//! * Source guard: every call of the metrics API in `src/` (`metrics::inc / add`) names a
//!   `privacy_relay_*` metric literal and passes label KEYS from a fixed allow-list and
//!   label VALUES that are string literals or bounded `&'static str` expressions (enum
//!   `as_str()`, bounded code variables, `if` between such) — never `format!`,
//!   `to_string`, a leaked string, or an id / uuid / key / url variable.
//! * Runtime guard: representative flows run (contexts, sessions, authorization, relay
//!   number assignment / release / cooldown, inbound resolution, contact resolution
//!   refusal, outbox publication and dead letter, enforcement, retention, incidents) with
//!   recognizable canary identifiers, and the WHOLE rendered scrape is checked: every key
//!   is allowed, and no value looks like a UUID, a telephone number, a long digit run, an
//!   event / correlation reference, an external reference, a provider binding or a key
//!   version.
//! * The scrape is served by its own listener router, never by the service router.

mod common;

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use cf_service_auth::ApplicationEnvironmentKind;
use common::{at, iso, Consumer, Harness};
use http_body_util::BodyExt;
use privacy_relay::{
    events::EventPublisher,
    retention::{worker::RetentionWorker, RetentionSettings},
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

/// Every label key a Privacy Relay metric may carry (bounded vocabularies only).
const ALLOWED_LABEL_KEYS: &[&str] = &[
    "action",
    "category",
    "data_class",
    "decision",
    "event",
    "kind",
    "outcome",
    "reason",
    "route",
    "status",
];

/// Identifier fragments that may never name a label value variable.
const FORBIDDEN_VALUE_WORDS: &[&str] = &[
    "id",
    "uuid",
    "key",
    "url",
    "uri",
    "number",
    "phone",
    "e164",
    "secret",
    "token",
    "digit",
    "contact",
    "address",
    "host",
    "email",
    "name",
    "path",
    "body",
    "payload",
    "reference",
    "binding",
    "version",
    "tenant",
    "principal",
    "session",
    "context",
];

/// Constructs that turn data into a label value.
const FORBIDDEN_VALUE_CONSTRUCTS: &[&str] = &[
    "format!",
    "to_string",
    "to_owned",
    "String",
    "leak",
    "Box::",
    "concat",
    "as_ref",
    "&*",
    "into()",
    "unwrap",
];

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The text between the parenthesis opened at `start - 1` and its match.
fn balanced(text: &str, start: usize) -> &str {
    let mut depth = 1;
    let mut in_str = false;
    let bytes = text.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'(' | b'[' | b'{' if !in_str => depth += 1,
            b')' | b']' | b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return &text[start..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unbalanced metric call");
}

/// Splits `text` at top-level commas.
fn top_level(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut in_str, mut current) = (0i32, false, String::new());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_str => {
                current.push(c);
                if let Some(n) = chars.next() {
                    current.push(n);
                }
                continue;
            }
            '"' => in_str = !in_str,
            '(' | '[' | '{' if !in_str => depth += 1,
            ')' | ']' | '}' if !in_str => depth -= 1,
            ',' if !in_str && depth == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn is_literal(v: &str) -> bool {
    v.len() >= 2 && v.starts_with('"') && v.ends_with('"') && !v[1..v.len() - 1].contains('"')
}

/// The `{ .. }` branch bodies of an `if c { a } else { b }` label value (the condition
/// is not a value and is not checked).
fn branches(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, c) in value.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    out.push(value[start..i].trim().to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// Why a label value expression is not a bounded `&'static str`, if it is not.
fn value_problem(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("if ") {
        let arms = branches(value);
        if arms.len() < 2 {
            return Some(format!("unparsed conditional label value: {value}"));
        }
        return arms.iter().find_map(|arm| value_problem(arm));
    }
    if is_literal(value) {
        let inner = &value[1..value.len() - 1];
        return (!inner.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
        }))
        .then(|| format!("literal {value} is not a bounded code"));
    }
    for construct in FORBIDDEN_VALUE_CONSTRUCTS {
        if value.contains(construct) {
            return Some(format!("`{construct}` in a label value"));
        }
    }
    // Only identifiers, field access, `as_str()`, string literals and `if` / `else`.
    let mut rest = value.to_string();
    rest = rest.replace("as_str()", " ");
    for token in rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '"')) {
        if token.is_empty() || is_literal(token) {
            continue;
        }
        if token.starts_with('"') {
            return Some(format!("unbounded literal in {value}"));
        }
        if !token
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Some(format!("`{token}` is not a bounded value"));
        }
        for word in token.split('_') {
            if FORBIDDEN_VALUE_WORDS.contains(&word) {
                return Some(format!("`{token}` looks like an identifier / secret"));
            }
        }
    }
    if value.contains('(') && !value.contains("as_str()") {
        return Some(format!("call in a label value: {value}"));
    }
    None
}

/// (file, metric call text) of every metrics API call in `src/`.
fn metric_calls() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    let mut calls = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        if rel == "src/metrics.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for api in ["inc(", "add("] {
            let needle = format!("metrics::{api}");
            let mut from = 0;
            while let Some(at) = text[from..].find(&needle) {
                let start = from + at + needle.len();
                calls.push((rel.clone(), balanced(&text, start).to_string()));
                from = start;
            }
        }
    }
    calls
}

fn check_call(file: &str, call: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let args = top_level(call);
    let name = args.first().map(String::as_str).unwrap_or_default();
    if !(is_literal(name) && name[1..].starts_with("privacy_relay_")) {
        problems.push(format!(
            "{file}: metric name must be a privacy_relay_* literal: {name}"
        ));
    }
    let Some(labels) = args.get(1) else {
        problems.push(format!("{file}: no label slice: {call}"));
        return problems;
    };
    let Some(inner) = labels.strip_prefix("&[").and_then(|l| l.strip_suffix(']')) else {
        problems.push(format!(
            "{file}: labels must be an inline `&[..]` slice: {labels}"
        ));
        return problems;
    };
    for pair in top_level(inner) {
        let Some(pair) = pair.strip_prefix('(').and_then(|p| p.strip_suffix(')')) else {
            problems.push(format!("{file}: label is not a (key, value) tuple: {pair}"));
            continue;
        };
        let kv = top_level(pair);
        let (key, value) = (kv[0].as_str(), kv.get(1).map(String::as_str).unwrap_or(""));
        if !(is_literal(key) && ALLOWED_LABEL_KEYS.contains(&&key[1..key.len() - 1])) {
            problems.push(format!("{file}: label key {key} is not on the allow-list"));
        }
        if let Some(problem) = value_problem(value) {
            problems.push(format!("{file}: {key}: {problem}"));
        }
    }
    problems
}

#[test]
fn every_metric_call_uses_allowed_keys_and_bounded_values() {
    let calls = metric_calls();
    assert!(calls.len() >= 15, "metric calls not found: {}", calls.len());
    let problems: Vec<String> = calls
        .iter()
        .flat_map(|(file, call)| check_call(file, call))
        .collect();
    assert!(
        problems.is_empty(),
        "unbounded metric labels:\n{}",
        problems.join("\n")
    );
}

/// The guard itself refuses the shapes it exists for (and accepts the bounded ones).
#[test]
fn the_source_guard_rejects_identifier_labels() {
    for bad in [
        r#""privacy_relay_x_total", &[("tenant", "A")]"#,
        r#""privacy_relay_x_total", &[("reason", Box::leak(tenant_id.to_string().into_boxed_str()))]"#,
        r#""privacy_relay_x_total", &[("reason", format!("{context_id}").leak())]"#,
        r#""privacy_relay_x_total", &[("reason", relay_session_id)]"#,
        r#""privacy_relay_x_total", &[("reason", contact_reference)]"#,
        r#""privacy_relay_x_total", &[("reason", key_version)]"#,
        r#""privacy_relay_x_total", &[("reason", "+447946000500")]"#,
        r#""privacy_relay_x_total", labels"#,
        r#""calling_x_total", &[]"#,
        r#"name, &[]"#,
    ] {
        assert!(!check_call("x.rs", bad).is_empty(), "accepted: {bad}");
    }
    for good in [
        r#""privacy_relay_x_total", &[]"#,
        r#""privacy_relay_x_total", &[("event", "CREATED")]"#,
        r#""privacy_relay_x_total", &[("reason", e.code().as_str()), ("route", req.route.as_str())]"#,
        r#""privacy_relay_x_total", &[("kind", if permanent { "PERMANENT" } else { "EXHAUSTED" })]"#,
        r#""privacy_relay_x_total", &[("reason", code), ("kind", kind)]"#,
    ] {
        assert!(
            check_call("x.rs", good).is_empty(),
            "refused: {good}: {:?}",
            check_call("x.rs", good)
        );
    }
}

/// Every `name{k="v",...}` series of the rendered registry: (name, [(key, value)]).
fn rendered_series(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let series = line.split_whitespace().next().unwrap();
        let (name, labels) = match series.split_once('{') {
            Some((n, rest)) => (n, rest.trim_end_matches('}')),
            None => (series, ""),
        };
        let mut pairs = Vec::new();
        let mut rest = labels;
        while let Some(eq) = rest.find("=\"") {
            let key = rest[..eq].trim_start_matches(',').to_string();
            let after = &rest[eq + 2..];
            let close = after.find('"').expect("closed label value");
            pairs.push((key, after[..close].to_string()));
            rest = &after[close + 1..];
        }
        out.push((name.to_string(), pairs));
    }
    out
}

fn unsafe_value(v: &str) -> Option<&'static str> {
    let lower = v.to_ascii_lowercase();
    let digits_run = v
        .split(|c: char| !c.is_ascii_digit())
        .map(str::len)
        .max()
        .unwrap_or(0);
    let hex_run = v
        .split(|c: char| !c.is_ascii_hexdigit())
        .map(str::len)
        .max()
        .unwrap_or(0);
    if Uuid::parse_str(v).is_ok()
        || v.split(['-', '_', ':', '/'])
            .any(|p| Uuid::parse_str(p).is_ok())
        || hex_run >= 32
    {
        Some("uuid")
    } else if digits_run >= 6 {
        Some("6+ digits / telephone number")
    } else if lower.contains("sip:") {
        Some("sip uri")
    } else if lower.contains("http://") || lower.contains("https://") {
        Some("url")
    } else if lower.contains("evt_") || lower.contains("corr_") || lower.contains("req_") {
        Some("event / correlation / request reference")
    } else if v.contains('+') || v.contains('@') || v.contains(' ') {
        Some("address-like value")
    } else if v.starts_with("ref-")
        || v.starts_with("pb-")
        || v.starts_with("ck-")
        || v.starts_with("fk-")
    {
        Some("reference / binding / key version")
    } else {
        None
    }
}

fn assert_registry_is_bounded(text: &str) {
    let series = rendered_series(text);
    assert!(!series.is_empty());
    let mut problems = Vec::new();
    for (name, labels) in &series {
        if !name.starts_with("privacy_relay_") {
            problems.push(format!("{name}: not a privacy_relay_* metric"));
        }
        for (key, value) in labels {
            if !ALLOWED_LABEL_KEYS.contains(&key.as_str()) {
                problems.push(format!("{name}: label key `{key}` is not allowed"));
            }
            if let Some(why) = unsafe_value(value) {
                problems.push(format!("{name}{{{key}=\"{value}\"}}: {why}"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "unsafe metric labels:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_runtime_guard_rejects_identifier_values() {
    for bad in [
        "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
        "3f2504e04f8911d39a0c0305e82c3301",
        "+447946000500",
        "2079460123",
        "evt_0123",
        "corr_abc",
        "ref-case-1",
        "pb-metrics",
        "ck-test-1",
        "a@b",
    ] {
        assert!(unsafe_value(bad).is_some(), "{bad}");
    }
    for good in ["CREATED", "CONTEXT_EXPIRED", "PUBLIC_RELAY", "MALFORMED"] {
        assert!(unsafe_value(good).is_none(), "{good}");
    }
}

/// A transport refusing every event permanently (dead letter series).
struct Malformed;

#[async_trait]
impl EventPublisher for Malformed {
    async fn publish(&self, _: &str, _: &Value) -> Result<(), &'static str> {
        Err("MALFORMED")
    }
}

const RELAY: &str = "+44 20 7946 0500";
const CONTACT: &str = "+44 20 7946 0123";

/// Representative flows populate the registry; then every series is checked.
#[tokio::test]
async fn the_rendered_registry_never_carries_identifiers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    h.add_relay_number(RELAY, "pb-metrics").await;
    let principal = Uuid::new_v4();
    let contact = h.create_contact(&c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &contact, "VERIFIED", None).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let context = ctx["contextId"].as_str().unwrap().to_string();
    let session = h
        .create_session(
            &c,
            &context,
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        h.assign_relay_number(&c, &session).await.status,
        StatusCode::CREATED
    );
    // Authorization decisions (authorized and denied).
    for route in ["CALLBACK_RELAY", "APPLICATION"] {
        let r = h
            .post(
                "/internal/v1/communication/authorize",
                &c,
                json!({ "relaySessionId": session, "source": { "principalId": principal },
                        "targets": [{ "contactReferenceId": contact }], "channel": "VOICE",
                        "route": route, "purpose": "CALL_CREATE" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    // Inbound resolution (resolved, then unknown number).
    for called in [RELAY, "+44 20 7946 0999"] {
        h.telephony(
            "/internal/v1/relay-routing/resolve-inbound",
            json!({ "calledRelayIdentity": called, "providerObservedAt": iso(at(0)) }),
        )
        .await;
    }
    // A refused contact resolution (bogus grant).
    let telephony = h.ids.telephony.clone();
    h.send_as(
        &telephony,
        Method::POST,
        &format!("/internal/v1/contacts/{contact}/resolve-for-execution"),
        Some(&c),
        Some(
            json!({ "grantId": Uuid::new_v4(), "grantSecret": "not-a-grant-secret-value",
                     "callId": Uuid::new_v4(), "callLegId": Uuid::new_v4(),
                     "executionAttemptId": Uuid::new_v4() }),
        ),
    )
    .await;
    // Revoke (session cascade, relay release + cooldown, enforcement).
    assert_eq!(
        h.lifecycle(&c, &context, "revoke", 1).await.status,
        StatusCode::OK
    );
    // Expiry, cooldown completion, outbox dead letter (incident), retention.
    let expiring = h.bidirectional(&c, &[Uuid::new_v4(), Uuid::new_v4()]).await;
    h.app_session(&c, expiring["contextId"].as_str().unwrap())
        .await;
    h.clock.set(at(24 * 60 * 30));
    h.relay.run_sweep().await.unwrap();
    h.relay
        .clone()
        .with_event_publisher(Arc::new(Malformed))
        .publish_outbox(1000)
        .await
        .unwrap();
    RetentionWorker::new(
        h.conn().clone(),
        Arc::new(h.clock.clone()),
        RetentionSettings::from_lookup(&|_: &str| None).unwrap(),
    )
    .retention_tick(100)
    .await
    .unwrap();

    let text = h.relay.metrics_text().await.unwrap();
    for expected in [
        "privacy_relay_context_events_total{event=\"CREATED\"}",
        "privacy_relay_context_events_total{event=\"REVOKED\"}",
        "privacy_relay_context_events_total{event=\"EXPIRED\"}",
        "privacy_relay_session_events_total{event=\"CREATED\"}",
        "privacy_relay_session_events_total{event=\"REVOKED\"}",
        "privacy_relay_relay_number_events_total{event=\"ASSIGNED\"}",
        "privacy_relay_relay_number_events_total{event=\"RELEASED\",reason=\"SESSION_REVOKED\"}",
        "privacy_relay_relay_number_events_total{event=\"COOLDOWN_STARTED\"}",
        "privacy_relay_relay_number_events_total{event=\"COOLDOWN_COMPLETED\"}",
        "privacy_relay_authorization_decisions_total{decision=\"AUTHORIZED\",reason=\"AUTHORIZED\",route=\"CALLBACK_RELAY\"}",
        "privacy_relay_inbound_resolutions_total{outcome=\"RESOLVED\",reason=\"RESOLVED\"}",
        "privacy_relay_inbound_resolutions_total{outcome=\"NO_ACTIVE_RELAY_MAPPING\",reason=\"UNKNOWN_RELAY_IDENTITY\"}",
        "privacy_relay_contact_resolution_failures_total{reason=",
        "privacy_relay_enforcement_attempts_total{outcome=\"APPLIED\"}",
        "privacy_relay_outbox_dead_lettered_total{reason=\"MALFORMED\",kind=\"PERMANENT\"}",
        "privacy_relay_incidents_raised_total{category=\"OUTBOX_DEAD_LETTER\"}",
        "privacy_relay_outbox_pending ",
        "privacy_relay_outbox_oldest_pending_age_seconds ",
        "privacy_relay_outbox_dead_letter ",
        "privacy_relay_enforcement_oldest_pending_age_seconds ",
        "privacy_relay_relay_numbers{status=\"AVAILABLE\"} 1",
        "privacy_relay_open_incidents{category=\"OUTBOX_DEAD_LETTER\"} 1",
    ] {
        assert!(text.contains(expected), "{expected} missing:\n{text}");
    }
    assert_registry_is_bounded(&text);
    // Canary identifiers of these flows never appear anywhere in the scrape.
    for canary in [
        context.as_str(),
        session.as_str(),
        contact.as_str(),
        &principal.to_string(),
        &c.tenant_id.to_string(),
        "7946",
        "pb-metrics",
        "ck-test-1",
        "ref-",
    ] {
        assert!(!text.contains(canary), "{canary} in the scrape");
    }

    // Served on its own listener router; the service router has no /metrics.
    let r = privacy_relay::metrics::router(h.relay.clone())
        .oneshot(
            Request::get("/metrics")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let body = r.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("privacy_relay_outbox_pending"));
    for path in ["/metrics", "/internal/v1/metrics"] {
        let r = h
            .raw(Request::builder().method(Method::GET).uri(path), None)
            .await;
        assert_ne!(r.status, StatusCode::OK, "{path} on the service router");
        assert!(!r.text.contains("privacy_relay_"), "{path}");
    }
    let routes =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes.rs"))
            .unwrap();
    assert!(
        !routes.contains("metrics"),
        "the API router never mounts metrics"
    );
}

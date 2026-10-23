//! Part H §72.6: Calling's metrics never carry an identifier, a number, an address or a
//! secret — on ANY series, not just the ones a feature test happens to look at.
//!
//! * Source guard: every call of the metrics API in `src/` (`metrics::inc / add /
//!   set_gauge / observe`) names a `calling_*` metric literal and passes label KEYS from a
//!   fixed allow-list and label VALUES that are string literals or bounded `&'static str`
//!   expressions (enum `as_str()`, bounded code variables, `if` between such) — never
//!   `format!`, `to_string`, a leaked string, or an id / uuid / key / url variable.
//! * Runtime guard: representative flows run (application call, public call through the
//!   fake Telephony / Privacy Relay, webhook delivery, limit refusal, block refusal, abuse
//!   report) and the WHOLE rendered registry is checked: every key is allowed and no value
//!   looks like a UUID, a telephone number, a long digit run, a SIP URI, a webhook secret,
//!   a URL or an object key.

mod common;

use std::{path::Path, sync::Arc};

use axum::{http::StatusCode, routing::post, Router};
use calling::{
    domain::EnvironmentKind,
    platform::{limits::LimitValues, scopes::names},
    routing::{
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::spend::SpendPolicy,
    webhooks::{worker::LocalWebhookTransport, WebhookService, WebhookSettings},
};
use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
use common::{
    app::AppWorld,
    privacy::FakePrivacyRelay,
    telephony::{FakePublicMedia, FakeTelephony},
};
use serde_json::json;
use uuid::Uuid;

/// Every label key a Calling metric may carry (bounded vocabularies only). `le` is the
/// histogram bucket bound the renderer adds.
const ALLOWED_LABEL_KEYS: &[&str] = &[
    "action",
    "artifact",
    "blocked",
    "blocker",
    "call_kind",
    "category",
    "channel",
    "condition",
    "data_class",
    "decision",
    "destination",
    "kind",
    "limit",
    "operation",
    "outcome",
    "reason",
    "resource",
    "result",
    "route_class",
    "route_type",
    "source",
    "status",
    "strategy",
    "target_type",
    "type",
    "usage_type",
];

/// Identifier fragments that may never name a label value variable.
const FORBIDDEN_VALUE_WORDS: &[&str] = &[
    "id", "uuid", "key", "url", "uri", "number", "phone", "e164", "secret", "token", "digit",
    "dtmf", "contact", "address", "host", "email", "name", "path", "body", "payload",
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
        for api in ["inc(", "add(", "set_gauge(", "observe("] {
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
    if !(is_literal(name) && name[1..].starts_with("calling_")) {
        problems.push(format!(
            "{file}: metric name must be a calling_* literal: {name}"
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
    assert!(calls.len() > 50, "metric calls not found: {}", calls.len());
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
        r#""calling_x_total", &[("tenant", "A")]"#,
        r#""calling_x_total", &[("result", Box::leak(tenant_id.to_string().into_boxed_str()))]"#,
        r#""calling_x_total", &[("result", format!("{call_id}").leak())]"#,
        r#""calling_x_total", &[("result", call_id)]"#,
        r#""calling_x_total", &[("result", object_key)]"#,
        r#""calling_x_total", &[("result", "+2348012345678")]"#,
        r#""calling_x_total", &[("result", "https://x")]"#,
        r#""calling_x_total", labels"#,
        r#"name, &[]"#,
    ] {
        assert!(!check_call("x.rs", bad).is_empty(), "accepted: {bad}");
    }
    for good in [
        r#""calling_x_total", &[]"#,
        r#""calling_x_total", &[("result", "STORED")]"#,
        r#""calling_x_total", &[("reason", denial.as_str()), ("route_class", spec.route_class)]"#,
        r#""calling_x_total", &[("result", if valid { "VALID" } else { "INVALID" })]"#,
        r#""calling_x_total", &[("category", incident.category.as_str())]"#,
    ] {
        assert!(
            check_call("x.rs", good).is_empty(),
            "refused: {good}: {:?}",
            check_call("x.rs", good)
        );
    }
}

/// Every `name{k="v",...}` series of the rendered registry: (name, [(key, value)]).
fn rendered_series() -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    for line in calling::metrics::render().lines() {
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
    } else if lower.contains("whsec_") {
        Some("webhook secret")
    } else if lower.contains("http://") || lower.contains("https://") {
        Some("url")
    } else if lower.contains("recordings/") || lower.contains("voicemail/") {
        Some("object key")
    } else {
        None
    }
}

fn assert_registry_is_bounded() {
    let series = rendered_series();
    assert!(!series.is_empty());
    let mut problems = Vec::new();
    for (name, labels) in &series {
        if !name.starts_with("calling_") {
            problems.push(format!("{name}: not a calling_* metric"));
        }
        for (key, value) in labels {
            if key != "le" && !ALLOWED_LABEL_KEYS.contains(&key.as_str()) {
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
        "+2348012345678",
        "8012345",
        "sip:alice@example.org",
        "whsec_abc",
        "https://example.org",
        "recordings/a/b",
        "voicemail/x",
    ] {
        assert!(unsafe_value(bad).is_some(), "{bad}");
    }
    for good in [
        "STORED",
        "APP_TO_PSTN",
        "0.5",
        "3600",
        "+Inf",
        "SIGNING_KEY_UNKNOWN",
    ] {
        assert!(unsafe_value(good).is_none(), "{good}");
    }
}

fn webhook_settings() -> WebhookSettings {
    WebhookSettings {
        secret_box: Some(Arc::new(
            SecretBox::from_bytes(&std::array::from_fn::<u8, 32, _>(|i| {
                (i as u8).wrapping_mul(13).wrapping_add(5)
            }))
            .unwrap(),
        )),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        request_timeout: std::time::Duration::from_secs(1),
        max_attempts: 3,
        ..Default::default()
    }
}

/// Representative flows populate the registry; then every series is checked.
#[tokio::test]
async fn the_rendered_registry_never_carries_identifiers() {
    // 1. Public call (app-to-phone) through the fake Privacy Relay / Telephony.
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let media = Arc::new(FakePublicMedia::default());
    let Some(w) =
        AppWorld::with_public_network(privacy.clone(), telephony.clone(), Some(media)).await
    else {
        return;
    };
    privacy.allow(w.w.initiator.id().as_uuid());
    let contact = privacy.add_contact("NG");
    let scope = w.w.scope();
    calling::routing::store::store_policy(
        w.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
        &RoutingPolicy {
            allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::AppToPstn],
            public_calling_enabled: true,
            destinations: DestinationPolicy {
                allow: vec!["NG".into()],
                deny: vec![],
                default: CountryDecision::Deny,
            },
            ..RoutingPolicy::default()
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    calling::telephony::spend::store_policy(
        w.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: scope.environment_id().as_uuid(),
        },
        &SpendPolicy {
            currency: "NGN".into(),
            leg_allowance_minor: 5_000,
            max_leg_seconds: 3600,
            daily_limit_minor: 50_000,
            monthly_limit_minor: 500_000,
            max_concurrent_legs: 5,
            max_originations_per_minute: 20,
            max_per_party_per_hour: 10,
            max_per_principal_per_hour: None,
            max_per_service_account_per_hour: None,
            max_per_context_per_hour: None,
            enabled: true,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let caller = w.device(&w.w.initiator, "metrics-caller").await;
    let r = w
        .post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": w.w.initiator.id(),
                "initiatorEndpointId": caller.endpoint,
                "targetContactReference": contact,
                "privacyRelaySessionId": Uuid::new_v4(),
            }),
            &[("idempotency-key", "metrics-public")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(telephony.originates().len(), 1);
    // A refused destination (country not allowed).
    let r = w
        .post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": w.w.initiator.id(),
                "initiatorEndpointId": caller.endpoint,
                "targetContactReference": privacy.add_contact("US"),
                "privacyRelaySessionId": Uuid::new_v4(),
            }),
            &[("idempotency-key", "metrics-public-denied")],
        )
        .await;
    assert!(!r.status.is_success(), "{}", r.text);

    // 2. Application calls: connected, then ended.
    let (call, _, _) = w.connected_call("VOICE").await;
    let (_, v) = w.state(call).await;
    assert_eq!(w.command(call, "end", v).await.status, StatusCode::OK);

    // 3. Abuse report on that Call by a participant.
    let r = w
        .post(
            &format!("/v1/calls/{call}/abuse-reports"),
            json!({ "principalId": w.w.initiator.id(), "category": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);

    // 5. Webhook delivery (signed POST to a local receiver) and limit notices.
    for scope in [
        names::WEBHOOKS_MANAGE,
        names::WEBHOOKS_READ,
        names::USAGE_READ,
    ] {
        w.h()
            .platform
            .grant_application_scope(w.w.consumer.application.id, scope)
            .await
            .unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/hook", post(|| async { StatusCode::OK })),
        )
        .await
        .unwrap()
    });
    let manager = w
        .h()
        .issue(
            w.w.consumer.sa(EnvironmentKind::Production),
            &[names::WEBHOOKS_MANAGE, names::USAGE_READ, names::CALLS_READ],
        )
        .await;
    let ctx = w.h().ctx(manager.secret.expose_once()).await;
    let webhooks = WebhookService::new(
        w.h().conn().clone(),
        Arc::new(w.h().clock.clone()),
        webhook_settings(),
    );
    webhooks
        .create(
            &ctx,
            &format!("http://127.0.0.1:{port}/hook"),
            &[
                "call.requested".to_string(),
                "call.ended".to_string(),
                "limit.warning".to_string(),
                "limit.exceeded".to_string(),
            ],
            None,
        )
        .await
        .unwrap();
    // A request-rate limit refusal (with its limit notices).
    let limited = w
        .h()
        .issue(
            w.w.consumer.sa(EnvironmentKind::Production),
            &[names::CALLS_READ],
        )
        .await;
    w.h()
        .platform
        .set_credential_limits(
            limited.credential.id,
            LimitValues {
                requests_per_minute: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut statuses = Vec::new();
    for _ in 0..4 {
        statuses.push(w.h().whoami(limited.secret.expose_once()).await.status);
    }
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
    w.connected_call("VOICE").await;
    webhooks
        .publish_outbox_once(&LocalWebhookTransport::new(webhooks.clone()), 1000)
        .await
        .unwrap();
    let report = webhooks.deliver_due_once(100).await.unwrap();
    assert!(report.succeeded > 0, "{report:?}");

    // 6. Block refusal: the recipient blocks the initiator; a new Call is refused.
    let r = w
        .post(
            "/v1/blocks",
            json!({ "principalId": w.w.recipient.id(), "blockedPrincipalId": w.w.initiator.id(),
                     "reasonCode": "UNWANTED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    w.device(&w.w.recipient, "metrics-recipient").await;
    let r = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            "metrics-blocked",
        )
        .await;
    assert!(!r.status.is_success(), "{}", r.text);

    // 7. §84 / §109: a failed authentication, and the worker loop's backlog sample.
    assert_eq!(
        w.h().whoami("cfmk_not_a_credential").await.status,
        StatusCode::UNAUTHORIZED
    );
    w.h().state.app.sample_worker_backlog().await.unwrap();

    // The registry now holds every series these flows emit (and whatever concurrently
    // running tests of this binary emitted): all bounded.
    let rendered = calling::metrics::render();
    for expected in [
        "calling_webhook_deliveries_total",
        "calling_abuse_reports_total",
        "calling_block_rules_total",
        // §84 call lifecycle, RTC admission, worker backlog and authentication series.
        "calling_call_requests_total",
        "calling_call_request_refusals_total",
        "calling_call_setup_seconds",
        "calling_call_ring_seconds",
        "calling_call_connected_seconds",
        "calling_call_outcomes_total",
        "calling_media_admissions_total",
        "calling_outbox_pending",
        "calling_outbox_oldest_pending_age_seconds",
        "calling_telephony_commands_pending",
        "calling_telephony_commands_oldest_pending_age_seconds",
        "calling_telephony_commands_dead",
        "calling_webhook_deliveries_pending",
        "calling_authentication_failures_total",
    ] {
        assert!(
            rendered.contains(expected),
            "{expected} not emitted:\n{rendered}"
        );
    }
    assert_registry_is_bounded();
}

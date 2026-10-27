//! Part VI (Section 53) webhook certification journeys.
//!
//! Every journey starts its own certification stack — the REAL service binaries, real
//! Ed25519 service authentication, one PostgreSQL database per service, real Redis,
//! deterministic adapters — and a local customer endpoint ([`Receiver`]). Domain
//! operations go through the real API Gateway; webhook requests are verified with the
//! reference receiver verifier (`cf_webhooks::signing::verify`). Nothing is stubbed.
//!
//! Runtime: delivering to a loopback receiver is refused outside the `local` runtime by
//! design (`DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` /
//! `CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` are accepted ONLY with
//! `CF_RUNTIME_ENVIRONMENT=local`). The delivery journeys therefore run a stack whose
//! services ALL run in `local` (their own keys and trust bundles for that runtime); no
//! non-local policy is weakened. [`webhook_ssrf_policy_journey`] runs the default `test`
//! runtime and proves on the real processes that private endpoints are refused there.
//!
//! Prerequisites and skip rules are those of `tests/journeys.rs`: TEST_DATABASE_URL,
//! TEST_REDIS_URL and the service binaries (CFMEET_BIN_DIR); without them every journey
//! FAILS unless CFMEET_SKIP_DB_TESTS=1 skips explicitly. One journey at a time.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use base64::Engine;
use cf_service_auth::{services, OutboundServiceRequest, RuntimeEnvironment, ServiceId};
use cfmeet_certification::{
    provision::{self, Auth, Gateway, Resp, Sandbox, SANDBOX_SCOPES},
    receiver::{Received, Receiver, Reply},
    stack::{self, Stack},
    StackConfig, StackOptions,
};
use rand::RngCore;
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

static BUSY: AtomicBool = AtomicBool::new(false);

struct Serial;

impl Serial {
    async fn acquire() -> Serial {
        while BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Serial
    }
}

impl Drop for Serial {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// How a journey's stack routes and delivers webhooks.
#[derive(Debug, Clone, Copy)]
struct Hooks {
    /// `local` runtime (loopback receiver allowed) or the default `test` runtime.
    local: bool,
    /// `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`.
    central: bool,
    /// `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE`.
    dp_mode: &'static str,
    /// `MESSAGING_` / `PRIVACY_RELAY_` / `SMS_GATEWAY_PUBLIC_EVENTS_DESTINATION`.
    producers: &'static str,
    /// `CALLING_PUBLIC_EVENTS_DESTINATION`.
    calling: &'static str,
}

/// Consecutive failed attempts after which the Developer Platform disables an endpoint
/// (the smallest value it accepts).
const AUTO_DISABLE_AFTER: usize = 5;

fn sealing_key() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn options(h: Hooks) -> StackOptions {
    let mut o = StackOptions {
        runtime: Some(if h.local {
            RuntimeEnvironment::Local
        } else {
            RuntimeEnvironment::Test
        }),
        ..StackOptions::default()
    };
    // The Developer Platform API and its worker share the sealing key (the worker opens
    // what the API sealed).
    let key = sealing_key();
    for p in [stack::DEVELOPER_PLATFORM, stack::DEVELOPER_PLATFORM_WORKER] {
        o = o
            .set(p, "DEVELOPER_PLATFORM_WEBHOOKS_ENABLED", "true")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", h.dp_mode)
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY", key.clone())
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID", "cert-k1")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_REQUEST_TIMEOUT_MS", "5000")
            .set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_AUTO_DISABLE_AFTER",
                AUTO_DISABLE_AFTER.to_string(),
            );
        if h.local {
            o = o.set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
                "true",
            );
        }
    }
    if h.local {
        o = o.set(
            stack::CALLING,
            "CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
            "true",
        );
    }
    o.set(
        stack::CALLING,
        "CALLING_PUBLIC_EVENTS_DESTINATION",
        h.calling,
    )
    .set(
        stack::MESSAGING,
        "MESSAGING_PUBLIC_EVENTS_DESTINATION",
        h.producers,
    )
    .set(
        stack::PRIVACY_RELAY,
        "PRIVACY_RELAY_PUBLIC_EVENTS_DESTINATION",
        h.producers,
    )
    .set(
        stack::SMS_GATEWAY,
        "SMS_GATEWAY_PUBLIC_EVENTS_DESTINATION",
        h.producers,
    )
    .set(
        stack::NUMBERING,
        "NUMBERING_PUBLIC_EVENTS_DESTINATION",
        "developer-platform",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED",
        if h.central { "true" } else { "false" },
    )
}

/// Starts a webhook stack (`None`: explicitly skipped). Holds the journey lock.
async fn start(h: Hooks) -> Option<(Serial, Stack)> {
    let config = StackConfig::from_env().expect("certification prerequisites")?;
    let guard = Serial::acquire().await;
    let stack = Stack::start_with(config, options(h))
        .await
        .expect("certification webhook stack starts");
    assert_eq!(
        stack.runtime,
        if h.local {
            RuntimeEnvironment::Local
        } else {
            RuntimeEnvironment::Test
        }
    );
    Some((guard, stack))
}

fn tag() -> String {
    Uuid::new_v4().simple().to_string()[..10].to_string()
}

fn email(role: &str) -> String {
    format!("{role}-{}@cert.example", tag())
}

#[track_caller]
fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

const WAIT: Duration = Duration::from_secs(90);

async fn allow_managed(gw: &Gateway, secret: &str) {
    let cur = gw
        .get("/v1/messaging/policy", Auth::Credential(secret))
        .await;
    status(&cur, 200, "GET /v1/messaging/policy");
    let r = gw
        .request(
            Method::PUT,
            "/v1/messaging/policy",
            Auth::Credential(secret),
            Some(json!({ "expectedVersion": cur.body["version"], "policy": { "contexts": {
                "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } } })),
            &[],
        )
        .await;
    status(&r, 200, "PUT /v1/messaging/policy");
}

/// A MANAGED group conversation owned by `owner` (Messaging learns principals from the
/// identity feed: retried briefly).
async fn conversation(gw: &Gateway, secret: &str, owner: Uuid) -> String {
    let mut last = None;
    for _ in 0..50 {
        let r = gw
            .acting(
                Method::POST,
                "/v1/conversations",
                Auth::Credential(secret),
                owner,
                Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
            )
            .await;
        if r.status == 201 {
            return r.str("/conversationId");
        }
        last = Some(r);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("POST /v1/conversations: {:?}", last.map(|r| r.text));
}

async fn message(gw: &Gateway, secret: &str, owner: Uuid, cid: &str, text: &str) {
    let r = gw
        .acting(
            Method::POST,
            &format!("/v1/conversations/{cid}/messages"),
            Auth::Credential(secret),
            owner,
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": text })),
        )
        .await;
    status(&r, 201, "POST /v1/conversations/{id}/messages");
}

/// Creates a subscription through the gateway; returns (webhook id, signing secret).
async fn subscribe(gw: &Gateway, secret: &str, url: &str, types: &[&str]) -> (Uuid, String) {
    let r = gw
        .post(
            "/v1/webhooks",
            Auth::Credential(secret),
            json!({ "url": url, "eventTypes": types, "description": "certification" }),
        )
        .await;
    status(&r, 201, &format!("POST /v1/webhooks {url}"));
    let signing = r.str("/signingSecret");
    assert!(signing.starts_with("whsec_"), "{}", r.text);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    (r.uuid("/webhook/webhookId"), signing)
}

/// The delivery history of a subscription (customer API).
async fn deliveries(gw: &Gateway, secret: &str, webhook: Uuid) -> Vec<Value> {
    let r = polled(
        gw,
        &format!("/v1/webhooks/{webhook}/deliveries?limit=100"),
        Auth::Credential(secret),
    )
    .await;
    status(&r, 200, "GET /v1/webhooks/{id}/deliveries");
    r.body["data"].as_array().cloned().unwrap_or_default()
}

/// A GET that a journey repeats while it waits: a SANDBOX environment's platform quota
/// (120 requests / minute) is honoured — a 429 waits for its Retry-After and asks again.
async fn polled(gw: &Gateway, path: &str, auth: Auth<'_>) -> Resp {
    for _ in 0..5 {
        let r = gw.get(path, auth).await;
        if r.status != 429 {
            return r;
        }
        let wait: u64 = r
            .header("retry-after")
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        tokio::time::sleep(Duration::from_secs(wait.clamp(1, 61))).await;
    }
    panic!("GET {path}: still rate limited");
}

/// Polls the delivery history until `pred` holds.
async fn deliveries_until(
    gw: &Gateway,
    secret: &str,
    webhook: Uuid,
    what: &str,
    pred: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let d = deliveries(gw, secret, webhook).await;
        if pred(&d) {
            return d;
        }
        if std::time::Instant::now() > deadline {
            panic!("{what}: not observed within {WAIT:?}; deliveries {d:#?}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The attempt history of a delivery (Developer Console route, member session).
async fn attempts(gw: &Gateway, session: &str, environment: Uuid, delivery: &str) -> Vec<Value> {
    let r = gw
        .get(
            &format!("/v1/environments/{environment}/webhook-deliveries/{delivery}/attempts"),
            Auth::Session(session),
        )
        .await;
    status(&r, 200, "GET .../webhook-deliveries/{id}/attempts");
    r.body["items"].as_array().cloned().unwrap_or_default()
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && Uuid::parse_str(s).is_ok()
}

fn is_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// A delivered public event carries only safe projected fields: the envelope keys, and
/// `data` values that are opaque UUIDs, bounded codes or non-negative integers — never a
/// telephone number, provider id, message content, token or secret (`forbidden`).
#[track_caller]
fn assert_safe(r: &Received, forbidden: &[&str]) {
    let v = r.json();
    let obj = v.as_object().expect("a JSON object body");
    for k in obj.keys() {
        assert!(
            [
                "id",
                "type",
                "apiVersion",
                "occurredAt",
                "data",
                "correlationId"
            ]
            .contains(&k.as_str()),
            "unexpected top-level key {k}: {v}"
        );
    }
    assert_eq!(v["apiVersion"], "v1", "{v}");
    assert_eq!(v["id"].as_str(), Some(r.event_id()), "header = body id");
    for (k, val) in v["data"].as_object().expect("data object") {
        let safe = match val {
            Value::String(s) => is_uuid(s) || is_code(s),
            Value::Number(n) => n.as_u64().is_some(),
            _ => false,
        };
        assert!(safe, "unsafe data value {k}={val} in {v}");
    }
    let text = String::from_utf8_lossy(&r.body);
    // No E.164 number of any kind can be present ('+' appears in no safe field).
    assert!(!text.contains('+'), "a '+' (telephone number?) in {text}");
    for f in forbidden.iter().filter(|f| !f.is_empty()) {
        assert!(!text.contains(f), "{f:?} leaked into a webhook: {text}");
    }
}

/// Every resource id (`data` UUIDs) of the events received on `path`.
fn ids_in(rx: &Receiver, path: &str) -> BTreeSet<String> {
    rx.received(path)
        .iter()
        .flat_map(|r| {
            r.json()["data"]
                .as_object()
                .map(|d| {
                    d.values()
                        .filter_map(|v| v.as_str().filter(|s| is_uuid(s)).map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect()
}

fn types_of(got: &[Received]) -> BTreeSet<String> {
    got.iter().map(Received::event_type).collect()
}

/// A Privacy Relay communication context (through Calling's public routes).
async fn relay_context(gw: &Gateway, secret: &str, participants: &[Uuid]) -> (String, i64) {
    let mut last = None;
    for _ in 0..50 {
        let r = gw
            .post(
                "/v1/privacy-relay/contexts",
                Auth::Credential(secret),
                json!({
                    "externalContextReference": format!("ref-{}", tag()),
                    "participants": participants.iter().map(|p| json!({ "principalId": p })).collect::<Vec<_>>(),
                    "allowedChannels": ["VOICE"],
                    "directionPolicy": "BIDIRECTIONAL",
                    "expiresAt": (chrono::Utc::now() + chrono::Duration::hours(1))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                }),
            )
            .await;
        if r.status == 201 || r.status == 200 {
            return (
                r.str("/context/contextId"),
                r.body["context"]["version"].as_i64().unwrap_or(1),
            );
        }
        last = Some(r);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!(
        "POST /v1/privacy-relay/contexts: {:?}",
        last.map(|r| r.text)
    );
}

/// A number of the synthetic inventory reserved and then cancelled (two
/// `number.reservation_changed` facts at least); returns (number resource id, e164).
async fn reserve_and_cancel(gw: &Gateway, secret: &str) -> (String, String, String) {
    let search = gw
        .get(
            "/v1/numbers/search?countryCode=US",
            Auth::Credential(secret),
        )
        .await;
    status(&search, 200, "GET /v1/numbers/search");
    let first = &search.body["data"][0];
    let number = first["numberResourceId"]
        .as_str()
        .unwrap_or_else(|| panic!("no number in {}", search.text))
        .to_string();
    let e164 = first["e164"].as_str().unwrap_or_default().to_string();
    let r = gw
        .post(
            "/v1/numbers/reservations",
            Auth::Credential(secret),
            json!({ "numberResourceId": number }),
        )
        .await;
    assert!(
        r.status == 202 || r.status == 201,
        "POST /v1/numbers/reservations: {}",
        r.text
    );
    let reservation = r.str("/reservationId");
    // The reservation is cancellable once its provider hold settled (RESERVATION_BUSY
    // until then).
    let mut last = None;
    for _ in 0..120 {
        let c = gw
            .post(
                &format!("/v1/numbers/reservations/{reservation}/cancel"),
                Auth::Credential(secret),
                json!({}),
            )
            .await;
        if (200..300).contains(&c.status) {
            last = None;
            break;
        }
        assert!(
            c.status == 409 || c.status == 429,
            "POST /v1/numbers/reservations/{{id}}/cancel: {}",
            c.text
        );
        last = Some(c);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if let Some(c) = last {
        panic!("reservation never cancellable: {}", c.text);
    }
    (number, e164, reservation)
}

/// A platform envelope of Messaging (`cfmeet.platform.event-envelope` v1) for `sb`.
fn envelope(sb: &Sandbox, producer: &str, runtime: &str, conversation: Uuid) -> Value {
    json!({
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "messaging.conversation.created",
        "schema_version": 1,
        "occurred_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "producer_service": producer,
        "runtime_environment": runtime,
        "correlation_id": format!("corr_{}", Uuid::new_v4().simple()),
        "ownership": {
            "tenant_id": sb.customer.tenant_id,
            "application_id": sb.customer.application_id,
            "application_environment_id": sb.customer.environment_id,
        },
        "payload": { "conversation_id": conversation, "conversation_kind": "GROUP",
                     "security_profile": "MANAGED", "version": 1 },
    })
}

/// `POST /internal/v1/platform/events` at the Developer Platform, signed with `as_service`'s
/// REAL key (a receiver-side proof: the ingestion's own checks, not the gateway's).
async fn ingest(stack: &Stack, as_service: &str, body: &Value) -> Resp {
    let signed = stack
        .signer(as_service)
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::developer_platform(),
        ))
        .expect("signs");
    let mut r = stack.http.post(format!(
        "{}/internal/v1/platform/events",
        stack.url(stack::DEVELOPER_PLATFORM)
    ));
    for (k, v) in signed.headers().iter() {
        r = r.header(k.as_str(), v.as_bytes());
    }
    Resp::from(r.json(body).send().await.expect("DP reachable")).await
}

const JOURNEY_TYPES: &[&str] = &[
    "conversation.created",
    "conversation.member_added",
    "message.created",
    "number.reservation_changed",
    "relay.created",
    "relay.revoked",
    "sms.received",
    "sms.delivered",
    "call.ended",
];

// =====================================================================================
// 1. External developer — signed webhook
// =====================================================================================

#[tokio::test]
async fn external_developer_signed_webhook_journey() {
    let Some((_g, stack)) = start(Hooks {
        local: true,
        central: true,
        dp_mode: "active",
        producers: "developer-platform",
        calling: "developer-platform",
    })
    .await
    else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let rx = Receiver::start().await.unwrap();
    let a = provision::developer_sandbox(&stack, &email("hooks-a"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let b = provision::developer_sandbox(&stack, &email("hooks-b"), SANDBOX_SCOPES)
        .await
        .unwrap();
    // A second environment of tenant A (same project, same developer).
    let env2 = gw
        .post(
            &format!("/v1/projects/{}/environments", a.project_id),
            Auth::Session(&a.user.session),
            json!({ "kind": "SANDBOX" }),
        )
        .await;
    let env2_id = if env2.status == 201 {
        env2.uuid("/id")
    } else {
        // One SANDBOX per project: an operator-created TEST environment instead.
        let r = operator.environment(a.project_id, "TEST").await.unwrap();
        assert!(
            (200..300).contains(&r.status),
            "second environment: {}",
            r.text
        );
        r.uuid("/id")
    };
    let a2 = operator
        .credential_for(a.organization_id, a.project_id, env2_id, SANDBOX_SCOPES)
        .await
        .unwrap();

    // 1. Subscriptions (central webhooks: the gateway routes /v1/webhooks to the
    //    Developer Platform). The signing secret is returned ONCE.
    let (wa, secret_a) = subscribe(&gw, &a.customer.secret, &rx.url("/a"), JOURNEY_TYPES).await;
    let (wb, secret_b) = subscribe(&gw, &b.customer.secret, &rx.url("/b"), JOURNEY_TYPES).await;
    let (wa2, secret_a2) =
        subscribe(&gw, &a2.secret, &rx.url("/a2"), &["conversation.created"]).await;
    let one = gw
        .get(
            &format!("/v1/webhooks/{wa}"),
            Auth::Credential(&a.customer.secret),
        )
        .await;
    status(&one, 200, "GET /v1/webhooks/{id}");
    assert!(one.body.get("signingSecret").is_none() && !one.text.contains(&secret_a));
    let list = gw
        .get("/v1/webhooks", Auth::Credential(&a.customer.secret))
        .await;
    status(&list, 200, "GET /v1/webhooks");
    assert!(list.text.contains(&wa.to_string()) && !list.text.contains(&secret_a));
    assert!(
        !list.text.contains(&wb.to_string()) && !list.text.contains(&wa2.to_string()),
        "another tenant's / environment's subscription listed: {}",
        list.text
    );
    // The subscription lives at the Developer Platform (its console sees it, NATIVE).
    let console = gw
        .get(
            &format!("/v1/environments/{}/webhooks/{wa}", a.environment_id),
            Auth::Session(&a.user.session),
        )
        .await;
    status(&console, 200, "console GET webhook");
    assert_eq!(console.body["origin"], "NATIVE");
    assert!(!console.text.contains(&secret_a));
    // B's credential cannot see A's subscription.
    let r = gw
        .get(
            &format!("/v1/webhooks/{wa}"),
            Auth::Credential(&b.customer.secret),
        )
        .await;
    status(&r, 404, "B reads A's webhook");

    // 2. Real domain operations, tenants A, B and A's second environment in parallel.
    let alice = operator.principal(a.environment_id).await.unwrap();
    let bob = operator.principal(a.environment_id).await.unwrap();
    let mallory = operator.principal(b.environment_id).await.unwrap();
    let carol = operator.principal(env2_id).await.unwrap();
    let a_text = format!("A secret content {}", tag());
    let b_text = format!("B secret content {}", tag());
    let (a_conv, b_conv, a2_conv) = tokio::join!(
        async {
            allow_managed(&gw, &a.customer.secret).await;
            let cid = conversation(&gw, &a.customer.secret, alice).await;
            message(&gw, &a.customer.secret, alice, &cid, &a_text).await;
            cid
        },
        async {
            allow_managed(&gw, &b.customer.secret).await;
            let cid = conversation(&gw, &b.customer.secret, mallory).await;
            for i in 0..3 {
                message(
                    &gw,
                    &b.customer.secret,
                    mallory,
                    &cid,
                    &format!("{b_text} {i}"),
                )
                .await;
            }
            cid
        },
        async {
            allow_managed(&gw, &a2.secret).await;
            conversation(&gw, &a2.secret, carol).await
        },
    );
    // Numbering (synthetic inventory): a reservation, then its cancellation.
    let (number, e164, reservation) = reserve_and_cancel(&gw, &a.customer.secret).await;
    // Privacy Relay (Calling's public routes -> Privacy Relay): a context, then revoked.
    let (context, version) = relay_context(&gw, &a.customer.secret, &[alice, bob]).await;
    let r = gw
        .post(
            &format!("/v1/privacy-relay/contexts/{context}/revoke"),
            Auth::Credential(&a.customer.secret),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert!((200..300).contains(&r.status), "revoke context: {}", r.text);

    // 3. A's endpoint receives every family, signed with A's secret.
    let expected = [
        "conversation.created",
        "message.created",
        "number.reservation_changed",
        "relay.created",
        "relay.revoked",
    ];
    rx.wait_until("/a", WAIT, "every expected event family", |got| {
        let t = types_of(got);
        expected.iter().all(|e| t.contains(*e))
    })
    .await
    .unwrap();
    // Let stragglers (and any duplicate) arrive before counting.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let got = rx.received("/a");
    eprintln!(
        "tenant A received {} webhooks: {:?}",
        got.len(),
        got.iter().map(Received::event_type).collect::<Vec<_>>()
    );
    let forbidden = [
        a_text.as_str(),
        b_text.as_str(),
        e164.as_str(),
        a.customer.secret.as_str(),
        secret_a.as_str(),
        a.user.session.as_str(),
        "whsec_",
        "cfmk_",
        "e164",
        "provider",
    ];
    let mut event_ids = BTreeSet::new();
    for r in &got {
        r.verify(&secret_a)
            .unwrap_or_else(|e| panic!("{}: {e}", r.event_type()));
        assert!(r.verify(&secret_b).is_err(), "B's secret must not verify");
        assert!(r.event_id().starts_with("whe_"), "{}", r.event_id());
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_safe(r, &forbidden);
        assert!(
            event_ids.insert(r.event_id().to_string()),
            "event {} delivered twice",
            r.event_id()
        );
    }
    let ids = ids_in(&rx, "/a");
    for own in [&a_conv, &number, &reservation, &context] {
        assert!(ids.contains(own.as_str()), "{own} missing from A's events");
    }
    assert!(
        !ids.contains(&b_conv) && !ids.contains(&a2_conv),
        "another tenant's / environment's events reached A: {ids:?}"
    );
    // Event ids are stable: every received request is one recorded delivery of that event.
    let d = deliveries_until(&gw, &a.customer.secret, wa, "A's deliveries settled", |d| {
        d.len() >= got.len() && d.iter().all(|x| x["status"] == "SUCCEEDED")
    })
    .await;
    let by_delivery: BTreeMap<String, String> = d
        .iter()
        .map(|x| {
            (
                x["deliveryId"].as_str().unwrap().to_string(),
                x["eventId"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for r in &got {
        assert_eq!(
            by_delivery.get(r.delivery_id()).map(String::as_str),
            Some(r.event_id()),
            "CF-Webhook-Id / CF-Webhook-Event-Id match the delivery record"
        );
    }

    // 4. Tenant B and A's other environment received only their own events.
    let gb = rx
        .wait_until("/b", WAIT, "B's message events", |g| {
            g.iter()
                .filter(|r| r.event_type() == "message.created")
                .count()
                >= 3
        })
        .await
        .unwrap();
    for r in &gb {
        r.verify(&secret_b).unwrap();
        assert!(r.verify(&secret_a).is_err());
        assert_safe(r, &forbidden);
    }
    let ids_b = ids_in(&rx, "/b");
    assert!(ids_b.contains(&b_conv));
    for foreign in [&a_conv, &a2_conv, &number, &context] {
        assert!(!ids_b.contains(foreign.as_str()), "A's {foreign} reached B");
    }
    let ga2 = rx.wait_for("/a2", 1, WAIT).await.unwrap();
    for r in &ga2 {
        r.verify(&secret_a2).unwrap();
    }
    let ids_a2 = ids_in(&rx, "/a2");
    assert!(ids_a2.contains(&a2_conv));
    assert!(!ids_a2.contains(&a_conv) && !ids_a2.contains(&b_conv));

    // 5. Receiver-side proofs at the ingestion (Messaging's real key): another runtime
    //    environment's envelope and a spoofed producer are refused; a valid envelope is
    //    accepted once — re-publishing it returns the SAME event id and never fans out
    //    again.
    let conv = Uuid::new_v4();
    let other_env = envelope(&a, services::MESSAGING, "production", conv);
    let r = ingest(&stack, services::MESSAGING, &other_env).await;
    status(&r, 400, "an envelope of another runtime environment");
    let spoofed = envelope(&a, services::CALLING, stack.runtime.as_str(), conv);
    let r = ingest(&stack, services::MESSAGING, &spoofed).await;
    status(&r, 400, "an envelope claiming another producer");
    let valid = envelope(&a, services::MESSAGING, stack.runtime.as_str(), conv);
    let first = ingest(&stack, services::MESSAGING, &valid).await;
    status(&first, 200, "a valid Messaging envelope");
    let again = ingest(&stack, services::MESSAGING, &valid).await;
    status(&again, 200, "the same envelope re-published");
    assert_eq!(
        first.body["eventId"], again.body["eventId"],
        "stable event id"
    );
    assert_eq!(again.body["duplicate"], true);
    let event_id = first.str("/eventId");
    rx.wait_until("/a", WAIT, "the ingested envelope", |g| {
        g.iter().any(|r| r.event_id() == event_id)
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let n = rx
        .received("/a")
        .iter()
        .filter(|r| r.event_id() == event_id)
        .count();
    assert_eq!(n, 1, "a re-published event is delivered once");
    assert!(
        !ids_in(&rx, "/b").contains(&conv.to_string()),
        "A's event reached B"
    );
    stack.teardown().await;
}

// =====================================================================================
// 2. Failure, retry, dead letter, replay, endpoint health
// =====================================================================================

#[tokio::test]
async fn webhook_failure_and_replay_journey() {
    let Some((_g, stack)) = start(Hooks {
        local: true,
        central: true,
        dp_mode: "active",
        producers: "developer-platform",
        calling: "developer-platform",
    })
    .await
    else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let rx = Receiver::start().await.unwrap();
    let a = provision::developer_sandbox(&stack, &email("hooks-fail"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let cred = a.customer.secret.clone();
    let session = a.user.session.clone();
    let env = a.environment_id;
    rx.script("/flaky", vec![Reply::status(500)]);
    rx.script(
        "/limited",
        vec![Reply::status(429).with_header("retry-after", "1")],
    );
    rx.script("/gone", vec![Reply::status(410)]);
    rx.set_default("/bad", Reply::status(400));
    rx.set_default("/dead", Reply::status(410));
    let conv = ["conversation.created"];
    let (w_flaky, s_flaky) = subscribe(&gw, &cred, &rx.url("/flaky"), &conv).await;
    let (w_limited, s_limited) = subscribe(&gw, &cred, &rx.url("/limited"), &conv).await;
    let (w_gone, s_gone) = subscribe(&gw, &cred, &rx.url("/gone"), &conv).await;
    let (w_bad, _) = subscribe(&gw, &cred, &rx.url("/bad"), &conv).await;
    let (w_dead, _) = subscribe(&gw, &cred, &rx.url("/dead"), &["message.created"]).await;

    let alice = operator.principal(env).await.unwrap();
    allow_managed(&gw, &cred).await;
    let cid = conversation(&gw, &cred, alice).await;

    // Permanent 4xx: FAILED (dead letter, retained) after ONE attempt.
    let gone = rx.wait_for("/gone", 1, WAIT).await.unwrap();
    gone[0].verify(&s_gone).unwrap();
    let original_event = gone[0].event_id().to_string();
    let original_delivery = gone[0].delivery_id().to_string();
    let d = deliveries_until(&gw, &cred, w_gone, "410 dead-lettered", |d| {
        d.first().is_some_and(|x| x["status"] == "FAILED")
    })
    .await;
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["attemptCount"], 1, "{:?}", d[0]);
    assert_eq!(d[0]["lastHttpStatus"], 410);
    assert_eq!(d[0]["failureCategory"], "HTTP_4XX");
    assert_eq!(
        d[0]["deliveryId"].as_str(),
        Some(original_delivery.as_str())
    );
    let failed = gw
        .get(
            &format!("/v1/webhooks/{w_gone}/deliveries?status=FAILED"),
            Auth::Credential(&cred),
        )
        .await;
    status(&failed, 200, "dead letters listed");
    assert_eq!(failed.body["data"].as_array().map(Vec::len), Some(1));
    let d = deliveries_until(&gw, &cred, w_bad, "400 dead-lettered", |d| {
        d.first().is_some_and(|x| x["status"] == "FAILED")
    })
    .await;
    assert_eq!(
        (d[0]["attemptCount"].clone(), d[0]["lastHttpStatus"].clone()),
        (json!(1), json!(400))
    );
    assert_eq!(
        rx.received("/bad").len(),
        1,
        "a permanent refusal is not retried"
    );

    // Manual replay of the dead letter: a NEW delivery id, the SAME event id, delivered
    // (the endpoint answers 200 now) and verifiable.
    let replay = gw
        .post(
            &format!("/v1/webhook-deliveries/{original_delivery}/replay"),
            Auth::Credential(&cred),
            json!({}),
        )
        .await;
    status(&replay, 201, "POST /v1/webhook-deliveries/{id}/replay");
    let replay_id = replay.str("/deliveryId");
    assert_ne!(replay_id, original_delivery);
    assert_eq!(replay.str("/eventId"), original_event);
    assert_eq!(replay.str("/replayOf"), original_delivery);
    let gone = rx.wait_for("/gone", 2, WAIT).await.unwrap();
    assert_eq!(gone[1].delivery_id(), replay_id);
    assert_eq!(
        gone[1].event_id(),
        original_event,
        "same event id on replay"
    );
    gone[1].verify(&s_gone).unwrap();
    assert_eq!(gone[1].json()["id"], gone[0].json()["id"]);
    let d = deliveries_until(&gw, &cred, w_gone, "replay succeeded", |d| {
        d.iter()
            .any(|x| x["deliveryId"] == json!(replay_id) && x["status"] == "SUCCEEDED")
    })
    .await;
    assert_eq!(d.len(), 2, "the dead letter is retained beside its replay");
    // Another tenant cannot replay A's delivery.
    let other = operator
        .customer("EXTERNAL", "SANDBOX", &["webhooks.manage", "webhooks.read"])
        .await
        .unwrap();
    let r = gw
        .post(
            &format!("/v1/webhook-deliveries/{original_delivery}/replay"),
            Auth::Credential(&other.secret),
            json!({}),
        )
        .await;
    status(&r, 404, "replay of another tenant's delivery");

    // 5xx then 200: retried (jittered backoff) and SUCCEEDED, same delivery and event id,
    // attempt history readable.
    let flaky = rx.wait_for("/flaky", 2, WAIT).await.unwrap();
    assert_eq!((flaky[0].answered, flaky[1].answered), (500, 200));
    assert_eq!(flaky[0].delivery_id(), flaky[1].delivery_id());
    assert_eq!(flaky[0].event_id(), flaky[1].event_id());
    for r in &flaky {
        r.verify(&s_flaky).unwrap();
    }
    let d = deliveries_until(&gw, &cred, w_flaky, "500 then 200 succeeded", |d| {
        d.first().is_some_and(|x| x["status"] == "SUCCEEDED")
    })
    .await;
    assert_eq!(d[0]["attemptCount"], 2);
    let h = attempts(&gw, &session, env, flaky[0].delivery_id()).await;
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!(
        (h[0]["outcome"].clone(), h[0]["httpStatus"].clone()),
        (json!("RETRY"), json!(500))
    );
    assert_eq!(h[0]["errorClass"], "HTTP_5XX");
    assert_eq!(
        (h[1]["outcome"].clone(), h[1]["httpStatus"].clone()),
        (json!("SUCCEEDED"), json!(200))
    );

    // 429 + Retry-After: retried no earlier than Retry-After, then SUCCEEDED.
    let limited = rx.wait_for("/limited", 2, WAIT).await.unwrap();
    assert_eq!((limited[0].answered, limited[1].answered), (429, 200));
    assert!(
        limited[1].at.duration_since(limited[0].at) >= Duration::from_secs(1),
        "retried before Retry-After"
    );
    limited[1].verify(&s_limited).unwrap();
    let d = deliveries_until(&gw, &cred, w_limited, "429 then 200 succeeded", |d| {
        d.first().is_some_and(|x| x["status"] == "SUCCEEDED")
    })
    .await;
    let h = attempts(&gw, &session, env, d[0]["deliveryId"].as_str().unwrap()).await;
    assert_eq!(h[0]["errorClass"], "HTTP_RATE_LIMITED", "{h:?}");

    // Endpoint health: AUTO_DISABLE_AFTER consecutive failures disable the endpoint.
    for i in 0..AUTO_DISABLE_AFTER {
        message(&gw, &cred, alice, &cid, &format!("health {i}")).await;
    }
    rx.wait_for("/dead", AUTO_DISABLE_AFTER, WAIT)
        .await
        .unwrap();
    let mut view = Value::Null;
    for _ in 0..60 {
        let r = polled(
            &gw,
            &format!("/v1/webhooks/{w_dead}"),
            Auth::Credential(&cred),
        )
        .await;
        status(&r, 200, "GET dead webhook");
        view = r.body;
        if view["status"] == "DISABLED" {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(view["status"], "DISABLED", "auto-disabled: {view}");
    let console = gw
        .get(
            &format!("/v1/environments/{env}/webhooks/{w_dead}"),
            Auth::Session(&session),
        )
        .await;
    status(&console, 200, "console webhook health");
    assert_eq!(
        console.body["health"]["disabledReason"],
        "CONSECUTIVE_FAILURES"
    );
    assert_eq!(
        console.body["health"]["consecutiveFailures"],
        AUTO_DISABLE_AFTER as i64
    );
    // A disabled endpoint receives nothing more, and its dead letters cannot be replayed.
    message(&gw, &cred, alice, &cid, "after disable").await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(rx.received("/dead").len(), AUTO_DISABLE_AFTER);
    let d = deliveries(&gw, &cred, w_dead).await;
    assert_eq!(
        d.len(),
        AUTO_DISABLE_AFTER,
        "no delivery to a disabled endpoint"
    );
    assert!(d.iter().all(|x| x["status"] == "FAILED"));
    let r = gw
        .post(
            &format!(
                "/v1/webhook-deliveries/{}/replay",
                d[0]["deliveryId"].as_str().unwrap()
            ),
            Auth::Credential(&cred),
            json!({}),
        )
        .await;
    status(&r, 404, "replay to a disabled endpoint");
    stack.teardown().await;
}

/// A `Retry-After` longer than the default backoff (first retry 15–30 s) must postpone
/// the retry accordingly.
#[tokio::test]
async fn webhook_retry_after_is_honoured() {
    let Some((_g, stack)) = start(Hooks {
        local: true,
        central: true,
        dp_mode: "active",
        producers: "developer-platform",
        calling: "developer-platform",
    })
    .await
    else {
        return;
    };
    let gw = stack.gateway();
    let rx = Receiver::start().await.unwrap();
    let a = provision::developer_sandbox(&stack, &email("hooks-ra"), SANDBOX_SCOPES)
        .await
        .unwrap();
    const RETRY_AFTER: i64 = 120;
    rx.script(
        "/slow",
        vec![Reply::status(429).with_header("retry-after", &RETRY_AFTER.to_string())],
    );
    let (w, _) = subscribe(
        &gw,
        &a.customer.secret,
        &rx.url("/slow"),
        &["conversation.created"],
    )
    .await;
    let alice = stack.operator().principal(a.environment_id).await.unwrap();
    allow_managed(&gw, &a.customer.secret).await;
    conversation(&gw, &a.customer.secret, alice).await;
    let first = rx.wait_for("/slow", 1, WAIT).await.unwrap();
    let d = deliveries_until(&gw, &a.customer.secret, w, "429 scheduled for retry", |d| {
        d.first().is_some_and(|x| x["status"] == "RETRY_SCHEDULED")
    })
    .await;
    let next = chrono::DateTime::parse_from_rfc3339(d[0]["nextAttemptAt"].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    let earliest = first[0].at_utc + chrono::Duration::seconds(RETRY_AFTER - 2);
    stack.teardown().await;
    assert!(
        next >= earliest,
        "Retry-After: {RETRY_AFTER} answered at {}, but the retry is scheduled at {next} \
         (the delivery worker ignores Retry-After)",
        first[0].at_utc
    );
}

// =====================================================================================
// 3. Shadow mode — Calling is the legacy owner, no double delivery
// =====================================================================================

/// Producers publish to BOTH receivers; Calling (the legacy owner) delivers; the
/// Developer Platform ingests in SHADOW mode.
const SHADOW: Hooks = Hooks {
    local: true,
    central: false,
    dp_mode: "shadow",
    producers: "both",
    calling: "both",
};

/// A Developer Console subscription (member session; Developer Platform-owned).
async fn console_subscribe(
    gw: &Gateway,
    sb: &Sandbox,
    url: &str,
    types: &[&str],
) -> (Uuid, String) {
    let r = gw
        .post(
            &format!("/v1/environments/{}/webhooks", sb.environment_id),
            Auth::Session(&sb.user.session),
            json!({ "url": url, "eventTypes": types }),
        )
        .await;
    status(&r, 201, "console POST /v1/environments/{id}/webhooks");
    (r.uuid("/webhook/webhookId"), r.str("/signingSecret"))
}

async fn console_deliveries(gw: &Gateway, sb: &Sandbox, webhook: Uuid) -> Vec<Value> {
    let r = polled(
        gw,
        &format!(
            "/v1/environments/{}/webhooks/{webhook}/deliveries?limit=100",
            sb.environment_id
        ),
        Auth::Session(&sb.user.session),
    )
    .await;
    status(&r, 200, "console deliveries");
    r.body["items"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn shadow_mode_no_double_delivery_journey() {
    let Some((_g, stack)) = start(SHADOW).await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let rx = Receiver::start().await.unwrap();
    let a = provision::developer_sandbox(&stack, &email("hooks-shadow"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let cred = a.customer.secret.clone();
    let types = ["conversation.created", "message.created", "relay.created"];
    // The customer's subscription, created while central webhooks are OFF: Calling.
    let (wc, secret_calling) = subscribe(&gw, &cred, &rx.url("/hook"), &types).await;
    // Its Developer Platform counterpart (the cut-over copy) for the same endpoint.
    let (wd, secret_dp) = console_subscribe(&gw, &a, &rx.url("/hook"), &types).await;
    assert_ne!(wc, wd);

    let alice = operator.principal(a.environment_id).await.unwrap();
    let bob = operator.principal(a.environment_id).await.unwrap();
    allow_managed(&gw, &cred).await;
    let cid = conversation(&gw, &cred, alice).await;
    message(&gw, &cred, alice, &cid, "shadow one").await;
    message(&gw, &cred, alice, &cid, "shadow two").await;
    let (context, _) = relay_context(&gw, &cred, &[alice, bob]).await;
    let expected: BTreeMap<&str, usize> = [
        ("conversation.created", 1),
        ("message.created", 2),
        ("relay.created", 1),
    ]
    .into_iter()
    .collect();
    let count = |got: &[Received], t: &str| got.iter().filter(|r| r.event_type() == t).count();
    rx.wait_until("/hook", WAIT, "Calling's deliveries", |got| {
        expected.iter().all(|(t, n)| count(got, t) >= *n)
    })
    .await
    .unwrap();
    // The Developer Platform recorded each event as SHADOW (nothing sent).
    let shadow = {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let d = console_deliveries(&gw, &a, wd).await;
            let ok = expected.iter().all(|(t, n)| {
                d.iter()
                    .filter(|x| x["eventType"] == json!(t) && x["status"] == "SHADOW")
                    .count()
                    >= *n
            });
            if ok {
                break d;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "SHADOW deliveries not recorded: {d:#?}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    // Give the Developer Platform worker several passes to (wrongly) send anything.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let got = rx.received("/hook");
    eprintln!(
        "endpoint received {} webhooks: {:?}; DP shadow deliveries {}",
        got.len(),
        got.iter().map(Received::event_type).collect::<Vec<_>>(),
        shadow.len()
    );
    // Exactly once per event, all from Calling (verifiable with Calling's secret only).
    for (t, n) in &expected {
        assert_eq!(count(&got, t), *n, "{t} delivered exactly once per event");
    }
    assert_eq!(
        got.len(),
        expected.values().sum::<usize>(),
        "no extra delivery"
    );
    let mut ids = BTreeSet::new();
    for r in &got {
        r.verify(&secret_calling)
            .unwrap_or_else(|e| panic!("Calling's signature: {e}"));
        assert!(
            r.verify(&secret_dp).is_err(),
            "a Developer Platform delivery was sent"
        );
        assert!(
            ids.insert(r.event_id().to_string()),
            "duplicate {}",
            r.event_id()
        );
        assert_safe(r, &["shadow one", "shadow two"]);
    }
    assert!(ids_in(&rx, "/hook").contains(&context));
    let d = console_deliveries(&gw, &a, wd).await;
    assert!(
        d.iter()
            .all(|x| x["status"] == "SHADOW" && x["attemptCount"] == 0),
        "shadow deliveries are never attempted: {d:#?}"
    );
    // Calling's own records agree (one SUCCEEDED delivery per event).
    let dc = deliveries(&gw, &cred, wc).await;
    assert_eq!(dc.len(), got.len(), "{dc:#?}");
    assert!(dc.iter().all(|x| x["status"] == "SUCCEEDED"));
    stack.teardown().await;
}

// =====================================================================================
// 4. Gateway switch — the same credential, Calling vs. the Developer Platform
// =====================================================================================

async fn gateway_central(stack: &Stack) -> bool {
    let r = Resp::from(
        stack
            .http
            .get(format!("{}/ready", stack.gateway_url()))
            .send()
            .await
            .unwrap(),
    )
    .await;
    status(&r, 200, "gateway /ready");
    r.body["centralWebhooks"].as_bool().unwrap()
}

async fn listed(gw: &Gateway, cred: &str) -> String {
    let r = gw.get("/v1/webhooks", Auth::Credential(cred)).await;
    status(&r, 200, "GET /v1/webhooks");
    r.text
}

#[tokio::test]
async fn gateway_webhook_switch_journey() {
    let Some((_g, mut stack)) = start(SHADOW).await else {
        return;
    };
    let rx = Receiver::start().await.unwrap();
    let a = provision::developer_sandbox(&stack, &email("hooks-switch"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let cred = a.customer.secret.clone();
    let types = ["conversation.created"];
    let gw = stack.gateway();
    assert!(!gateway_central(&stack).await);
    // Central OFF: /v1/webhooks is Calling's.
    let (wc, _) = subscribe(&gw, &cred, &rx.url("/calling"), &types).await;
    let (wd, _) = console_subscribe(&gw, &a, &rx.url("/dp"), &types).await;
    let text = listed(&gw, &cred).await;
    assert!(
        text.contains(&wc.to_string()) && !text.contains(&wd.to_string()),
        "{text}"
    );
    status(
        &gw.get(&format!("/v1/webhooks/{wd}"), Auth::Credential(&cred))
            .await,
        404,
        "the Developer Platform subscription is unknown to Calling",
    );
    // The Developer Platform does not know Calling's subscription either.
    let console = gw
        .get(
            &format!("/v1/environments/{}/webhooks", a.environment_id),
            Auth::Session(&a.user.session),
        )
        .await;
    status(&console, 200, "console list");
    assert!(!console.text.contains(&wc.to_string()));

    // Central ON (gateway restarted with the flag): the same credential reaches the
    // Developer Platform.
    stack
        .restart(
            stack::API_GATEWAY,
            &[("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED", "true")],
        )
        .await
        .unwrap();
    let gw = stack.gateway();
    assert!(gateway_central(&stack).await);
    let text = listed(&gw, &cred).await;
    assert!(
        text.contains(&wd.to_string()) && !text.contains(&wc.to_string()),
        "{text}"
    );
    status(
        &gw.get(&format!("/v1/webhooks/{wd}"), Auth::Credential(&cred))
            .await,
        200,
        "the Developer Platform subscription through the gateway",
    );
    status(
        &gw.get(&format!("/v1/webhooks/{wc}"), Auth::Credential(&cred))
            .await,
        404,
        "Calling's subscription is unknown to the Developer Platform",
    );
    let (wn, _) = subscribe(&gw, &cred, &rx.url("/new"), &types).await;
    let console = gw
        .get(
            &format!("/v1/environments/{}/webhooks/{wn}", a.environment_id),
            Auth::Session(&a.user.session),
        )
        .await;
    status(
        &console,
        200,
        "created through the gateway, owned by the DP",
    );
    assert_eq!(console.body["origin"], "NATIVE");

    // Central OFF again: back to Calling.
    stack
        .restart(
            stack::API_GATEWAY,
            &[("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED", "false")],
        )
        .await
        .unwrap();
    let gw = stack.gateway();
    assert!(!gateway_central(&stack).await);
    let text = listed(&gw, &cred).await;
    assert!(
        text.contains(&wc.to_string())
            && !text.contains(&wd.to_string())
            && !text.contains(&wn.to_string()),
        "{text}"
    );
    stack.teardown().await;
}

// =====================================================================================
// 5. SSRF policy on the real processes (`test` runtime)
// =====================================================================================

/// The machine's host name when it resolves only to prohibited (loopback / private)
/// addresses — a name the registration check cannot know is private.
async fn private_hostname() -> Option<String> {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .ok()?
        .trim()
        .to_string();
    if name.is_empty() || name == "localhost" || name.ends_with(".local") {
        return None;
    }
    let addrs: Vec<_> = tokio::net::lookup_host((name.as_str(), 443))
        .await
        .ok()?
        .collect();
    (!addrs.is_empty()
        && addrs
            .iter()
            .all(|a| cf_webhooks::endpoint::is_prohibited(a.ip())))
    .then_some(name)
}

#[tokio::test]
async fn webhook_ssrf_policy_journey() {
    let Some((_g, mut stack)) = start(Hooks {
        local: false,
        central: true,
        dp_mode: "active",
        producers: "developer-platform",
        calling: "developer-platform",
    })
    .await
    else {
        return;
    };
    let gw = stack.gateway();
    let a = provision::developer_sandbox(&stack, &email("hooks-ssrf"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let cred = a.customer.secret.clone();
    // A listener nothing may ever reach.
    let trap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = trap.local_addr().unwrap().port();
    let refused = [
        format!("http://127.0.0.1:{port}/hook"),
        format!("https://127.0.0.1:{port}/hook"),
        "https://169.254.169.254/latest/meta-data/".to_string(),
        "http://169.254.169.254/latest/meta-data/".to_string(),
        format!("https://localhost:{port}/hook"),
        "https://metadata.google.internal/computeMetadata/v1/".to_string(),
        format!("https://[::1]:{port}/hook"),
        "https://10.0.0.1/hook".to_string(),
        "https://192.168.1.10/hook".to_string(),
        format!("https://[::ffff:127.0.0.1]:{port}/hook"),
    ];
    let try_subscribe = |gw: Gateway, url: String| {
        let cred = cred.clone();
        async move {
            gw.post(
                "/v1/webhooks",
                Auth::Credential(&cred),
                json!({ "url": url, "eventTypes": ["conversation.created"] }),
            )
            .await
        }
    };
    // Developer Platform (central webhooks ON).
    for url in &refused {
        let r = try_subscribe(gw.clone(), url.clone()).await;
        status(&r, 400, &format!("DP subscription to {url}"));
        assert_eq!(r.code(), Some("VALIDATION_ERROR"), "{}", r.text);
    }
    // A host name that resolves to a private address passes registration (the name is
    // not on any deny list) but is never connected to: the delivery client resolves
    // through the guarded resolver, which drops prohibited addresses.
    if let Some(host) = private_hostname().await {
        let (w, _) = subscribe(
            &gw,
            &cred,
            &format!("https://{host}:{port}/hook"),
            &["conversation.created"],
        )
        .await;
        let alice = stack.operator().principal(a.environment_id).await.unwrap();
        allow_managed(&gw, &cred).await;
        conversation(&gw, &cred, alice).await;
        let d = deliveries_until(&gw, &cred, w, "blocked delivery dead-lettered", |d| {
            d.first().is_some_and(|x| x["status"] == "FAILED")
        })
        .await;
        assert_eq!(d[0]["failureCategory"], "ENDPOINT_BLOCKED", "{:?}", d[0]);
        assert_eq!(d[0]["lastHttpStatus"], Value::Null);
        let accepted = tokio::time::timeout(Duration::from_millis(500), trap.accept()).await;
        assert!(accepted.is_err(), "the private address was connected to");
    } else {
        eprintln!(
            "NOTE: the host name does not resolve to a private address only; the \
             resolver-time SSRF check was not exercised"
        );
    }
    // Legacy Calling (central webhooks OFF) refuses the same endpoints.
    stack
        .restart(
            stack::API_GATEWAY,
            &[("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED", "false")],
        )
        .await
        .unwrap();
    let gw = stack.gateway();
    for url in &refused {
        let r = try_subscribe(gw.clone(), url.clone()).await;
        assert!(
            (400..500).contains(&r.status) && ![401, 403, 404, 429].contains(&r.status),
            "Calling subscription to {url}: {} {}",
            r.status,
            r.text
        );
    }
    // Neither process starts with private endpoints allowed outside `local`.
    for (process, var) in [
        (
            stack::DEVELOPER_PLATFORM,
            "DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
        ),
        (stack::CALLING, "CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS"),
    ] {
        let err = stack
            .restart(process, &[(var, "true")])
            .await
            .expect_err("private endpoints are refused in the test runtime");
        let text = format!("{err:#}");
        assert!(
            text.contains(var),
            "{process} refuses {var} by name: {text}"
        );
    }
    stack.teardown().await;
}

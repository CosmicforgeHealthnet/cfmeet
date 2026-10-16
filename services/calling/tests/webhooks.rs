//! Part G §67 / §68: durable event publication and customer webhooks against a real
//! PostgreSQL and a real local HTTP receiver: application-scoped subscriptions (IDOR,
//! scopes, secrets never returned twice), SSRF / DNS-rebinding refusal, signed deliveries
//! (signature, timestamp window, unique delivery ids, stable event ids), retries,
//! dead letter, manual replay, worker crash recovery, concurrent claims, transport outage,
//! and Privacy Relay events — never a provider payload verbatim.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    routing::post,
    Router,
};
use calling::{
    platform::scopes::names,
    webhooks::{
        worker::{EventTransport, LocalWebhookTransport},
        WebhookSettings,
    },
};
use cf_service_auth::{OutboundServiceRequest, ServiceId};
use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox, signing};
use common::{
    enterprise::{credential, strings, EnterpriseWorld},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

/// What the receiver saw.
#[derive(Debug, Clone)]
struct Received {
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Default)]
struct ReceiverState {
    seen: Vec<Received>,
    /// Statuses to answer, in order (then 200).
    script: Vec<u16>,
    delay_ms: u64,
}

#[derive(Clone)]
struct Receiver {
    state: Arc<Mutex<ReceiverState>>,
    url: String,
}

async fn hook(
    State(state): State<Arc<Mutex<ReceiverState>>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let (status, delay) = {
        let mut s = state.lock().unwrap();
        s.seen.push(Received {
            headers,
            body: body.to_vec(),
        });
        let status = if s.script.is_empty() {
            200
        } else {
            s.script.remove(0)
        };
        (status, s.delay_ms)
    };
    if delay > 0 {
        tokio::time::sleep(StdDuration::from_millis(delay)).await;
    }
    StatusCode::from_u16(status).unwrap()
}

impl Receiver {
    async fn start() -> Self {
        let state = Arc::new(Mutex::new(ReceiverState::default()));
        let app = Router::new()
            .route("/hook", post(hook))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            state,
            url: format!("http://127.0.0.1:{port}/hook"),
        }
    }

    fn seen(&self) -> Vec<Received> {
        self.state.lock().unwrap().seen.clone()
    }

    fn script(&self, statuses: &[u16]) {
        self.state.lock().unwrap().script = statuses.to_vec();
    }

    fn delay(&self, ms: u64) {
        self.state.lock().unwrap().delay_ms = ms;
    }
}

/// A deterministic, non-weak 32-byte test key (Part H §74.12 refuses all-equal or
/// low-entropy keys, so the former `[42u8; 32]` is no longer accepted).
fn key_bytes(seed: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(7));
    }
    k
}

fn dev_settings() -> WebhookSettings {
    WebhookSettings {
        secret_box: Some(Arc::new(SecretBox::from_bytes(&key_bytes(42)).unwrap())),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        request_timeout: StdDuration::from_secs(1),
        max_attempts: 3,
    }
}

struct World {
    e: EnterpriseWorld,
    manager: calling::platform::credentials::IssuedCredential,
}

impl World {
    async fn new(settings: WebhookSettings) -> Option<Self> {
        let e = EnterpriseWorld::configured(move |rt| rt.webhooks = settings).await?;
        for scope in [
            names::WEBHOOKS_READ,
            names::WEBHOOKS_MANAGE,
            names::USAGE_READ,
        ] {
            e.h()
                .platform
                .grant_application_scope(e.a.w.consumer.application.id, scope)
                .await
                .unwrap();
        }
        let manager = credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[
                names::WEBHOOKS_READ,
                names::WEBHOOKS_MANAGE,
                names::CALLS_READ,
                names::PRIVACY_SESSIONS_READ,
            ],
        )
        .await;
        Some(Self { e, manager })
    }

    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.e
            .send(self.manager.secret.expose_once(), method, path, body)
            .await
    }

    async fn subscribe(&self, url: &str, types: &[&str]) -> (Uuid, String) {
        let r = self
            .send(
                Method::POST,
                "/v1/webhooks",
                Some(json!({ "url": url, "eventTypes": types, "description": "test" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        common::contracts::check(
            "platform/webhooks/v1.schema.json#/$defs/WebhookWithSecret",
            &r.json,
        );
        (
            r.json["webhook"]["webhookId"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
            r.json["signingSecret"].as_str().unwrap().to_string(),
        )
    }

    async fn publish(&self) -> calling::webhooks::worker::PublishReport {
        let w = &self.e.h().state.webhooks;
        w.publish_outbox_once(&LocalWebhookTransport::new((**w).clone()), 1000)
            .await
            .unwrap()
    }

    async fn deliver(&self) -> calling::webhooks::worker::DeliveryReport {
        self.e
            .h()
            .state
            .webhooks
            .deliver_due_once(100)
            .await
            .unwrap()
    }

    async fn one(&self, sql: &str) -> String {
        strings(self.e.h().conn(), sql)
            .await
            .pop()
            .unwrap_or_default()
    }

    /// A ringing application Call (events: requested, state changes, invitations).
    async fn call(&self) -> Uuid {
        let a = &self.e.a;
        let caller = a
            .device(&a.w.initiator, &format!("c-{}", Uuid::new_v4().simple()))
            .await;
        a.device(&a.w.recipient, &format!("r-{}", Uuid::new_v4().simple()))
            .await;
        let r = a
            .create_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&a.w.recipient],
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
    }
}

fn header(r: &Received, name: &str) -> String {
    r.headers.get(name).unwrap().to_str().unwrap().to_string()
}

/// §64 / §68: subscriptions are application-scoped (another tenant gets 404 for every
/// operation), scopes gate management and each event type, the secret is returned only
/// on create / rotation, mutations are versioned, and client-supplied authority is refused.
#[tokio::test]
async fn subscriptions_are_scoped_versioned_and_never_return_the_secret() {
    let Some(w) = World::new(dev_settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (id, secret) = w.subscribe(&rx.url, &["call.ended", "call.ringing"]).await;
    assert!(secret.starts_with("whsec_"));
    let path = format!("/v1/webhooks/{id}");
    for (m, p) in [
        (Method::GET, path.clone()),
        (Method::GET, "/v1/webhooks".to_string()),
    ] {
        let r = w.send(m, &p, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert!(
            !r.text.contains(&secret) && !r.text.contains("whsec_"),
            "{}",
            r.text
        );
    }
    let r = w.send(Method::GET, &path, None).await;
    common::contracts::check("platform/webhooks/v1.schema.json#/$defs/Webhook", &r.json);
    // Stored sealed: the plaintext secret is nowhere in the database row.
    assert!(!w
        .one(&format!(
            "SELECT row_to_json(s)::text FROM webhook_subscriptions s WHERE id = '{id}'"
        ))
        .await
        .contains(&secret));
    // Versioned updates.
    let r = w
        .send(
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 1, "status": "DISABLED" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
    let r = w
        .send(
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 1, "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = w
        .send(
            Method::POST,
            &format!("{path}/rotate-secret"),
            Some(json!({ "expectedVersion": 2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let rotated = r.json["signingSecret"].as_str().unwrap();
    assert_ne!(rotated, secret);
    assert_eq!(r.json["webhook"]["secretVersion"], 2);
    // Client-supplied authority, unknown / wildcard / internal event types: refused.
    for body in [
        json!({ "url": rx.url, "eventTypes": ["call.ended"], "tenantId": Uuid::new_v4() }),
        json!({ "url": rx.url, "eventTypes": ["*"] }),
        json!({ "url": rx.url, "eventTypes": ["calling.call-state-changed"] }),
        json!({ "url": rx.url, "eventTypes": ["telephony.leg-answered"] }),
        json!({ "url": rx.url, "eventTypes": [] }),
    ] {
        let r = w.send(Method::POST, "/v1/webhooks", Some(body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    }
    // An event type whose read scope the credential lacks.
    let r = w
        .send(
            Method::POST,
            "/v1/webhooks",
            Some(json!({ "url": rx.url, "eventTypes": ["voicemail.ready"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Without webhooks.manage / webhooks.read.
    let reader = credential(
        w.e.h(),
        &w.e.a.w.consumer,
        w.e.a.w.kind,
        &[names::CALLS_READ],
    )
    .await;
    for (m, p, b) in [
        (
            Method::POST,
            "/v1/webhooks".to_string(),
            Some(json!({ "url": rx.url, "eventTypes": ["call.ended"] })),
        ),
        (Method::GET, path.clone(), None),
    ] {
        let r = w.e.send(reader.secret.expose_once(), m, &p, b).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    }
    // Another tenant with every webhook scope: 404 for every operation (IDOR).
    let (other, _) = w.e.other_tenant("initech").await;
    for scope in [names::WEBHOOKS_READ, names::WEBHOOKS_MANAGE] {
        w.e.h()
            .platform
            .grant_application_scope(other.application.id, scope)
            .await
            .unwrap();
    }
    let intruder = credential(
        w.e.h(),
        &other,
        w.e.a.w.kind,
        &[
            names::WEBHOOKS_READ,
            names::WEBHOOKS_MANAGE,
            names::CALLS_READ,
        ],
    )
    .await;
    let delivery = Uuid::new_v4();
    for (m, p, b) in [
        (Method::GET, path.clone(), None),
        (
            Method::PATCH,
            path.clone(),
            Some(json!({ "expectedVersion": 3, "status": "DISABLED" })),
        ),
        (Method::DELETE, format!("{path}?expectedVersion=3"), None),
        (
            Method::POST,
            format!("{path}/rotate-secret"),
            Some(json!({ "expectedVersion": 3 })),
        ),
        (Method::GET, format!("{path}/deliveries"), None),
        (
            Method::POST,
            format!("/v1/webhook-deliveries/{delivery}/replay"),
            None,
        ),
    ] {
        let r = w.e.send(intruder.secret.expose_once(), m, &p, b).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}: {}", r.text);
    }
    let r =
        w.e.send(
            intruder.secret.expose_once(),
            Method::GET,
            "/v1/webhooks",
            None,
        )
        .await;
    assert_eq!(r.json["data"].as_array().unwrap().len(), 0);
    // Delete.
    let r = w
        .send(Method::DELETE, &format!("{path}?expectedVersion=3"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DELETED");
    assert_eq!(
        w.send(Method::GET, &path, None).await.status,
        StatusCode::NOT_FOUND
    );
    // §64: every change is durably audited with its actor, request and correlation ids.
    assert_eq!(
        w.one(&format!(
            "SELECT string_agg(operation, ',' ORDER BY seq) FROM enterprise_admin_audit \
              WHERE resource_type = 'WEBHOOK_SUBSCRIPTION' AND resource_id = '{id}' \
                AND actor_kind = 'CUSTOMER_APPLICATION' AND request_id <> '' \
                AND correlation_id <> ''"
        ))
        .await,
        "CREATE,UPDATE,ROTATE_SECRET,DELETE"
    );
}

/// §68 SSRF: the production policy refuses local, private, link-local, metadata and
/// non-https endpoints and embedded credentials at registration.
#[tokio::test]
async fn ssrf_endpoints_are_refused() {
    let mut strict = dev_settings();
    strict.policy = EndpointPolicy::STRICT;
    let Some(w) = World::new(strict).await else {
        return;
    };
    for url in [
        "https://localhost/x",
        "https://127.0.0.1/x",
        "https://10.1.2.3/x",
        "https://172.16.0.9/x",
        "https://192.168.0.10/x",
        "https://169.254.169.254/latest/meta-data",
        "https://metadata.google.internal/x",
        "https://[::1]/x",
        "https://[::ffff:10.0.0.1]/x",
        "http://hooks.example.com/x",
        "https://user:pw@hooks.example.com/x",
        "https://hooks.example.com:22/x",
        "file:///etc/passwd",
    ] {
        let r = w
            .send(
                Method::POST,
                "/v1/webhooks",
                Some(json!({ "url": url, "eventTypes": ["call.ended"] })),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{url}: {}", r.text);
    }
    let r = w
        .send(
            Method::POST,
            "/v1/webhooks",
            Some(
                json!({ "url": "https://hooks.example.com/cfmeet", "eventTypes": ["call.ended"] }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        w.one("SELECT count(*)::text FROM webhook_subscriptions")
            .await,
        "1"
    );
}

/// §67 / §68: committed Call events are published from the outbox, projected into the
/// public catalog and delivered SIGNED, once per subscription, with unique delivery ids
/// and stable event ids; bodies carry no provider / internal payload.
#[tokio::test]
async fn outbox_events_become_signed_public_deliveries() {
    let Some(w) = World::new(dev_settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (sub, secret) = w
        .subscribe(
            &rx.url,
            &["call.requested", "call.ringing", "call.incoming"],
        )
        .await;
    let call = w.call().await;
    assert!(
        w.one("SELECT count(*)::text FROM calling_outbox_events WHERE status = 'PENDING'")
            .await
            != "0"
    );
    let report = w.publish().await;
    assert!(report.published > 0 && report.retried == 0, "{report:?}");
    assert_eq!(
        w.one("SELECT count(*)::text FROM calling_outbox_events WHERE status <> 'PUBLISHED'")
            .await,
        "0"
    );
    // Re-publishing (a replayed / duplicated publication) never fans out twice.
    let replayed_id = w
        .one("SELECT event_id FROM calling_outbox_events LIMIT 1")
        .await;
    let transport = LocalWebhookTransport::new((*w.e.h().state.webhooks).clone());
    let envelope: Value = serde_json::from_str(
        &w.one(&format!(
            "SELECT envelope::text FROM calling_outbox_events WHERE event_id = '{replayed_id}'"
        ))
        .await,
    )
    .unwrap();
    transport
        .publish("cfmeet.calling", &envelope)
        .await
        .unwrap();
    transport
        .publish("cfmeet.calling", &envelope)
        .await
        .unwrap();
    let deliveries: i64 = w
        .one(&format!(
            "SELECT count(*)::text FROM webhook_deliveries WHERE subscription_id = '{sub}'"
        ))
        .await
        .parse()
        .unwrap();
    let events: i64 = w
        .one("SELECT count(*)::text FROM webhook_events")
        .await
        .parse()
        .unwrap();
    assert_eq!(deliveries, events, "one delivery per (subscription, event)");
    let report = w.deliver().await;
    assert_eq!(report.succeeded as i64, deliveries, "{report:?}");
    let seen = rx.seen();
    assert_eq!(seen.len() as i64, deliveries);
    let mut delivery_ids = std::collections::BTreeSet::new();
    let mut types = std::collections::BTreeSet::new();
    for r in &seen {
        let (id, ts, sig) = (
            header(r, signing::HEADER_DELIVERY_ID),
            header(r, signing::HEADER_TIMESTAMP),
            header(r, signing::HEADER_SIGNATURE),
        );
        let now: i64 = ts.parse().unwrap();
        // The receiver's verification: signature over timestamp + delivery id + body.
        signing::verify(secret.as_bytes(), &sig, &ts, &id, &r.body, now, 300).unwrap();
        // Replayed later (outside the window): refused even though the signature is valid.
        assert_eq!(
            signing::verify(secret.as_bytes(), &sig, &ts, &id, &r.body, now + 301, 300),
            Err(signing::VerifyError::StaleTimestamp)
        );
        // A forged body / delivery id fails.
        assert!(signing::verify(secret.as_bytes(), &sig, &ts, &id, b"{}", now, 300).is_err());
        assert!(delivery_ids.insert(id));
        let body: Value = serde_json::from_slice(&r.body).unwrap();
        common::contracts::check(
            "platform/webhooks/v1.schema.json#/$defs/WebhookEvent",
            &body,
        );
        assert_eq!(body["id"], header(r, signing::HEADER_EVENT_ID));
        assert_eq!(body["data"]["callId"], call.to_string());
        types.insert(body["type"].as_str().unwrap().to_string());
        let text = String::from_utf8_lossy(&r.body).to_string();
        for forbidden in [
            "tenant_id",
            "envelope",
            "payload",
            "producer_service",
            "+234",
            "sip:",
            "provider",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }
    }
    assert!(
        types.contains("call.requested") && types.contains("call.ringing"),
        "{types:?}"
    );
    assert!(types
        .iter()
        .all(|t| ["call.requested", "call.ringing", "call.incoming"].contains(&t.as_str())));
    // Delivery history (safe view).
    let r = w
        .send(
            Method::GET,
            &format!("/v1/webhooks/{sub}/deliveries?status=SUCCEEDED"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check(
        "platform/webhooks/v1.schema.json#/$defs/WebhookDeliveryList",
        &r.json,
    );
    assert_eq!(r.json["data"].as_array().unwrap().len() as i64, deliveries);
}

/// §68: transient failures retry with backoff (no event loss), a 4xx is permanent, a
/// timeout retries, exhausted retries dead-letter (FAILED, visible), and a manual replay
/// is a NEW delivery of the SAME event — including while the original is still retrying.
#[tokio::test]
async fn retries_dead_letter_timeouts_and_manual_replay() {
    let Some(w) = World::new(dev_settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (sub, _) = w.subscribe(&rx.url, &["call.requested"]).await;
    w.call().await;
    w.publish().await;
    let original = w.one("SELECT id::text FROM webhook_deliveries").await;
    // Receiver down (503): retried, not lost.
    rx.script(&[503]);
    let r = w.deliver().await;
    assert_eq!(r.retried, 1, "{r:?}");
    let state = || async {
        w.one(&format!(
            "SELECT status || ':' || attempt_count || ':' || COALESCE(failure_category, '') || ':' || \
                    COALESCE(last_http_status::text, '') FROM webhook_deliveries WHERE id = '{original}'"
        ))
        .await
    };
    assert_eq!(state().await, "RETRY_SCHEDULED:1:HTTP_5XX:503");
    // Not due yet: nothing happens.
    assert_eq!(w.deliver().await.retried, 0);
    // A manual replay while the original is still retrying: a separate delivery.
    let r = w
        .send(
            Method::POST,
            &format!("/v1/webhook-deliveries/{original}/replay"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    common::contracts::check(
        "platform/webhooks/v1.schema.json#/$defs/WebhookDelivery",
        &r.json,
    );
    let replay = r.json["deliveryId"].as_str().unwrap().to_string();
    assert_eq!(r.json["replayOf"], original);
    // The replay is due now: the receiver is still down (503), it retries too.
    rx.script(&[503]);
    w.deliver().await;
    let replay_state = || async {
        w.one(&format!(
            "SELECT status || ':' || attempt_count || ':' || COALESCE(failure_category, '') || ':' || \
                    COALESCE(last_http_status::text, '') FROM webhook_deliveries WHERE id = '{replay}'"
        ))
        .await
    };
    assert_eq!(replay_state().await, "RETRY_SCHEDULED:1:HTTP_5XX:503");
    // The receiver recovers: both are delivered on their second attempt — nothing lost,
    // the original and the replay are distinct deliveries of the same event.
    w.e.h().clock.advance(chrono::Duration::hours(7));
    w.deliver().await;
    assert_eq!(state().await, "SUCCEEDED:2::200");
    assert_eq!(replay_state().await, "SUCCEEDED:2::200");
    assert_eq!(
        w.one(&format!(
            "SELECT string_agg(outcome || ':' || COALESCE(http_status::text, ''), ',' ORDER BY attempt_number) \
               FROM webhook_delivery_attempts WHERE delivery_id = '{original}'"
        ))
        .await,
        "RETRY:503,SUCCEEDED:200"
    );
    // Both carried the same event id with different delivery ids.
    let seen = rx.seen();
    let ids: std::collections::BTreeSet<String> = seen
        .iter()
        .map(|r| header(r, signing::HEADER_DELIVERY_ID))
        .collect();
    let events: std::collections::BTreeSet<String> = seen
        .iter()
        .map(|r| header(r, signing::HEADER_EVENT_ID))
        .collect();
    assert!(ids.contains(&original) && ids.contains(&replay), "{ids:?}");
    assert_eq!(events.len(), 1, "same event, distinct deliveries");
    // Exhausted retries: a fresh event to a receiver that keeps failing ends FAILED.
    rx.script(&[500, 500, 500, 500, 500, 500]);
    w.call().await;
    w.publish().await;
    for _ in 0..4 {
        w.deliver().await;
        w.e.h().clock.advance(chrono::Duration::hours(7));
    }
    let failed = w
        .one(&format!(
            "SELECT count(*)::text FROM webhook_deliveries WHERE subscription_id = '{sub}' \
               AND status = 'FAILED' AND attempt_count = 3 AND replay_of IS NULL"
        ))
        .await;
    assert!(failed != "0", "dead-lettered after bounded retries");
    let r = w
        .send(
            Method::GET,
            &format!("/v1/webhooks/{sub}/deliveries?status=FAILED"),
            None,
        )
        .await;
    assert!(!r.json["data"].as_array().unwrap().is_empty());
    // A permanent 4xx is not retried.
    rx.script(&[400]);
    w.call().await;
    w.publish().await;
    w.deliver().await;
    assert!(
        w.one(
            "SELECT count(*)::text FROM webhook_deliveries WHERE failure_category = 'HTTP_4XX' \
               AND status = 'FAILED' AND attempt_count = 1"
        )
        .await
            != "0"
    );
    // A timeout (receiver slower than the 1 s request timeout) is retried.
    rx.script(&[]);
    rx.delay(2500);
    w.call().await;
    w.publish().await;
    w.deliver().await;
    assert!(
        w.one(
            "SELECT count(*)::text FROM webhook_deliveries WHERE failure_category = 'TIMEOUT' \
               AND status = 'RETRY_SCHEDULED'"
        )
        .await
            != "0"
    );
}

/// §67 / §68 reliability: two workers never claim the same delivery; a worker that
/// crashes after its claim loses the lease and the delivery is reclaimed and delivered
/// once; an event matched twice fans out once.
#[tokio::test]
async fn concurrent_workers_and_crash_recovery_deliver_once() {
    let Some(w) = World::new(dev_settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    w.subscribe(&rx.url, &["call.requested", "call.ringing"])
        .await;
    for _ in 0..3 {
        w.call().await;
    }
    w.publish().await;
    let total: usize = w
        .one("SELECT count(*)::text FROM webhook_deliveries")
        .await
        .parse()
        .unwrap();
    let svc = w.e.h().state.webhooks.clone();
    let (a, b) = tokio::join!(svc.claim_due(100), svc.claim_due(100));
    let (a, b) = (a.unwrap(), b.unwrap());
    let ids_a: std::collections::BTreeSet<Uuid> = a.iter().map(|c| c.id).collect();
    assert!(b.iter().all(|c| !ids_a.contains(&c.id)), "disjoint claims");
    assert_eq!(a.len() + b.len(), total);
    // Both "workers" crash before delivering: nothing is sent, nothing is due yet.
    assert_eq!(svc.deliver_due_once(100).await.unwrap().succeeded, 0);
    assert!(rx.seen().is_empty());
    // The leases expire; the deliveries are reclaimed and delivered exactly once each.
    w.e.h().clock.advance(chrono::Duration::seconds(60));
    let (x, y) = tokio::join!(svc.deliver_due_once(100), svc.deliver_due_once(100));
    assert_eq!(x.unwrap().succeeded + y.unwrap().succeeded, total);
    assert_eq!(rx.seen().len(), total);
    assert_eq!(
        w.one("SELECT count(*)::text FROM webhook_deliveries WHERE status <> 'SUCCEEDED'")
            .await,
        "0"
    );
    // Both claims were attempt 1 (lost) and attempt 2 (delivered): the history is honest.
    assert_eq!(
        w.one("SELECT count(DISTINCT attempt_count)::text FROM webhook_deliveries")
            .await,
        "1"
    );
}

/// A failing transport that counts calls.
struct Down;

#[async_trait::async_trait]
impl EventTransport for Down {
    async fn publish(&self, _: &str, _: &Value) -> Result<(), &'static str> {
        Err("BROKER_UNAVAILABLE")
    }
}

/// §67: with the event transport unavailable the outbox stays authoritative (PENDING,
/// bounded failure code, backoff), dead-letters after bounded attempts, and an operator
/// replay publishes it once the transport is back.
#[tokio::test]
async fn transport_outage_keeps_the_outbox_authoritative() {
    let Some(w) = World::new(dev_settings()).await else {
        return;
    };
    w.call().await;
    let svc = w.e.h().state.webhooks.clone();
    let pending: i64 = w
        .one("SELECT count(*)::text FROM calling_outbox_events WHERE status = 'PENDING'")
        .await
        .parse()
        .unwrap();
    assert!(pending > 0);
    let r = svc.publish_outbox_once(&Down, 1000).await.unwrap();
    assert_eq!(r.retried as i64, pending);
    assert_eq!(
        w.one(
            "SELECT count(*)::text FROM calling_outbox_events WHERE status = 'PENDING' \
                 AND last_error_code = 'BROKER_UNAVAILABLE' AND next_attempt_at > created_at"
        )
        .await
        .parse::<i64>()
        .unwrap(),
        pending
    );
    for _ in 0..12 {
        w.e.h().clock.advance(chrono::Duration::minutes(10));
        svc.publish_outbox_once(&Down, 1000).await.unwrap();
    }
    assert_eq!(
        w.one("SELECT count(*)::text FROM calling_outbox_events WHERE status = 'DEAD_LETTER'")
            .await
            .parse::<i64>()
            .unwrap(),
        pending
    );
    let event = w
        .one("SELECT event_id FROM calling_outbox_events WHERE status = 'DEAD_LETTER' LIMIT 1")
        .await;
    assert!(svc.replay_dead_lettered_outbox(&event).await.unwrap());
    assert!(
        !svc.replay_dead_lettered_outbox(&event).await.unwrap(),
        "replay is idempotent"
    );
    let r = w.publish().await;
    assert_eq!(r.published, 1);
    assert_eq!(
        w.one(&format!(
            "SELECT status FROM calling_outbox_events WHERE event_id = '{event}'"
        ))
        .await,
        "PUBLISHED"
    );
}

/// §67 / §68: a committed Privacy Relay event reaches Calling's platform-event ingress
/// only with `platform-event.publish` from Privacy Relay itself, is projected (relay.*,
/// safe fields only — never a number in its payload) and delivered once.
#[tokio::test]
async fn privacy_relay_events_are_normalized_before_delivery() {
    let Some(w) = World::new(dev_settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    w.subscribe(&rx.url, &["relay.revoked"]).await;
    let s = w.e.a.w.scope();
    let envelope = json!({
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "privacy-relay.session.revoked",
        "schema_version": 1,
        "occurred_at": "2031-03-01T10:00:00.000000Z",
        "producer_service": "cfmeet.privacy-relay",
        "runtime_environment": "test",
        "correlation_id": "corr_relay_1",
        "ownership": {
            "tenant_id": s.tenant_id().as_uuid(),
            "application_id": s.application_id().as_uuid(),
            "application_environment_id": s.environment_id().as_uuid(),
        },
        "payload": {
            "context_id": Uuid::new_v4(), "session_id": Uuid::new_v4(), "session_status": "REVOKED",
            "data": { "relay_number": "+2348012345678", "provider_number_id": "PN123" },
        },
    });
    let world = &w;
    let send = |signer: Arc<cf_service_auth::ServiceTokenSigner>, body: Value| async move {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
            .unwrap();
        let headers: Vec<(String, String)> = signed
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        world
            .e
            .h()
            .request_with(
                Method::POST,
                "/internal/v1/platform/events",
                None,
                Some(body),
                &refs,
            )
            .await
    };
    let peers = w.e.h().peers.clone();
    // Telephony is not granted platform-event.publish; a foreign producer is refused.
    let r = send(peers.telephony.clone(), envelope.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let mut forged = envelope.clone();
    forged["producer_service"] = json!("cfmeet.telephony");
    let r = send(peers.privacy_relay.clone(), forged).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Delivered twice (at-least-once publication): one public event, one delivery.
    for _ in 0..2 {
        let r = send(peers.privacy_relay.clone(), envelope.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    assert_eq!(
        w.one("SELECT count(*)::text FROM webhook_deliveries").await,
        "1"
    );
    w.deliver().await;
    let seen = rx.seen();
    assert_eq!(seen.len(), 1);
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    common::contracts::check(
        "platform/webhooks/v1.schema.json#/$defs/WebhookEvent",
        &body,
    );
    assert_eq!(body["type"], "relay.revoked");
    assert_eq!(body["data"]["resource"], "SESSION");
    let text = String::from_utf8_lossy(&seen[0].body);
    assert!(
        !text.contains("234") && !text.contains("PN123") && !text.contains("relay_number"),
        "{text}"
    );
    // An internal-only fact (relay numbers) is acknowledged but never public.
    let mut internal = envelope.clone();
    internal["event_id"] = json!(format!("evt_{}", Uuid::new_v4().simple()));
    internal["event_type"] = json!("privacy-relay.relay-number.assigned");
    let r = send(peers.privacy_relay.clone(), internal).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        w.one("SELECT count(*)::text FROM webhook_events").await,
        "1"
    );
}

/// §68 DNS rebinding at delivery: a subscription whose (accepted) host name resolves to
/// a prohibited address when the delivery connects is never reached — FAILED
/// ENDPOINT_BLOCKED, nothing received.
#[tokio::test]
async fn deliveries_never_connect_to_rebound_addresses() {
    let Some(w) = World::new(dev_settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let port = rx
        .url
        .split(':')
        .nth(2)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let url = format!("http://rebind.example.com:{port}/hook");
    w.subscribe(&url, &["call.requested"]).await;
    w.call().await;
    w.publish().await;
    // Production resolution policy (no private networks), http allowed for the local
    // receiver; the name "rebinds" to loopback at connection time.
    let policy = EndpointPolicy {
        allow_insecure_http: true,
        allow_private_networks: false,
    };
    let rebinding: cf_webhooks::endpoint::Lookup = Arc::new(|_host: String| {
        Box::pin(async { Ok(vec![std::net::SocketAddr::from(([127, 0, 0, 1], 0))]) })
    });
    let mut settings = dev_settings();
    settings.policy = policy;
    let svc = calling::webhooks::WebhookService::new(
        w.e.h().state.db.clone(),
        w.e.h().state.clock.clone(),
        settings,
    )
    .with_client(cf_webhooks::endpoint::client_with(
        cf_webhooks::endpoint::GuardedResolver::with_lookup(policy, rebinding),
        StdDuration::from_secs(2),
    ));
    let r = svc.deliver_due_once(10).await.unwrap();
    assert_eq!(r.failed, 1, "{r:?}");
    assert!(
        rx.seen().is_empty(),
        "the internal address was never contacted"
    );
    assert_eq!(
        w.one("SELECT failure_category FROM webhook_deliveries")
            .await,
        "ENDPOINT_BLOCKED"
    );
}

fn keyring(current: (&str, u8), previous: Option<(&str, u8)>) -> WebhookSettings {
    use cf_webhooks::secret_box::SealingKey;
    WebhookSettings {
        secret_box: Some(Arc::new(
            SecretBox::new(
                SealingKey::from_bytes(current.0, &key_bytes(current.1)).unwrap(),
                previous.map(|(kid, seed)| SealingKey::from_bytes(kid, &key_bytes(seed)).unwrap()),
            )
            .unwrap(),
        )),
        ..dev_settings()
    }
}

/// Part H §74.12: the deployment key that seals signing secrets rotates without
/// downtime. Subscriptions created under key A keep delivering correctly signed requests
/// after B becomes current (A previous); `reseal` moves every row to B in bounded,
/// idempotent batches; with A removed everything still works; a row under a key the
/// deployment does not hold is refused, counted and never delivered unsigned — and is what
/// startup refuses to run with.
#[tokio::test]
async fn deployment_key_rotation_keeps_deliveries_signed_and_reseals() {
    use calling::webhooks::{keys::unknown_key_ids, WebhookService};
    let Some(w) = World::new(keyring(("key-a", 1), None)).await else {
        return;
    };
    let rx = Receiver::start().await;
    let mut subs = Vec::new();
    for _ in 0..3 {
        subs.push(w.subscribe(&rx.url, &["call.requested"]).await);
    }
    assert_eq!(
        w.one("SELECT COALESCE(string_agg(DISTINCT secret_key_id, ','), 'none') FROM webhook_subscriptions")
            .await,
        "key-a"
    );
    assert_eq!(
        w.one("SELECT count(*)::text FROM webhook_subscriptions WHERE secret_sealed LIKE 'v2.key-a.%'")
            .await,
        "3"
    );
    let h = w.e.h();
    let service = |settings: WebhookSettings| {
        WebhookService::new(h.conn().clone(), Arc::new(h.clock.clone()), settings)
    };
    let verify_all = |seen: &[Received]| {
        for r in seen {
            let id = header(r, signing::HEADER_DELIVERY_ID);
            let ts = header(r, signing::HEADER_TIMESTAMP);
            let sig = header(r, signing::HEADER_SIGNATURE);
            let now: i64 = ts.parse().unwrap();
            // Exactly one subscription's secret verifies each request.
            let ok = subs
                .iter()
                .filter(|(_, secret)| {
                    signing::verify(secret.as_bytes(), &sig, &ts, &id, &r.body, now, 300).is_ok()
                })
                .count();
            assert_eq!(
                ok, 1,
                "delivery {id} not signed with its subscription secret"
            );
        }
    };

    // Rotate: B current, A previous. Deliveries of rows still sealed under A are signed.
    let rotated = service(keyring(("key-b", 2), Some(("key-a", 1))));
    assert!(unknown_key_ids(h.conn(), rotated.key_ring().unwrap())
        .await
        .unwrap()
        .is_empty());
    w.call().await;
    w.publish().await;
    let report = rotated.deliver_due_once(100).await.unwrap();
    assert_eq!(report.succeeded, 3, "{report:?}");
    verify_all(&rx.seen());
    // A new subscription made under the rotated deployment seals with B.
    let status = rotated.key_status().await.unwrap();
    assert_eq!(status.current_key_id, "key-b");
    assert_eq!(status.pending_reseal, 3);
    assert_eq!(
        status
            .keys
            .iter()
            .map(|k| (k.resolved_key_id.as_str(), k.role, k.subscriptions))
            .collect::<Vec<_>>(),
        [("key-a", "PREVIOUS", 3)]
    );

    // Reseal in batches of 2: every row ends under B; re-running is a no-op.
    let first = rotated.reseal_batch(None, 2).await.unwrap();
    assert_eq!((first.resealed, first.unopenable), (2, 0));
    let second = rotated.reseal_batch(first.last_id, 2).await.unwrap();
    assert_eq!((second.resealed, second.unopenable), (1, 0));
    let done = rotated.reseal_batch(second.last_id, 2).await.unwrap();
    assert_eq!((done.resealed, done.last_id), (0, None));
    assert_eq!(rotated.reseal_batch(None, 100).await.unwrap().resealed, 0);
    assert_eq!(
        w.one(
            "SELECT count(*)::text FROM webhook_subscriptions WHERE secret_key_id = 'key-b' \
                AND secret_sealed LIKE 'v2.key-b.%'"
        )
        .await,
        "3"
    );
    assert_eq!(rotated.key_status().await.unwrap().pending_reseal, 0);
    // The sealed value never contains the secret, and the secret itself is unchanged
    // (customers do not have to update their receivers for a deployment-key rotation).
    let sealed = strings(h.conn(), "SELECT secret_sealed FROM webhook_subscriptions").await;
    for (_, secret) in &subs {
        assert!(sealed.iter().all(|s| !s.contains(secret.as_str())));
    }

    // Drop A: B alone still signs every subscription correctly.
    let only_b = service(keyring(("key-b", 2), None));
    assert!(unknown_key_ids(h.conn(), only_b.key_ring().unwrap())
        .await
        .unwrap()
        .is_empty());
    let before = rx.seen().len();
    w.call().await;
    w.publish().await;
    let report = only_b.deliver_due_once(100).await.unwrap();
    assert_eq!(report.succeeded, 3, "{report:?}");
    let seen = rx.seen();
    assert_eq!(seen.len(), before + 3);
    verify_all(&seen[before..]);

    // A row sealed under a key this deployment does not hold: refused and counted, never
    // sent unsigned; startup would refuse to run with it.
    let (victim, _) = &subs[0];
    let foreign = cf_webhooks::secret_box::SecretBox::new(
        cf_webhooks::secret_box::SealingKey::from_bytes("key-z", &key_bytes(9)).unwrap(),
        None,
    )
    .unwrap()
    .seal("whsec_foreign", &victim.to_string());
    common::exec(
        h.conn(),
        &format!(
            "UPDATE webhook_subscriptions SET secret_sealed = '{foreign}', secret_key_id = 'key-z' \
              WHERE id = '{victim}'"
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        unknown_key_ids(h.conn(), only_b.key_ring().unwrap())
            .await
            .unwrap(),
        ["key-z"]
    );
    let refusals = || {
        calling::metrics::counter(
            "calling_webhook_signing_refusals_total",
            &[("reason", "SIGNING_KEY_UNKNOWN")],
        )
    };
    let refused_before = refusals();
    let before = rx.seen().len();
    w.call().await;
    w.publish().await;
    let report = only_b.deliver_due_once(100).await.unwrap();
    assert_eq!((report.succeeded, report.retried), (2, 1), "{report:?}");
    assert_eq!(
        rx.seen().len(),
        before + 2,
        "nothing sent for the unknown key"
    );
    // The two delivered requests are the other subscriptions', correctly signed.
    verify_all(&rx.seen()[before..]);
    assert_eq!(
        strings(
            h.conn(),
            &format!(
                "SELECT COALESCE(failure_category, '') FROM webhook_deliveries \
                  WHERE subscription_id = '{victim}' AND status = 'RETRY_SCHEDULED'"
            ),
        )
        .await,
        ["SIGNING_KEY_UNKNOWN"]
    );
    assert!(refusals() > refused_before);
    // Reseal cannot open it either: counted, left untouched (no silent re-keying).
    let report = only_b.reseal_batch(None, 100).await.unwrap();
    assert_eq!((report.resealed, report.unopenable), (0, 1));
    assert_eq!(
        w.one(&format!(
            "SELECT secret_key_id FROM webhook_subscriptions WHERE id = '{victim}'"
        ))
        .await,
        "key-z"
    );
}

/// Part H §72.8 / §74.12: the operator reseal (`platform_admin webhooks reseal`) re-seals
/// every row in bounded batches and then writes exactly ONE operator-audit row
/// (`WEBHOOK_KEYS_RESEALED`, target the key ring) carrying counts and the batch size only
/// — never a key, a seal or a secret.
#[tokio::test]
async fn operator_reseal_writes_one_audit_row_with_counts_only() {
    use calling::{platform::operator::OperatorContext, webhooks::WebhookService};
    let Some(w) = World::new(keyring(("key-a", 1), None)).await else {
        return;
    };
    let rx = Receiver::start().await;
    let mut secrets = Vec::new();
    for _ in 0..3 {
        secrets.push(w.subscribe(&rx.url, &["call.requested"]).await.1);
    }
    let h = w.e.h();
    let rotated = WebhookService::new(
        h.conn().clone(),
        Arc::new(h.clock.clone()),
        keyring(("key-b", 2), Some(("key-a", 1))),
    );
    let op = OperatorContext::new("ops.keys").unwrap();
    let summary = rotated.operator_reseal_all(&op, 2).await.unwrap();
    assert_eq!(
        (
            summary.resealed,
            summary.unopenable,
            summary.batches,
            summary.batch_size
        ),
        (3, 0, 3, 2)
    );
    assert_eq!(rotated.key_status().await.unwrap().pending_reseal, 0);
    let rows = strings(
        h.conn(),
        &format!(
            "SELECT actor || '|' || target_kind || '|' || COALESCE(target_id::text, '-') || '|' || \
               COALESCE(tenant_id::text, '-') || '|' || detail::text FROM platform_operator_audit \
             WHERE action = 'WEBHOOK_KEYS_RESEALED' AND correlation_id = '{}'",
            op.correlation_id()
        ),
    )
    .await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let detail: Value = serde_json::from_str(rows[0].rsplit('|').next().unwrap()).unwrap();
    assert!(
        rows[0].starts_with("ops.keys|WEBHOOK_KEY_RING|-|-|"),
        "{rows:?}"
    );
    assert_eq!(
        detail,
        json!({ "resealed": 3, "unopenable": 0, "batches": 3, "batchSize": 2 })
    );
    for secret in &secrets {
        assert!(!rows[0].contains(secret.as_str()));
    }
    assert!(!rows[0].contains("v2.") && !rows[0].contains("key-"));
    // Re-running is a no-op reseal, audited again (one row per command).
    let again = rotated.operator_reseal_all(&op, 2).await.unwrap();
    assert_eq!((again.resealed, again.batches), (0, 1));
    assert_eq!(
        strings(
            h.conn(),
            "SELECT count(*)::text FROM platform_operator_audit WHERE action = 'WEBHOOK_KEYS_RESEALED'",
        )
        .await,
        ["2"]
    );
}

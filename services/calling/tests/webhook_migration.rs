//! Part VI §53 webhook migration (Section 53 §9.1 / §19.3), Calling's side, against a real
//! PostgreSQL, the real router and real service tokens: the controlled export (inventory,
//! digests, deterministic keyset pages with ids preserved, signing secrets ONLY re-sealed
//! under the migration transfer key, unopenable secrets exported without any material),
//! the freeze / delivery / retired-authority flags, the rollback import, and that no
//! plaintext secret reaches an HTTP response, the database or the logs.

mod common;

use std::{
    collections::BTreeSet,
    io::Write,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    routing::post,
    Router,
};
use calling::{
    platform::scopes::names,
    webhooks::{
        ownership_migration::{self, EXPORT_PURPOSE, ROLLBACK_PURPOSE},
        worker::LocalWebhookTransport,
        WebhookService, WebhookSettings,
    },
};
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use cf_webhooks::{
    endpoint::EndpointPolicy,
    secret_box::{SealingKey, SecretBox},
    signing,
};
use common::{
    enterprise::{credential, strings, EnterpriseWorld},
    TestResponse,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Receiver, keys, settings.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Received {
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Default)]
struct ReceiverState {
    seen: Vec<Received>,
    script: Vec<u16>,
    /// `Retry-After` sent with every non-2xx answer.
    retry_after: Option<&'static str>,
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
) -> axum::response::Response {
    use axum::response::IntoResponse;
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
    let mut r = StatusCode::from_u16(status).unwrap().into_response();
    if let (Some(v), false) = (s.retry_after, (200..300).contains(&status)) {
        r.headers_mut().insert("retry-after", v.parse().unwrap());
    }
    r
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
}

fn key_bytes(seed: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(7));
    }
    k
}

fn ring(kid: &str, seed: u8) -> Arc<SecretBox> {
    Arc::new(SecretBox::new(SealingKey::from_bytes(kid, &key_bytes(seed)).unwrap(), None).unwrap())
}

/// The migration transfer key both services hold (under their own variable names).
fn transfer() -> Arc<SecretBox> {
    ring("transfer-1", 91)
}

fn settings() -> WebhookSettings {
    WebhookSettings {
        secret_box: Some(ring("calling-1", 42)),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        request_timeout: StdDuration::from_secs(1),
        max_attempts: 2,
        transfer_key: Some(transfer()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// World.
// ---------------------------------------------------------------------------

struct World {
    e: EnterpriseWorld,
    manager: common::IssuedCredential,
}

impl World {
    async fn new(settings: WebhookSettings) -> Option<Self> {
        let e = EnterpriseWorld::configured(move |rt| rt.webhooks = settings).await?;
        for scope in [names::WEBHOOKS_READ, names::WEBHOOKS_MANAGE] {
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
            ],
        )
        .await;
        Some(Self { e, manager })
    }

    fn h(&self) -> &common::Harness {
        self.e.h()
    }

    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.e
            .send(self.manager.secret.expose_once(), method, path, body)
            .await
    }

    /// A customer request through a router whose webhook service uses `settings`
    /// (same database and clock): how a restarted deployment with other flags answers.
    async fn send_with(
        &self,
        settings: WebhookSettings,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> TestResponse {
        let mut state = self.h().state.clone();
        state.webhooks = Arc::new(self.service(settings));
        let router = calling::routes::app(
            state,
            calling::routes::public_api(),
            calling::routes::internal_api(),
        );
        let mut builder = Request::builder().method(method).uri(path).header(
            "authorization",
            format!("Bearer {}", self.manager.secret.expose_once()),
        );
        let body = match body {
            Some(b) => {
                builder = builder.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let response = router.oneshot(builder.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        TestResponse {
            status,
            headers,
            json: serde_json::from_str(&text).unwrap_or(Value::Null),
            text,
        }
    }

    fn service(&self, settings: WebhookSettings) -> WebhookService {
        WebhookService::new(
            self.h().conn().clone(),
            Arc::new(self.h().clock.clone()),
            settings,
        )
    }

    async fn subscribe(&self, url: &str, types: &[&str]) -> (Uuid, String) {
        let r = self
            .send(
                Method::POST,
                "/v1/webhooks",
                Some(json!({ "url": url, "eventTypes": types, "description": "migration" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        (
            r.json["webhook"]["webhookId"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
            r.json["signingSecret"].as_str().unwrap().to_string(),
        )
    }

    async fn publish(&self) {
        let w = &self.h().state.webhooks;
        w.publish_outbox_once(&LocalWebhookTransport::new((**w).clone()), 1000)
            .await
            .unwrap();
    }

    async fn deliver(&self) -> calling::webhooks::worker::DeliveryReport {
        self.h().state.webhooks.deliver_due_once(100).await.unwrap()
    }

    async fn one(&self, sql: &str) -> String {
        strings(self.h().conn(), sql)
            .await
            .pop()
            .unwrap_or_default()
    }

    async fn call(&self) {
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
    }

    /// An internal request signed by `signer` (the Developer Platform unless stated).
    async fn internal(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> TestResponse {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
            .unwrap();
        let headers: Vec<(String, String)> = signed
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        self.h().request_with(method, path, None, body, &refs).await
    }

    async fn dp(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        let dp = self.h().peers.developer_platform.clone();
        self.internal(&dp, method, path, body).await
    }

    /// Every page of an export (`limit` rows per page), following the cursor chain.
    async fn export_all(&self, entity: &str, limit: u32, extra: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut path = format!("/internal/v1/webhook-migration/{entity}?limit={limit}{extra}");
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={c}"));
            }
            let r = self.dp(Method::GET, &path, None).await;
            assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text);
            assert_eq!(r.headers["cache-control"], "no-store");
            out.extend(r.json["data"].as_array().unwrap().iter().cloned());
            match r.json["nextCursor"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => break,
            }
        }
        out
    }
}

/// Captures every log line written while the guard lives (this test's thread only).
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Logs {
    type Writer = Logs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
    }
}

fn assert_no_secret(what: &str, text: &str, secrets: &[&str]) {
    for s in secrets {
        assert!(!text.contains(s), "{what} contains a plaintext secret");
        // Not even the random part (base64url) of a `whsec_` value.
        assert!(
            !text.contains(s.trim_start_matches("whsec_")),
            "{what} contains a secret's random part"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// A clean schema exports nothing: zero counts, empty digests, empty pages; defaults are
/// today's behaviour. Without the transfer key the subscription export is refused; the
/// routes need a service token with the receiver-granted permission.
#[tokio::test]
async fn an_empty_calling_exports_nothing_and_reports_default_flags() {
    let Some(w) = World::new(WebhookSettings {
        transfer_key: None,
        ..settings()
    })
    .await
    else {
        return;
    };
    let r = w
        .dp(
            Method::GET,
            "/internal/v1/webhook-migration/inventory",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let inv = &r.json;
    assert_eq!(inv["flags"]["mutationsFrozen"], false);
    assert_eq!(inv["flags"]["deliveryEnabled"], true);
    assert_eq!(inv["flags"]["authority"], "active");
    assert_eq!(inv["flags"]["transferKeyId"], Value::Null);
    for e in ["subscriptions", "events", "deliveries", "attempts"] {
        assert_eq!(inv[e]["total"], 0, "{e}");
    }
    assert_eq!(inv["subscriptions"]["unopenableSecrets"], 0);
    let r = w
        .dp(Method::GET, "/internal/v1/webhook-migration/digest", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    for e in ["subscriptions", "events", "deliveries", "attempts"] {
        assert_eq!(r.json[e]["count"], 0);
        assert_eq!(r.json[e]["digest"], empty);
    }
    for e in ["events", "deliveries", "attempts"] {
        assert!(w.export_all(e, 10, "").await.is_empty());
    }
    let r = w
        .dp(
            Method::GET,
            "/internal/v1/webhook-migration/subscriptions",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");
    // Permission and authentication.
    let gateway = w.h().peers.api_gateway.clone();
    let r = w
        .internal(
            &gateway,
            Method::GET,
            "/internal/v1/webhook-migration/inventory",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let messaging = w.h().peers.messaging.clone();
    let r = w
        .internal(
            &messaging,
            Method::POST,
            "/internal/v1/webhook-migration/rollback/deliveries",
            Some(json!({ "deliveryIds": [] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = w
        .h()
        .request(
            Method::GET,
            "/internal/v1/webhook-migration/digest",
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .dp(
            Method::GET,
            "/internal/v1/webhook-migration/events?cursor=not-a-cursor",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}

/// Existing data in several states (ACTIVE / DISABLED / DELETED subscriptions, delivered,
/// retried, dead-lettered and replayed deliveries, attempts) exports with every id, status
/// and replay reference preserved, in a deterministic order, page by page. Each secret
/// arrives ONLY transfer-sealed (bound to its subscription and the direction) and opens
/// to the original; no plaintext is in any response, row or log line.
#[tokio::test]
async fn existing_data_exports_deterministically_with_secrets_only_transfer_sealed() {
    let Some(w) = World::new(settings()).await else {
        return;
    };
    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let rx = Receiver::start().await;
    let (active, active_secret) = w
        .subscribe(&rx.url, &["call.requested", "call.ringing"])
        .await;
    let (disabled, disabled_secret) = w.subscribe(&rx.url, &["call.requested"]).await;
    let (deleted, deleted_secret) = w.subscribe(&rx.url, &["call.requested"]).await;
    w.call().await;
    w.publish().await;
    // The first delivery fails permanently (dead letter), the second retries once.
    rx.script(&[410, 503]);
    w.deliver().await;
    w.h().clock.advance(chrono::Duration::hours(7));
    w.deliver().await;
    let original = w
        .one(&format!(
            "SELECT id::text FROM webhook_deliveries WHERE subscription_id = '{active}' \
               AND status = 'SUCCEEDED' ORDER BY created_at, id LIMIT 1"
        ))
        .await;
    let r = w
        .send(
            Method::POST,
            &format!("/v1/webhook-deliveries/{original}/replay"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let replay = r.json["deliveryId"].as_str().unwrap().to_string();
    let r = w
        .send(
            Method::PATCH,
            &format!("/v1/webhooks/{disabled}"),
            Some(json!({ "expectedVersion": 1, "status": "DISABLED" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = w
        .send(
            Method::DELETE,
            &format!("/v1/webhooks/{deleted}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_ne!(
        w.one("SELECT count(*)::text FROM webhook_deliveries WHERE status = 'FAILED'")
            .await,
        "0",
        "a dead letter exists"
    );

    // Inventory.
    let inv = w
        .dp(
            Method::GET,
            "/internal/v1/webhook-migration/inventory",
            None,
        )
        .await;
    assert_eq!(inv.status, StatusCode::OK, "{}", inv.text);
    assert_eq!(inv.json["subscriptions"]["total"], 3);
    for s in ["ACTIVE", "DISABLED", "DELETED"] {
        assert_eq!(inv.json["subscriptions"]["byStatus"][s], 1, "{s}");
    }
    assert_eq!(inv.json["subscriptions"]["unopenableSecrets"], 0);
    assert_eq!(inv.json["flags"]["transferKeyId"], "transfer-1");

    // Subscriptions, one per page: the cursor chain is complete and ordered by id.
    let subs = w.export_all("subscriptions", 1, "").await;
    assert_eq!(subs.len(), 3);
    let ids: Vec<String> = subs
        .iter()
        .map(|s| s["webhookId"].as_str().unwrap().to_string())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "id order");
    let t = transfer();
    for (id, secret, status) in [
        (active, &active_secret, "ACTIVE"),
        (disabled, &disabled_secret, "DISABLED"),
        (deleted, &deleted_secret, "DELETED"),
    ] {
        let s = subs
            .iter()
            .find(|s| s["webhookId"] == id.to_string())
            .unwrap();
        assert_eq!(s["status"], status);
        // From Calling's Platform Access projection of the environment.
        assert!(["SANDBOX", "TEST", "PRODUCTION"].contains(&s["environmentKind"].as_str().unwrap()));
        assert_eq!(s["secret"]["state"], "TRANSFER_SEALED");
        assert_eq!(s["secret"]["transferKeyId"], "transfer-1");
        let sealed = s["secret"]["transferSealed"].as_str().unwrap();
        assert!(sealed.starts_with("v2.transfer-1."));
        // Opens to the ORIGINAL secret only for this subscription and this direction.
        assert_eq!(
            t.open(
                sealed,
                &ownership_migration::transfer_owner(EXPORT_PURPOSE, id)
            )
            .unwrap(),
            *secret
        );
        assert!(t
            .open(
                sealed,
                &ownership_migration::transfer_owner(ROLLBACK_PURPOSE, id)
            )
            .is_err());
        assert!(t
            .open(
                sealed,
                &ownership_migration::transfer_owner(EXPORT_PURPOSE, Uuid::new_v4())
            )
            .is_err());
        assert_eq!(
            s["secret"]["fingerprint"],
            ownership_migration::secret_fingerprint(secret, id)
        );
        // Calling's own key ring cannot open the transfer seal (separate custody).
        assert!(ring("calling-1", 42).open(sealed, &id.to_string()).is_err());
    }
    assert!(subs
        .iter()
        .find(|s| s["webhookId"] == deleted.to_string())
        .unwrap()["deletedAt"]
        .is_string());
    // Deterministic: a second export is identical except for the (random-nonce) seals.
    let strip = |v: &[Value]| -> Vec<Value> {
        v.iter()
            .cloned()
            .map(|mut s| {
                s["secret"]["transferSealed"] = Value::Null;
                s
            })
            .collect()
    };
    assert_eq!(
        strip(&subs),
        strip(&w.export_all("subscriptions", 2, "").await)
    );

    // Events, deliveries, attempts: ids, replay references and statuses preserved.
    let events = w.export_all("events", 1, "").await;
    assert_eq!(
        events.len() as i64,
        inv.json["events"]["total"].as_i64().unwrap()
    );
    assert!(events
        .iter()
        .all(|e| e["eventId"].as_str().unwrap().starts_with("whe_")
            && e["body"]["id"] == e["eventId"]));
    let deliveries = w.export_all("deliveries", 2, "").await;
    assert_eq!(
        deliveries.len() as i64,
        inv.json["deliveries"]["total"].as_i64().unwrap()
    );
    let pos = |id: &str| {
        deliveries
            .iter()
            .position(|d| d["deliveryId"] == id)
            .unwrap()
    };
    assert!(
        pos(&original) < pos(&replay),
        "an original precedes its replay"
    );
    assert_eq!(deliveries[pos(&replay)]["replayOf"], original.as_str());
    let statuses: BTreeSet<String> = deliveries
        .iter()
        .map(|d| d["status"].as_str().unwrap().to_string())
        .collect();
    assert!(
        statuses.contains("FAILED") && statuses.contains("SUCCEEDED"),
        "{statuses:?}"
    );
    assert_eq!(
        deliveries,
        w.export_all("deliveries", 50, "").await,
        "page size never changes the export"
    );
    let open = w.export_all("deliveries", 50, "&status=open").await;
    assert!(open
        .iter()
        .all(|d| ["PENDING", "RETRY_SCHEDULED", "DELIVERING"]
            .contains(&d["status"].as_str().unwrap())));
    let attempts = w.export_all("attempts", 3, "").await;
    assert_eq!(
        attempts.len() as i64,
        inv.json["attempts"]["total"].as_i64().unwrap()
    );
    assert!(!attempts.is_empty());

    // Digests: counts agree, repeat exactly, and move only for the changed entity.
    let digest = w
        .dp(Method::GET, "/internal/v1/webhook-migration/digest", None)
        .await
        .json;
    assert_eq!(digest["algorithm"], "sha256-canonical-lines-v1");
    assert_eq!(digest["subscriptions"]["count"], 3);
    assert_eq!(
        digest["events"]["count"].as_i64().unwrap(),
        events.len() as i64
    );
    let again = w
        .dp(Method::GET, "/internal/v1/webhook-migration/digest", None)
        .await
        .json;
    assert_eq!(digest, again);
    let r = w
        .send(
            Method::PATCH,
            &format!("/v1/webhooks/{active}"),
            Some(json!({ "expectedVersion": 1, "description": "changed" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let changed = w
        .dp(Method::GET, "/internal/v1/webhook-migration/digest", None)
        .await
        .json;
    assert_ne!(
        changed["subscriptions"]["digest"],
        digest["subscriptions"]["digest"]
    );
    assert_eq!(changed["events"], digest["events"]);

    // No plaintext anywhere: responses, rows, logs.
    let secrets = [
        active_secret.as_str(),
        disabled_secret.as_str(),
        deleted_secret.as_str(),
    ];
    for entity in ["subscriptions", "events", "deliveries", "attempts"] {
        let r = w
            .dp(
                Method::GET,
                &format!("/internal/v1/webhook-migration/{entity}?limit=500"),
                None,
            )
            .await;
        assert_no_secret(entity, &r.text, &secrets);
        assert!(!r.text.contains("whsec_"), "{entity}");
    }
    for table in [
        "webhook_subscriptions",
        "webhook_events",
        "webhook_deliveries",
        "webhook_delivery_attempts",
    ] {
        let rows = w
            .one(&format!(
                "SELECT COALESCE(string_agg(row_to_json(t)::text, ''), '') FROM {table} t"
            ))
            .await;
        assert_no_secret(table, &rows, &secrets);
    }
    let text = logs.text();
    assert!(
        text.contains("subscriptions page exported"),
        "the export is logged"
    );
    assert_no_secret("logs", &text, &secrets);
}

/// A secret Calling cannot open (its key id is not configured) is exported UNOPENABLE
/// with no secret material at all and counted by the inventory: the importer must require
/// rotation — nothing is ever substituted.
#[tokio::test]
async fn unopenable_secrets_are_exported_without_any_material() {
    let Some(w) = World::new(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (id, secret) = w.subscribe(&rx.url, &["call.requested"]).await;
    // A deployment whose key ring no longer holds the key the secret is sealed under.
    let other = w.service(WebhookSettings {
        secret_box: Some(ring("calling-2", 43)),
        ..settings()
    });
    let page = other.export_subscriptions(None, 10).await.unwrap();
    assert_eq!(page.data.len(), 1);
    let s = &page.data[0];
    assert_eq!(s["webhookId"], id.to_string());
    assert_eq!(s["secret"]["state"], "UNOPENABLE");
    assert_eq!(s["secret"]["unopenableReason"], "SIGNING_KEY_UNKNOWN");
    for k in ["transferSealed", "transferKeyId", "fingerprint"] {
        assert_eq!(s["secret"][k], Value::Null, "{k}");
    }
    assert!(!s.to_string().contains(&secret));
    let inv = other.migration_inventory().await.unwrap();
    assert_eq!(inv["subscriptions"]["unopenableSecrets"], 1);
    // The configured deployment still opens it.
    let inv = w.h().state.webhooks.migration_inventory().await.unwrap();
    assert_eq!(inv["subscriptions"]["unopenableSecrets"], 0);
}

/// The freeze (`CALLING_WEBHOOK_MUTATIONS_FROZEN`): create / update / delete /
/// rotate-secret / replay answer 503 SERVICE_UNAVAILABLE with `CF-Webhook-Migration:
/// frozen` and Retry-After, change nothing, while reads and deliveries continue.
#[tokio::test]
async fn the_freeze_refuses_mutations_while_reads_and_delivery_continue() {
    let Some(w) = World::new(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (id, secret) = w.subscribe(&rx.url, &["call.requested"]).await;
    w.call().await;
    w.publish().await;
    let frozen = WebhookSettings {
        mutations_frozen: true,
        ..settings()
    };
    let path = format!("/v1/webhooks/{id}");
    let delivery = w
        .one("SELECT id::text FROM webhook_deliveries LIMIT 1")
        .await;
    for (method, p, body) in [
        (
            Method::POST,
            "/v1/webhooks".to_string(),
            Some(json!({ "url": rx.url, "eventTypes": ["call.requested"] })),
        ),
        (
            Method::PATCH,
            path.clone(),
            Some(json!({ "expectedVersion": 1, "status": "DISABLED" })),
        ),
        (Method::DELETE, format!("{path}?expectedVersion=1"), None),
        (
            Method::POST,
            format!("{path}/rotate-secret"),
            Some(json!({ "expectedVersion": 1 })),
        ),
        (
            Method::POST,
            format!("/v1/webhook-deliveries/{delivery}/replay"),
            None,
        ),
    ] {
        let r = w.send_with(frozen.clone(), method.clone(), &p, body).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {p}: {}",
            r.text
        );
        assert_eq!(r.json["error"]["code"], "SERVICE_UNAVAILABLE");
        assert_eq!(r.headers["cf-webhook-migration"], "frozen");
        assert_eq!(r.headers["retry-after"], "300");
    }
    assert_eq!(
        w.one("SELECT count(*)::text || ':' || max(version)::text FROM webhook_subscriptions")
            .await,
        "1:1",
        "nothing changed"
    );
    // Reads continue.
    for p in [
        path.clone(),
        "/v1/webhooks".into(),
        format!("{path}/deliveries"),
    ] {
        let r = w.send_with(frozen.clone(), Method::GET, &p, None).await;
        assert_eq!(r.status, StatusCode::OK, "{p}: {}", r.text);
    }
    // Delivery continues (signed with the unchanged secret).
    let report = w.service(frozen).deliver_due_once(100).await.unwrap();
    assert!(report.succeeded > 0, "{report:?}");
    let seen = rx.seen();
    let r = &seen[0];
    let h = |n: &str| r.headers.get(n).unwrap().to_str().unwrap().to_string();
    signing::verify(
        secret.as_bytes(),
        &h(signing::HEADER_SIGNATURE),
        &h(signing::HEADER_TIMESTAMP),
        &h(signing::HEADER_DELIVERY_ID),
        &r.body,
        h(signing::HEADER_TIMESTAMP).parse().unwrap(),
        300,
    )
    .unwrap();
}

/// `CALLING_WEBHOOK_DELIVERY_ENABLED=false`: the worker sends nothing and claims nothing;
/// every delivery stays recorded (PENDING) and re-enabling resumes exactly there.
#[tokio::test]
async fn disabled_delivery_sends_nothing_and_loses_no_record() {
    let Some(w) = World::new(WebhookSettings {
        delivery_enabled: false,
        ..settings()
    })
    .await
    else {
        return;
    };
    let rx = Receiver::start().await;
    w.subscribe(&rx.url, &["call.requested", "call.ringing"])
        .await;
    w.call().await;
    w.publish().await;
    let recorded = w.one("SELECT count(*)::text FROM webhook_deliveries").await;
    assert_ne!(recorded, "0");
    for _ in 0..3 {
        assert_eq!(w.deliver().await, Default::default());
        w.h().clock.advance(chrono::Duration::hours(1));
    }
    assert!(rx.seen().is_empty(), "nothing was sent");
    assert_eq!(
        w.one("SELECT count(*)::text FROM webhook_deliveries WHERE status = 'PENDING' AND attempt_count = 0")
            .await,
        recorded,
        "every delivery is still recorded and untouched"
    );
    let inv = w.h().state.webhooks.migration_inventory().await.unwrap();
    assert_eq!(inv["flags"]["deliveryEnabled"], false);
    assert_eq!(inv["deliveries"]["open"].to_string(), recorded);
    // Re-enabled (a rollback): every recorded delivery is sent once.
    let report = w.service(settings()).deliver_due_once(100).await.unwrap();
    assert_eq!(report.succeeded.to_string(), recorded, "{report:?}");
    assert_eq!(rx.seen().len().to_string(), recorded);
}

/// `CALLING_WEBHOOK_AUTHORITY=retired`: every customer webhook route answers 503 with
/// `CF-Webhook-Migration: moved`, the worker is off, the records are retained and the
/// migration export still works.
#[tokio::test]
async fn the_retired_authority_answers_moved_and_stops_delivery() {
    let Some(w) = World::new(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (id, _) = w.subscribe(&rx.url, &["call.requested"]).await;
    w.call().await;
    w.publish().await;
    let retired = WebhookSettings {
        authority: ownership_migration::WebhookAuthority::Retired,
        delivery_enabled: false,
        ..settings()
    };
    for (method, p) in [
        (Method::GET, "/v1/webhooks".to_string()),
        (Method::GET, format!("/v1/webhooks/{id}")),
        (Method::GET, format!("/v1/webhooks/{id}/deliveries")),
        (Method::POST, "/v1/webhooks".to_string()),
    ] {
        let body = (method == Method::POST)
            .then(|| json!({ "url": rx.url, "eventTypes": ["call.requested"] }));
        let r = w.send_with(retired.clone(), method.clone(), &p, body).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{method} {p}");
        assert_eq!(r.headers["cf-webhook-migration"], "moved");
    }
    let svc = w.service(retired);
    assert_eq!(svc.deliver_due_once(100).await.unwrap(), Default::default());
    assert!(rx.seen().is_empty());
    assert_eq!(
        svc.export_subscriptions(None, 10).await.unwrap().data.len(),
        1,
        "data retained and exportable"
    );
    let inv = svc.migration_inventory().await.unwrap();
    assert_eq!(inv["flags"]["authority"], "retired");
    assert_eq!(inv["flags"]["deliveryEnabled"], false);
}

/// The rollback import: a subscription changed at the Developer Platform after the cutover
/// (newer version, rotated secret transfer-sealed for the rollback direction) is applied —
/// the new secret re-sealed under Calling's key ring and used to sign — an older version
/// is left unchanged, a subscription created there is inserted, a foreign transfer seal is
/// refused; delivered-elsewhere deliveries are settled and never sent.
#[tokio::test]
async fn the_rollback_import_applies_newer_subscriptions_and_settles_deliveries() {
    let Some(w) = World::new(WebhookSettings {
        delivery_enabled: false,
        ..settings()
    })
    .await
    else {
        return;
    };
    let rx = Receiver::start().await;
    let (id, original) = w
        .subscribe(&rx.url, &["call.requested", "call.ringing"])
        .await;
    let row = w
        .one(&format!(
            "SELECT tenant_id::text || ',' || application_id::text || ',' || environment_id::text \
               FROM webhook_subscriptions WHERE id = '{id}'"
        ))
        .await;
    let scope: Vec<&str> = row.split(',').collect();
    let t = transfer();
    let rotated = signing::generate_secret();
    let now = chrono::Utc::now();
    let item = |id: Uuid,
                version: i64,
                secret_version: i32,
                sealed: Option<String>,
                types: &[&str]| {
        json!({
            "webhookId": id, "tenantId": scope[0], "applicationId": scope[1],
            "environmentId": scope[2], "url": rx.url, "description": "rolled back",
            "eventTypes": types, "status": "ACTIVE",
            "secretVersion": secret_version, "version": version,
            "createdAt": now, "updatedAt": now, "deletedAt": null,
            "secret": sealed.map(|s| json!({ "transferKeyId": "transfer-1", "transferSealed": s })),
        })
    };
    let created = Uuid::new_v4();
    let created_secret = signing::generate_secret();
    let body = json!({ "subscriptions": [
        item(id, 3, 2, Some(t.seal(&rotated, &ownership_migration::transfer_owner(ROLLBACK_PURPOSE, id))), &["call.requested"]),
        item(created, 1, 1, Some(t.seal(&created_secret, &ownership_migration::transfer_owner(ROLLBACK_PURPOSE, created))), &["voicemail.ready"]),
        // Sealed for the EXPORT direction: refused (direction-bound associated data).
        item(Uuid::new_v4(), 1, 1, Some(t.seal(&created_secret, &ownership_migration::transfer_owner(EXPORT_PURPOSE, Uuid::new_v4()))), &["call.requested"]),
    ]});
    let r = w
        .dp(
            Method::POST,
            "/internal/v1/webhook-migration/rollback/subscriptions",
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["applied"], 2, "{}", r.json);
    assert_eq!(r.json["conflicts"][0]["reason"], "TRANSFER_SEAL_UNOPENABLE");
    assert_eq!(
        w.one(&format!(
            "SELECT version || ':' || secret_version || ':' || array_to_string(event_types, ',') || ':' || description \
               FROM webhook_subscriptions WHERE id = '{id}'"
        ))
        .await,
        "3:2:call.requested:rolled back"
    );
    // Applying the same export again changes nothing (idempotent).
    let r = w
        .dp(
            Method::POST,
            "/internal/v1/webhook-migration/rollback/subscriptions",
            Some(body),
        )
        .await;
    assert_eq!(r.json["applied"], 0);
    assert_eq!(r.json["unchanged"], 2);
    // No plaintext stored; Calling's key ring opens the new seal to the rotated secret.
    let rows = w
        .one("SELECT string_agg(row_to_json(s)::text, '') FROM webhook_subscriptions s")
        .await;
    assert_no_secret("rows", &rows, &[&rotated, &created_secret, &original]);
    // Deliveries: two recorded (delivery disabled); one is settled (the Developer
    // Platform delivered it) and never sent, the other is sent signed with the rotated
    // secret once delivery is re-enabled.
    w.call().await;
    w.publish().await;
    let ids = strings(
        w.h().conn(),
        &format!(
            "SELECT id::text FROM webhook_deliveries WHERE subscription_id = '{id}' ORDER BY id"
        ),
    )
    .await;
    assert!(!ids.is_empty());
    let settled: Uuid = ids[0].parse().unwrap();
    let r = w
        .dp(
            Method::POST,
            "/internal/v1/webhook-migration/rollback/deliveries",
            Some(json!({ "deliveryIds": [settled] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["settled"], 1);
    assert_eq!(
        w.one(&format!(
            "SELECT status || ':' || failure_category FROM webhook_deliveries WHERE id = '{settled}'"
        ))
        .await,
        "CANCELLED:DELIVERED_BY_DEVELOPER_PLATFORM"
    );
    let report = w.service(settings()).deliver_due_once(100).await.unwrap();
    let seen = rx.seen();
    assert_eq!(seen.len(), report.succeeded);
    for r in &seen {
        let h = |n: &str| r.headers.get(n).unwrap().to_str().unwrap().to_string();
        assert_ne!(h(signing::HEADER_DELIVERY_ID), settled.to_string());
        signing::verify(
            rotated.as_bytes(),
            &h(signing::HEADER_SIGNATURE),
            &h(signing::HEADER_TIMESTAMP),
            &h(signing::HEADER_DELIVERY_ID),
            &r.body,
            h(signing::HEADER_TIMESTAMP).parse().unwrap(),
            300,
        )
        .unwrap();
    }
}

/// The receiver's `Retry-After` on a 429 (or 503) is honoured: the retry is scheduled no
/// earlier than it asks (never earlier than the backoff either).
#[tokio::test]
async fn a_receivers_retry_after_is_honoured() {
    use calling::domain::Clock;
    let Some(w) = World::new(WebhookSettings {
        max_attempts: 5,
        ..settings()
    })
    .await
    else {
        return;
    };
    let rx = Receiver::start().await;
    w.subscribe(&rx.url, &["call.requested"]).await;
    w.call().await;
    w.publish().await;
    {
        let mut s = rx.state.lock().unwrap();
        s.script = vec![429];
        s.retry_after = Some("120");
    }
    let now = w.h().clock.now();
    let report = w.deliver().await;
    assert_eq!(report.retried, 1, "{report:?}");
    let delay: i64 = w
        .one(&format!(
            "SELECT (extract(epoch FROM next_attempt_at - '{}'::timestamptz))::bigint::text \
               FROM webhook_deliveries WHERE status = 'RETRY_SCHEDULED'",
            now.to_rfc3339()
        ))
        .await
        .parse()
        .unwrap();
    assert!(delay >= 120, "next attempt in {delay}s");
    // A short Retry-After never shortens the backoff (at least 15 s for the first retry).
    {
        let mut s = rx.state.lock().unwrap();
        s.script = vec![503];
        s.retry_after = Some("1");
    }
    w.h().clock.advance(chrono::Duration::hours(1));
    let now = w.h().clock.now();
    assert_eq!(w.deliver().await.retried, 1);
    let delay: i64 = w
        .one(&format!(
            "SELECT (extract(epoch FROM next_attempt_at - '{}'::timestamptz))::bigint::text \
               FROM webhook_deliveries WHERE status = 'RETRY_SCHEDULED'",
            now.to_rfc3339()
        ))
        .await
        .parse()
        .unwrap();
    assert!(delay >= 15, "next attempt in {delay}s");
}

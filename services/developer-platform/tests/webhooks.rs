//! Section 53 — central customer webhooks through the REAL router (real gateway and
//! producer service tokens), a real PostgreSQL database and a real local HTTP receiver.
//! Platform Access and the session authority are the in-memory fakes of the harness.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use axum::{
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    routing::post,
    Router,
};
use bytes::Bytes;
use cf_service_auth::{DelegatedPlatformContext, ServiceTokenSigner};
use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox, signing};
use common::*;
use developer_platform::webhooks::{DeliveryMode, WebhookService, WebhookSettings};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const WH: &str = "platform-developer/webhooks/v1.schema.json#/$defs/";
const CALLING_WH: &str = "platform/webhooks/v1.schema.json#/$defs/";
const ERR: &str = "platform-developer/common/v1.schema.json#/$defs/Error";
const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";

// ---------------------------------------------------------------------------
// A local webhook receiver.
// ---------------------------------------------------------------------------

#[derive(Clone)]
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
    port: u16,
}

async fn hook(
    State(state): State<Arc<Mutex<ReceiverState>>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, [(&'static str, &'static str); 1]) {
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
    (
        StatusCode::from_u16(status).unwrap(),
        [("location", "/elsewhere")],
    )
}

impl Receiver {
    async fn start() -> Self {
        let state = Arc::new(Mutex::new(ReceiverState::default()));
        let app = Router::new()
            .route("/hook", post(hook))
            .route("/elsewhere", post(hook))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            state,
            url: format!("http://127.0.0.1:{port}/hook"),
            port,
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

fn header(r: &Received, name: &str) -> String {
    r.headers
        .get(name)
        .unwrap_or_else(|| panic!("{name} missing"))
        .to_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// A deterministic, non-weak 32-byte test key.
fn key_bytes(seed: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(7));
    }
    k
}

/// The local-runtime settings: active delivery, private http endpoints allowed (the local
/// receiver), short timeout, three attempts.
fn settings() -> WebhookSettings {
    WebhookSettings {
        enabled: true,
        key_ring: Some(Arc::new(SecretBox::from_bytes(&key_bytes(42)).unwrap())),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        mode: DeliveryMode::Active,
        request_timeout: StdDuration::from_secs(1),
        max_attempts: 3,
        auto_disable_after: 50,
        ..Default::default()
    }
}

type Scope3 = (Uuid, Uuid, Uuid);

/// A Platform Access environment (tenant, application, environment) of `kind`.
fn pa_env(c: &Control, tenant: Uuid, app: Uuid, kind: &str) -> Scope3 {
    let id = Uuid::new_v4();
    c.pa.envs.lock().unwrap().insert(
        id,
        MemEnv {
            tenant,
            app,
            kind: kind.into(),
            status: "ACTIVE".into(),
        },
    );
    (tenant, app, id)
}

fn envelope(producer: &str, event_type: &str, scope: Scope3, payload: Value) -> Value {
    json!({
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": event_type,
        "schema_version": 1,
        "occurred_at": "2026-10-23T10:00:00Z",
        "producer_service": producer,
        "runtime_environment": "test",
        "correlation_id": "corr_0a1b2c3d4e5f60718293a4b5c6d7e8f9",
        "ownership": {
            "tenant_id": scope.0,
            "application_id": scope.1,
            "application_environment_id": scope.2,
        },
        "payload": payload,
    })
}

/// A Calling `call.requested` envelope whose internal payload carries unsafe data.
fn call_requested(scope: Scope3) -> Value {
    envelope(
        "cfmeet.calling",
        "calling.call-requested",
        scope,
        json!({
            "event_type": "CALL_REQUESTED",
            "call_id": Uuid::new_v4(),
            "call_version": 1,
            "data": { "caller_number": "+15550100123", "provider_call_id": "CA123",
                      "sip_uri": "sip:alice@example.com", "reason": "OUTBOUND" },
        }),
    )
}

async fn publish(c: &Control, signer: &ServiceTokenSigner, env: &Value) -> Response {
    c.internal(
        signer,
        Method::POST,
        "/internal/v1/platform/events",
        Some(env.clone()),
    )
    .await
}

async fn one(c: &Control, sql: &str) -> String {
    c.db.conn
        .query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

async fn delivery_state(c: &Control, id: &str) -> String {
    one(
        c,
        &format!(
            "SELECT status || ':' || attempt_count || ':' || COALESCE(failure_category, '') || ':' || \
                    COALESCE(last_http_status::text, '') FROM devplat_webhook_deliveries WHERE id = '{id}'"
        ),
    )
    .await
}

/// Subscribes `d`'s environment to `types` at `url`; returns (id, secret).
async fn subscribe(
    c: &Control,
    d: &DelegatedPlatformContext,
    url: &str,
    types: &[&str],
) -> (String, String) {
    let r = c
        .req(
            As::Machine(d),
            Method::POST,
            "/v1/webhooks",
            Some(json!({ "url": url, "eventTypes": types, "description": "test hook" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{WH}WebhookWithSecret"), &r.json);
    (
        r.json["webhook"]["webhookId"].as_str().unwrap().to_string(),
        r.json["signingSecret"].as_str().unwrap().to_string(),
    )
}

const ALL: &[&str] = &[
    "webhooks.read",
    "webhooks.manage",
    "calls.read",
    "numbers.read",
    "messages.read",
    "meetings.read",
];

/// A harness with webhooks, one sandbox environment and a credential holding [`ALL`].
async fn world(
    s: WebhookSettings,
) -> Option<(Control, Webhooks, Scope3, DelegatedPlatformContext)> {
    let (c, w) = Control::with_webhooks(s).await?;
    let scope = pa_env(&c, Uuid::new_v4(), Uuid::new_v4(), "SANDBOX");
    let d = c.pa.machine(scope.2, ALL);
    Some((c, w, scope, d))
}

// ---------------------------------------------------------------------------
// Subscriptions.
// ---------------------------------------------------------------------------

/// CRUD with Calling's contract and error codes; the secret is shown once and stored
/// only sealed; every change is versioned and audited without secret or URL.
#[tokio::test]
async fn machine_crud_shows_the_secret_once_and_stores_it_only_sealed() {
    let Some((c, _w, scope, d)) = world(settings()).await else {
        return;
    };
    let (id, secret) = subscribe(
        &c,
        &d,
        "http://127.0.0.1:9/hook",
        &["call.ended", "call.requested"],
    )
    .await;
    assert!(secret.starts_with("whsec_") && secret.len() == 49);
    // Calling's own customer schema accepts the Developer Platform's answer.
    let got = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{id}"),
            None,
        )
        .await;
    assert_eq!(got.status, StatusCode::OK);
    conforms(&format!("{CALLING_WH}Webhook"), &got.json);
    conforms(&format!("{WH}Webhook"), &got.json);
    assert_eq!(got.headers.get("cache-control").unwrap(), "no-store");
    assert!(!got.json.to_string().contains(&secret));
    assert_eq!(
        got.json["eventTypes"],
        json!(["call.ended", "call.requested"])
    );
    // At rest: only a `v2.<kid>.` seal bound to the subscription, never the secret.
    let sealed = one(
        &c,
        &format!("SELECT secret_sealed FROM devplat_webhook_subscriptions WHERE id = '{id}'"),
    )
    .await;
    assert!(sealed.starts_with("v2.k0.") && !sealed.contains(&secret[6..]));
    let dump = one(
        &c,
        "SELECT string_agg(t::text, '|') FROM devplat_webhook_subscriptions t",
    )
    .await;
    assert!(!dump.contains(&secret) && !dump.contains(&secret[6..]));
    assert_eq!(
        one(&c, &format!("SELECT environment_id::text || ':' || environment_kind || ':' || origin FROM devplat_webhook_subscriptions WHERE id = '{id}'")).await,
        format!("{}:SANDBOX:NATIVE", scope.2),
        "scope from the confirmed chain"
    );
    // List with cursor pagination.
    let (id2, _) = subscribe(&c, &d, "http://127.0.0.1:9/two", &["call.ended"]).await;
    let p1 = c
        .req(As::Machine(&d), Method::GET, "/v1/webhooks?limit=1", None)
        .await;
    conforms(&format!("{CALLING_WH}WebhookList"), &p1.json);
    assert_eq!(p1.json["data"][0]["webhookId"], json!(id));
    let cursor = p1.json["nextCursor"].as_str().unwrap();
    let p2 = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks?limit=1&cursor={cursor}"),
            None,
        )
        .await;
    assert_eq!(p2.json["data"][0]["webhookId"], json!(id2));
    assert!(p2.json["nextCursor"].is_null());
    for bad in [
        "/v1/webhooks?limit=0",
        "/v1/webhooks?cursor=%25%25",
        "/v1/webhooks?tenantId=x",
    ] {
        let r = c.req(As::Machine(&d), Method::GET, bad, None).await;
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{bad}");
        conforms(ERR, &r.json);
    }
    assert_eq!(
        c.req(
            As::Machine(&d),
            Method::GET,
            "/v1/webhooks/not-a-uuid",
            None
        )
        .await
        .json["error"]["code"],
        "NOT_FOUND"
    );
    // Optimistic concurrency.
    let up = c
        .req(
            As::Machine(&d),
            Method::PATCH,
            &format!("/v1/webhooks/{id}"),
            Some(json!({ "expectedVersion": 1, "description": null, "status": "DISABLED" })),
        )
        .await;
    assert_eq!(up.status, StatusCode::OK, "{}", up.json);
    assert_eq!(
        (up.json["version"].clone(), up.json["status"].clone()),
        (json!(2), json!("DISABLED"))
    );
    assert!(up.json["description"].is_null());
    let stale = c
        .req(
            As::Machine(&d),
            Method::PATCH,
            &format!("/v1/webhooks/{id}"),
            Some(json!({ "expectedVersion": 1, "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(
        (stale.status, stale.json["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("VERSION_CONFLICT"))
    );
    let empty = c
        .req(
            As::Machine(&d),
            Method::PATCH,
            &format!("/v1/webhooks/{id}"),
            Some(json!({ "expectedVersion": 2 })),
        )
        .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    // Rotation: a new secret, once; a new seal.
    let rot = c
        .req(
            As::Machine(&d),
            Method::POST,
            &format!("/v1/webhooks/{id}/rotate-secret"),
            Some(json!({ "expectedVersion": 2 })),
        )
        .await;
    assert_eq!(rot.status, StatusCode::OK, "{}", rot.json);
    conforms(&format!("{CALLING_WH}WebhookWithSecret"), &rot.json);
    let rotated = rot.json["signingSecret"].as_str().unwrap();
    assert_ne!(rotated, secret);
    assert_eq!(rot.json["webhook"]["secretVersion"], 2);
    assert_ne!(
        one(
            &c,
            &format!("SELECT secret_sealed FROM devplat_webhook_subscriptions WHERE id = '{id}'")
        )
        .await,
        sealed
    );
    // Soft delete.
    let missing = c
        .req(
            As::Machine(&d),
            Method::DELETE,
            &format!("/v1/webhooks/{id}"),
            None,
        )
        .await;
    assert_eq!(missing.json["error"]["code"], "VALIDATION_ERROR");
    let del = c
        .req(
            As::Machine(&d),
            Method::DELETE,
            &format!("/v1/webhooks/{id}?expectedVersion=3"),
            None,
        )
        .await;
    assert_eq!(
        (del.status, del.json["status"].clone()),
        (StatusCode::OK, json!("DELETED"))
    );
    assert_eq!(
        c.req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{id}"),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_webhook_subscriptions WHERE id = '{id}'"
        ))
        .await,
        1,
        "history kept"
    );
    // Audit: one row per change, by the service account; never the URL or a secret.
    let audit = one(&c, &format!(
        "SELECT string_agg(action, ',' ORDER BY seq) FROM devplat_activity_events WHERE tenant_id = '{}' AND target_type = 'WEBHOOK'", scope.0)).await;
    assert_eq!(
        audit,
        "webhook.created,webhook.created,webhook.updated,webhook.secret_rotated,webhook.deleted"
    );
    let summaries = one(
        &c,
        "SELECT string_agg(summary::text || actor_ref, '|') FROM devplat_activity_events",
    )
    .await;
    assert!(!summaries.contains("127.0.0.1") && !summaries.contains("whsec_"));
    assert!(summaries.contains(&format!(
        "service_account:{}",
        d.initiating_service_account_id()
    )));
}

/// `webhooks.read` reads, `webhooks.manage` changes, and every subscribed event type needs
/// its own read scope; humans use the console; no key ring = 503; flag off = 404 (ingest: 503, retry later).
#[tokio::test]
async fn scopes_are_enforced_per_operation_and_event_type() {
    let Some((c, _w, scope, manager)) = world(settings()).await else {
        return;
    };
    let reader = c.pa.machine(scope.2, &["webhooks.read"]);
    let manage_only = c.pa.machine(scope.2, &["webhooks.manage"]);
    let body = |types: &[&str]| Some(json!({ "url": "http://127.0.0.1:9/h", "eventTypes": types }));
    assert_eq!(
        c.req(As::Machine(&reader), Method::GET, "/v1/webhooks", None)
            .await
            .status,
        StatusCode::OK
    );
    let r = c
        .req(
            As::Machine(&reader),
            Method::POST,
            "/v1/webhooks",
            body(&["meeting.started"]),
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].clone()),
        (StatusCode::FORBIDDEN, json!("INSUFFICIENT_SCOPE"))
    );
    conforms(ERR, &r.json);
    let r = c
        .req(As::Machine(&manage_only), Method::GET, "/v1/webhooks", None)
        .await;
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    // Domain read scope: call.* needs calls.read.
    let r = c
        .req(
            As::Machine(&manage_only),
            Method::POST,
            "/v1/webhooks",
            body(&["call.ended"]),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    // Meeting events are read with `meetings.read`.
    let r = c
        .req(
            As::Machine(&manage_only),
            Method::POST,
            "/v1/webhooks",
            body(&["meeting.started"]),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    let manage_meetings = c.pa.machine(scope.2, &["webhooks.manage", "meetings.read"]);
    let ok = c
        .req(
            As::Machine(&manage_meetings),
            Method::POST,
            "/v1/webhooks",
            body(&["meeting.started", "meeting.created"]),
        )
        .await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.json);
    let id = ok.json["webhook"]["webhookId"].as_str().unwrap();
    let r = c
        .req(
            As::Machine(&manage_meetings),
            Method::PATCH,
            &format!("/v1/webhooks/{id}"),
            Some(json!({ "expectedVersion": 1, "eventTypes": ["number.assignment_changed"] })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    let r = c
        .req(
            As::Machine(&manager),
            Method::PATCH,
            &format!("/v1/webhooks/{id}"),
            Some(json!({ "expectedVersion": 1, "eventTypes": ["number.assignment_changed"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    // Unknown, wildcard and internal types; scope fields in the body.
    for b in [
        body(&["call.everything"]),
        body(&["*"]),
        body(&["calling.call-state-changed"]),
        Some(
            json!({ "url": "http://127.0.0.1:9/h", "eventTypes": ["call.ended"], "tenantId": scope.0 }),
        ),
        body(&[]),
    ] {
        let r = c
            .req(
                As::Machine(&manager),
                Method::POST,
                "/v1/webhooks",
                b.clone(),
            )
            .await;
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{b:?}");
    }
    // A human session is not a machine credential.
    let (token, _) = c.sessions.sign_in("dev@acme.example");
    let r = c
        .req(As::User(&token), Method::GET, "/v1/webhooks", None)
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    // Without a sealing key: fail closed.
    let mut no_key = settings();
    no_key.key_ring = None;
    let Some((c2, _w2, _s2, d2)) = world(no_key).await else {
        return;
    };
    let r = c2
        .req(
            As::Machine(&d2),
            Method::POST,
            "/v1/webhooks",
            body(&["meeting.started"]),
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].clone()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!("CAPABILITY_UNAVAILABLE")
        )
    );
    conforms(ERR, &r.json);
    // Flag off: every webhook route is 404.
    let Some((off, off_w)) = Control::with_webhooks_disabled(settings()).await else {
        return;
    };
    let s3 = pa_env(&off, Uuid::new_v4(), Uuid::new_v4(), "SANDBOX");
    let d3 = off.pa.machine(s3.2, ALL);
    assert_eq!(
        off.req(As::Machine(&d3), Method::GET, "/v1/webhooks", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Ingest while webhooks are off: retry later (503), so producers keep the event in
    // their outbox rather than dead-lettering it; callers without the permission are
    // still refused first.
    let r = publish(&off, &off_w.producers.calling, &call_requested(s3)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.json);
    let r = off
        .internal(
            &off.operator,
            Method::POST,
            "/internal/v1/platform/events",
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

/// A sandbox subscription never receives a production event (and vice versa); another
/// tenant's (or environment's) ids are 404 without an existence hint.
#[tokio::test]
async fn environments_and_tenants_are_isolated() {
    let Some((c, w, sandbox, d_sbx)) = world(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let production = pa_env(&c, sandbox.0, sandbox.1, "PRODUCTION");
    let d_prod = c.pa.machine(production.2, ALL);
    let other = pa_env(&c, Uuid::new_v4(), Uuid::new_v4(), "SANDBOX");
    let d_other = c.pa.machine(other.2, ALL);
    let (sub_sbx, _) = subscribe(&c, &d_sbx, &rx.url, &["call.requested"]).await;
    let (sub_prod, _) = subscribe(&c, &d_prod, &rx.url, &["call.requested"]).await;
    assert_eq!(
        one(&c, &format!("SELECT environment_kind FROM devplat_webhook_subscriptions WHERE id = '{sub_prod}'")).await,
        "PRODUCTION"
    );
    let p = &w.producers.calling;
    let r = publish(&c, p, &call_requested(production)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let prod_event = r.json["eventId"].as_str().unwrap().to_string();
    let r = publish(&c, p, &call_requested(sandbox)).await;
    let sbx_event = r.json["eventId"].as_str().unwrap().to_string();
    publish(&c, p, &call_requested(other)).await;
    assert_eq!(
        one(&c, "SELECT string_agg(s.environment_kind || '=' || d.event_id, ',' ORDER BY s.environment_kind) \
                  FROM devplat_webhook_deliveries d JOIN devplat_webhook_subscriptions s ON s.id = d.subscription_id").await,
        format!("PRODUCTION={prod_event},SANDBOX={sbx_event}")
    );
    // Cross-environment and cross-tenant reads, changes and replays are 404.
    let sbx_delivery = one(
        &c,
        &format!(
            "SELECT id::text FROM devplat_webhook_deliveries WHERE subscription_id = '{sub_sbx}'"
        ),
    )
    .await;
    for d in [&d_prod, &d_other] {
        for (m, path, body) in [
            (Method::GET, format!("/v1/webhooks/{sub_sbx}"), None),
            (
                Method::GET,
                format!("/v1/webhooks/{sub_sbx}/deliveries"),
                None,
            ),
            (
                Method::PATCH,
                format!("/v1/webhooks/{sub_sbx}"),
                Some(json!({ "expectedVersion": 1, "status": "DISABLED" })),
            ),
            (
                Method::DELETE,
                format!("/v1/webhooks/{sub_sbx}?expectedVersion=1"),
                None,
            ),
            (
                Method::POST,
                format!("/v1/webhooks/{sub_sbx}/rotate-secret"),
                Some(json!({ "expectedVersion": 1 })),
            ),
            (
                Method::POST,
                format!("/v1/webhook-deliveries/{sbx_delivery}/replay"),
                None,
            ),
        ] {
            let r = c.req(As::Machine(d), m.clone(), &path, body).await;
            assert_eq!(
                (r.status, r.json["error"]["code"].clone()),
                (StatusCode::NOT_FOUND, json!("NOT_FOUND")),
                "{m} {path}"
            );
        }
    }
    let list = c
        .req(As::Machine(&d_other), Method::GET, "/v1/webhooks", None)
        .await;
    assert_eq!(list.json["data"], json!([]));
    // Delivered to the right endpoints only.
    w.service.deliver_due_once(100).await.unwrap();
    let events: Vec<String> = rx
        .seen()
        .iter()
        .map(|r| header(r, signing::HEADER_EVENT_ID))
        .collect();
    assert_eq!(events.len(), 2);
    assert!(events.contains(&prod_event) && events.contains(&sbx_event));
}

// ---------------------------------------------------------------------------
// Ingestion.
// ---------------------------------------------------------------------------

/// Dedupe on (producer, source event id), producer binding, producer families, the
/// receiver's runtime environment, and the safe projection.
#[tokio::test]
async fn ingestion_dedupes_refuses_spoofing_and_foreign_families_and_projects_safely() {
    let Some((c, w, scope, d)) = world(settings()).await else {
        return;
    };
    subscribe(
        &c,
        &d,
        "http://127.0.0.1:9/h",
        &[
            "call.requested",
            "number.assignment_changed",
            "meeting.started",
        ],
    )
    .await;
    let p = &w.producers;
    let env = call_requested(scope);
    let first = publish(&c, &p.calling, &env).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json);
    conforms(&format!("{WH}PublishEventResponse"), &first.json);
    assert_eq!(first.json["duplicate"], false);
    let again = publish(&c, &p.calling, &env).await;
    assert_eq!(
        again.json["eventId"], first.json["eventId"],
        "same event id"
    );
    assert_eq!(again.json["duplicate"], true);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_events").await,
        1
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        1
    );
    // The public view: allow-listed keys and safe values only.
    let payload = one(&c, "SELECT payload::text FROM devplat_webhook_events").await;
    let body: Value = serde_json::from_str(&payload).unwrap();
    conforms(&format!("{WH}WebhookEvent"), &body);
    conforms(&format!("{CALLING_WH}WebhookEvent"), &body);
    assert_eq!(body["type"], "call.requested");
    assert_eq!(body["data"]["reason"], "OUTBOUND");
    for leaked in ["+1555", "CA123", "sip:", "caller_number"] {
        assert!(!payload.contains(leaked), "{leaked} leaked: {payload}");
    }
    // Spoofing: Calling publishing as Messaging, Messaging publishing Calling's events.
    let mut spoof = call_requested(scope);
    spoof["producer_service"] = json!("cfmeet.messaging");
    let r = publish(&c, &p.calling, &spoof).await;
    assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    let r = publish(&c, &p.messaging, &spoof).await;
    assert_eq!(
        r.json["error"]["code"], "VALIDATION_ERROR",
        "unknown family for Messaging"
    );
    // A service without developer-event.publish; another runtime environment; garbage.
    let r = publish(&c, &c.peers.gateway, &call_requested(scope)).await;
    assert_eq!(r.json["error"]["code"], "INTERNAL_PERMISSION_DENIED");
    let mut other_env = call_requested(scope);
    other_env["runtime_environment"] = json!("production");
    assert_eq!(
        publish(&c, &p.calling, &other_env).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        publish(&c, &p.calling, &json!({ "hello": 1 })).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_events").await,
        1,
        "nothing refused was stored"
    );
    // An internal-only fact of the producer's own domain: acknowledged, never public.
    let internal = envelope(
        "cfmeet.calling",
        "calling.media-credential-issued",
        scope,
        json!({ "event_type": "CALL_MEDIA_CREDENTIAL_ISSUED" }),
    );
    let r = publish(&c, &p.calling, &internal).await;
    assert_eq!(
        (r.status, r.json["eventId"].clone()),
        (StatusCode::OK, Value::Null)
    );
    // Numbering and the Meeting Service: their families, projected without numbers / URLs.
    let number = envelope(
        "cfmeet.numbering",
        "number.assignment.changed",
        scope,
        json!({
        "assignment_id": Uuid::new_v4(), "number_resource_id": Uuid::new_v4(), "to_status": "ACTIVE",
        "from_status": "PENDING", "revision": 2, "e164": "+15550100123", "provider_ref": "prov-77" }),
    );
    let r = publish(&c, &p.numbering, &number).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let meeting = envelope(
        "cfmeet.meeting-service",
        "meeting-service.meeting.started",
        scope,
        json!({
        "meeting_id": Uuid::new_v4(), "participant_count": 3, "title": "Board meeting",
        "join_url": "https://meet.example/abc", "host_email": "ceo@acme.example" }),
    );
    let r = publish(&c, &p.meeting, &meeting).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    // Out of order and replayed: the meeting event again, then the number event again.
    publish(&c, &p.meeting, &meeting).await;
    publish(&c, &p.numbering, &number).await;
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_events").await,
        3
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        3
    );
    let all = one(
        &c,
        "SELECT string_agg(payload::text, '|') FROM devplat_webhook_events",
    )
    .await;
    for leaked in ["+1555", "prov-77", "Board", "https://", "ceo@"] {
        assert!(!all.contains(leaked), "{leaked} leaked");
    }
    for row in all.split('|') {
        conforms(
            &format!("{WH}WebhookEvent"),
            &serde_json::from_str::<Value>(row).unwrap(),
        );
    }
    // An event type nobody subscribed to is recorded (dedupe) but fans out to no one.
    let sms = envelope(
        "cfmeet.sms-gateway",
        "sms-gateway.message.received",
        scope,
        json!({
        "message_id": Uuid::new_v4(), "status": "RECEIVED", "content": "hi" }),
    );
    let r = publish(&c, &p.sms, &sms).await;
    assert!(r.json["eventId"].is_string());
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        3
    );
}

// ---------------------------------------------------------------------------
// Delivery.
// ---------------------------------------------------------------------------

/// Signed exactly as `cf_webhooks::signing::verify` accepts; 5xx / 429 / timeouts retry
/// with backoff until success; the attempt history records status classes only.
#[tokio::test]
async fn conflicting_replays_are_refused_recorded_and_surface_in_ingestion_health() {
    let Some((c, w, scope, d)) = world(settings()).await else {
        return;
    };
    subscribe(&c, &d, "http://127.0.0.1:9/h", &["call.requested"]).await;
    let p = &w.producers;
    let healthy = c
        .operator(Method::GET, "/internal/v1/operator/ingestion/health", None)
        .await;
    assert_eq!(healthy.status, StatusCode::OK, "{}", healthy.json);
    assert_eq!(healthy.json["status"], "OK", "{}", healthy.json);
    let env = call_requested(scope);
    let first = publish(&c, &p.calling, &env).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json);
    // The same source event saying something else: another payload, time or scope.
    let mut other_payload = env.clone();
    other_payload["payload"]["data"]["reason"] = json!("INBOUND");
    let mut other_time = env.clone();
    other_time["occurred_at"] = json!("2026-10-23T10:00:01Z");
    let mut other_scope = env.clone();
    let foreign = pa_env(&c, scope.0, scope.1, "SANDBOX");
    other_scope["ownership"]["application_environment_id"] = json!(foreign.2);
    for conflict in [&other_payload, &other_time, &other_scope] {
        let r = publish(&c, &p.calling, conflict).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
        assert_eq!(r.json["error"]["code"], "CONFLICTING_REPLAY");
    }
    // The original is kept; an identical replay is still the same duplicate.
    let again = publish(&c, &p.calling, &env).await;
    assert_eq!(again.json["eventId"], first.json["eventId"]);
    assert_eq!(again.json["duplicate"], true);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_events").await,
        1
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        1
    );
    let payload = one(&c, "SELECT payload::text FROM devplat_webhook_events").await;
    assert!(
        payload.contains("OUTBOUND") && !payload.contains("INBOUND"),
        "{payload}"
    );
    // Recorded for operators (producer, source id, reason — no payload), append-only.
    assert_eq!(
        c.count(
            "SELECT count(*) FROM devplat_event_rejections WHERE producer = 'cfmeet.calling' \
                 AND reason_code = 'CONFLICTING_REPLAY'"
        )
        .await,
        3
    );
    let cols = one(
        &c,
        "SELECT string_agg(column_name, ',' ORDER BY column_name) FROM information_schema.columns \
         WHERE table_name = 'devplat_event_rejections'",
    )
    .await;
    assert_eq!(cols, "id,producer,reason_code,received_at,source_event_id");
    for sql in [
        "UPDATE devplat_event_rejections SET reason_code = 'X_Y_Z'",
        "DELETE FROM devplat_event_rejections",
    ] {
        assert!(c.db.conn.execute_unprepared(sql).await.is_err(), "{sql}");
    }
    let h = c
        .operator(Method::GET, "/internal/v1/operator/ingestion/health", None)
        .await;
    assert_eq!(h.json["status"], "DEGRADED", "{}", h.json);
    assert_eq!(
        h.json["eventRejectionsLast24h"],
        json!([{ "producer": "cfmeet.calling", "count": 3 }])
    );
    // Operator-only.
    let customer = c
        .internal(
            &p.calling,
            Method::GET,
            "/internal/v1/operator/ingestion/health",
            None,
        )
        .await;
    assert_eq!(customer.status, StatusCode::FORBIDDEN, "{}", customer.json);
}

#[tokio::test]
async fn deliveries_are_signed_and_retried_until_success() {
    let Some((c, w, scope, d)) = world(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (sub, secret) = subscribe(&c, &d, &rx.url, &["call.requested"]).await;
    let event = publish(&c, &w.producers.calling, &call_requested(scope))
        .await
        .json["eventId"]
        .as_str()
        .unwrap()
        .to_string();
    let delivery = one(&c, "SELECT id::text FROM devplat_webhook_deliveries").await;
    rx.script(&[500, 429]);
    let r = w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(r.retried, 1, "{r:?}");
    assert_eq!(
        delivery_state(&c, &delivery).await,
        "RETRY_SCHEDULED:1:HTTP_5XX:500"
    );
    assert_eq!(
        w.service.deliver_due_once(100).await.unwrap().retried,
        0,
        "not due yet"
    );
    w.advance(7 * 3600);
    w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(
        delivery_state(&c, &delivery).await,
        "RETRY_SCHEDULED:2:HTTP_RATE_LIMITED:429"
    );
    w.advance(7 * 3600);
    assert_eq!(w.service.deliver_due_once(100).await.unwrap().succeeded, 1);
    assert_eq!(delivery_state(&c, &delivery).await, "SUCCEEDED:3::200");
    assert_eq!(
        one(&c, &format!("SELECT string_agg(outcome || ':' || COALESCE(http_status::text, '') || ':' || COALESCE(error_class, ''), ',' ORDER BY attempt_number) FROM devplat_webhook_attempts WHERE delivery_id = '{delivery}'")).await,
        "RETRY:500:HTTP_5XX,RETRY:429:HTTP_RATE_LIMITED,SUCCEEDED:200:"
    );
    // Every request: the delivery id, the stable event id, a fresh timestamp, a valid v1
    // signature over the exact body, which is the stored public event.
    let seen = rx.seen();
    assert_eq!(seen.len(), 3);
    let stored: Value =
        serde_json::from_str(&one(&c, "SELECT payload::text FROM devplat_webhook_events").await)
            .unwrap();
    let now = chrono::Utc::now().timestamp() + w.offset.load(std::sync::atomic::Ordering::SeqCst);
    for r in &seen {
        assert_eq!(header(r, signing::HEADER_DELIVERY_ID), delivery);
        assert_eq!(header(r, signing::HEADER_EVENT_ID), event);
        assert_eq!(header(r, "content-type"), "application/json");
        signing::verify(
            secret.as_bytes(),
            &header(r, signing::HEADER_SIGNATURE),
            &header(r, signing::HEADER_TIMESTAMP),
            &delivery,
            &r.body,
            now,
            8 * 3600 * 3,
        )
        .expect("signature verifies");
        assert!(signing::verify(
            b"whsec_wrong",
            &header(r, signing::HEADER_SIGNATURE),
            &header(r, signing::HEADER_TIMESTAMP),
            &delivery,
            &r.body,
            now,
            86_400
        )
        .is_err());
        assert_eq!(serde_json::from_slice::<Value>(&r.body).unwrap(), stored);
    }
    let last = seen.last().unwrap();
    assert!(signing::verify(
        secret.as_bytes(),
        &header(last, signing::HEADER_SIGNATURE),
        &header(last, signing::HEADER_TIMESTAMP),
        &delivery,
        &last.body,
        now,
        signing::DEFAULT_TOLERANCE_SECONDS
    )
    .is_ok());
    // Health: the success reset the failure streak.
    assert_eq!(
        one(&c, &format!("SELECT consecutive_failures::text || ':' || (last_success_at IS NOT NULL)::text FROM devplat_webhook_subscriptions WHERE id = '{sub}'")).await,
        "0:true"
    );
    // A receiver slower than the request timeout: TIMEOUT, retried, then delivered.
    rx.delay(2500);
    publish(&c, &w.producers.calling, &call_requested(scope)).await;
    w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE status = 'RETRY_SCHEDULED' AND failure_category = 'TIMEOUT'").await, 1);
    rx.delay(0);
    w.advance(7 * 3600);
    w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE status = 'SUCCEEDED'")
            .await,
        2
    );
    // The history through the customer API (Calling's delivery shape).
    let h = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{sub}/deliveries?status=SUCCEEDED"),
            None,
        )
        .await;
    conforms(&format!("{CALLING_WH}WebhookDeliveryList"), &h.json);
    assert_eq!(h.json["data"].as_array().unwrap().len(), 2);
    let bad = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{sub}/deliveries?status=LOST"),
            None,
        )
        .await;
    assert_eq!(bad.json["error"]["code"], "VALIDATION_ERROR");
}

/// A permanent 4xx fails at once; exhausted retries dead-letter; a manual replay is a NEW
/// delivery of the SAME event (dead letters included); a redirect is never followed.
#[tokio::test]
async fn permanent_failures_and_exhaustion_dead_letter_and_replays_keep_the_event_id() {
    let Some((c, w, scope, d)) = world(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (sub, secret) = subscribe(&c, &d, &rx.url, &["call.requested"]).await;
    let p = &w.producers.calling;
    // 4xx: permanent.
    rx.script(&[400]);
    publish(&c, p, &call_requested(scope)).await;
    w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE status = 'FAILED' AND failure_category = 'HTTP_4XX' AND attempt_count = 1 AND completed_at IS NOT NULL").await, 1);
    // 3xx: refused, the redirect target is never contacted.
    rx.script(&[302]);
    publish(&c, p, &call_requested(scope)).await;
    w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE failure_category = 'HTTP_REDIRECT_REFUSED' AND status = 'FAILED'").await, 1);
    assert_eq!(rx.seen().len(), 2, "one request each, no redirect followed");
    // Exhaustion after max_attempts (3).
    rx.script(&[500, 500, 500]);
    let ev = publish(&c, p, &call_requested(scope)).await.json["eventId"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..4 {
        w.service.deliver_due_once(100).await.unwrap();
        w.advance(7 * 3600);
    }
    let dead = one(
        &c,
        &format!("SELECT id::text FROM devplat_webhook_deliveries WHERE event_id = '{ev}'"),
    )
    .await;
    assert_eq!(
        delivery_state(&c, &dead).await,
        "FAILED:3:HTTP_5XX:500",
        "dead letter"
    );
    let failed = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{sub}/deliveries?status=FAILED"),
            None,
        )
        .await;
    assert_eq!(failed.json["data"].as_array().unwrap().len(), 3);
    // Replay the dead letter: new delivery id, same event id, delivered.
    let r = c
        .req(
            As::Machine(&d),
            Method::POST,
            &format!("/v1/webhook-deliveries/{dead}/replay"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{CALLING_WH}WebhookDelivery"), &r.json);
    let replay = r.json["deliveryId"].as_str().unwrap().to_string();
    assert_ne!(replay, dead);
    assert_eq!(
        (
            r.json["replayOf"].clone(),
            r.json["eventId"].clone(),
            r.json["status"].clone()
        ),
        (json!(dead), json!(ev), json!("PENDING"))
    );
    let before = rx.seen().len();
    assert_eq!(w.service.deliver_due_once(100).await.unwrap().succeeded, 1);
    let got = rx.seen()[before].clone();
    assert_eq!(header(&got, signing::HEADER_EVENT_ID), ev);
    assert_eq!(header(&got, signing::HEADER_DELIVERY_ID), replay);
    let now = chrono::Utc::now().timestamp() + w.offset.load(std::sync::atomic::Ordering::SeqCst);
    signing::verify(
        secret.as_bytes(),
        &header(&got, signing::HEADER_SIGNATURE),
        &header(&got, signing::HEADER_TIMESTAMP),
        &replay,
        &got.body,
        now,
        300,
    )
    .unwrap();
    assert_eq!(
        delivery_state(&c, &dead).await,
        "FAILED:3:HTTP_5XX:500",
        "the original is untouched"
    );
    // Unknown delivery, or a deleted subscription: 404.
    let r = c
        .req(
            As::Machine(&d),
            Method::POST,
            &format!("/v1/webhook-deliveries/{}/replay", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let v = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{sub}"),
            None,
        )
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    c.req(
        As::Machine(&d),
        Method::DELETE,
        &format!("/v1/webhooks/{sub}?expectedVersion={v}"),
        None,
    )
    .await;
    let r = c
        .req(
            As::Machine(&d),
            Method::POST,
            &format!("/v1/webhook-deliveries/{dead}/replay"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// Endpoint health moves with every attempt; after `auto_disable_after` consecutive
/// failures the endpoint is disabled (open deliveries are then cancelled, not sent);
/// re-enabling resets the health.
#[tokio::test]
async fn endpoint_health_drives_automatic_disablement() {
    let mut s = settings();
    s.auto_disable_after = 5;
    s.max_attempts = 3;
    let Some((c, w, scope, d)) = world(s).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (sub, _) = subscribe(&c, &d, &rx.url, &["call.requested"]).await;
    let health = || async {
        one(&c, &format!("SELECT status || ':' || consecutive_failures || ':' || COALESCE(disabled_reason, '') FROM devplat_webhook_subscriptions WHERE id = '{sub}'")).await
    };
    rx.script(&[500; 20]);
    for _ in 0..3 {
        publish(&c, &w.producers.calling, &call_requested(scope)).await;
    }
    w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(health().await, "ACTIVE:3:");
    w.advance(7 * 3600);
    let r = w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(r.disabled, 1, "{r:?}");
    assert_eq!(health().await, "DISABLED:5:CONSECUTIVE_FAILURES");
    assert_eq!(rx.seen().len(), 5, "nothing sent after the disablement");
    // The remaining open delivery is cancelled when claimed, never sent.
    w.advance(7 * 3600);
    let r = w.service.deliver_due_once(100).await.unwrap();
    assert!(r.cancelled >= 1, "{r:?}");
    assert_eq!(rx.seen().len(), 5);
    assert_eq!(c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE status NOT IN ('CANCELLED', 'RETRY_SCHEDULED', 'FAILED')").await, 0);
    let view = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{sub}"),
            None,
        )
        .await;
    assert_eq!(view.json["status"], "DISABLED");
    // Re-enabling resets the health.
    let v = view.json["version"].as_i64().unwrap();
    let r = c
        .req(
            As::Machine(&d),
            Method::PATCH,
            &format!("/v1/webhooks/{sub}"),
            Some(json!({ "expectedVersion": v, "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(health().await, "ACTIVE:0:");
}

/// SSRF: private, loopback, metadata and plain-http endpoints are refused outside the
/// local runtime; a name re-binding to an internal address is never contacted.
#[tokio::test]
async fn ssrf_targets_are_refused_outside_the_local_runtime() {
    let mut strict = settings();
    strict.policy = EndpointPolicy::STRICT;
    let Some((c, _w, _scope, d)) = world(strict).await else {
        return;
    };
    for url in [
        "http://hooks.example.com/h",
        "https://127.0.0.1/h",
        "https://10.1.2.3/h",
        "https://192.168.0.10/h",
        "https://169.254.169.254/latest/meta-data",
        "https://metadata.google.internal/x",
        "https://localhost/h",
        "https://[::1]/h",
        "https://user:pass@hooks.example.com/h",
        "ftp://hooks.example.com/h",
    ] {
        let r = c
            .req(
                As::Machine(&d),
                Method::POST,
                "/v1/webhooks",
                Some(json!({ "url": url, "eventTypes": ["call.ended"] })),
            )
            .await;
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{url}");
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_subscriptions")
            .await,
        0
    );
    let ok = c
        .req(
            As::Machine(&d),
            Method::POST,
            "/v1/webhooks",
            Some(
                json!({ "url": "https://hooks.example.com/cfmeet", "eventTypes": ["call.ended"] }),
            ),
        )
        .await;
    assert_eq!(ok.status, StatusCode::CREATED);
    // DNS rebinding: the name resolves to loopback at connection time.
    let Some((c2, w2, scope2, d2)) = world(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    subscribe(
        &c2,
        &d2,
        &format!("http://rebind.example.com:{}/hook", rx.port),
        &["call.requested"],
    )
    .await;
    publish(&c2, &w2.producers.calling, &call_requested(scope2)).await;
    let policy = EndpointPolicy {
        allow_insecure_http: true,
        allow_private_networks: false,
    };
    let rebinding: cf_webhooks::endpoint::Lookup = Arc::new(|_host: String| {
        Box::pin(async { Ok(vec![std::net::SocketAddr::from(([127, 0, 0, 1], 0))]) })
    });
    let mut s = settings();
    s.policy = policy;
    let svc =
        WebhookService::new(c2.db.conn.clone(), s).with_client(cf_webhooks::endpoint::client_with(
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
        one(
            &c2,
            "SELECT failure_category FROM devplat_webhook_deliveries"
        )
        .await,
        "ENDPOINT_BLOCKED"
    );
}

/// Two workers never claim the same delivery; a worker that crashes after its claim
/// loses the lease and the delivery is reclaimed and delivered once.
#[tokio::test]
async fn concurrent_workers_and_crash_recovery_deliver_each_once() {
    let Some((c, w, scope, d)) = world(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    subscribe(&c, &d, &rx.url, &["call.requested"]).await;
    for _ in 0..6 {
        publish(&c, &w.producers.calling, &call_requested(scope)).await;
    }
    let total = c
        .count("SELECT count(*) FROM devplat_webhook_deliveries")
        .await as usize;
    assert_eq!(total, 6);
    let other = w.service.clone();
    let (a, b) = tokio::join!(w.service.claim_due(100), other.claim_due(100));
    let (a, b) = (a.unwrap(), b.unwrap());
    let ids: std::collections::BTreeSet<Uuid> = a.iter().map(|x| x.id).collect();
    assert!(b.iter().all(|x| !ids.contains(&x.id)), "disjoint claims");
    assert_eq!(a.len() + b.len(), total);
    // Both "workers" crash before sending: nothing is sent, nothing is claimable.
    assert_eq!(
        w.service.deliver_due_once(100).await.unwrap(),
        Default::default()
    );
    assert!(rx.seen().is_empty());
    // The leases (timeout + 30 s) expire: reclaimed, delivered exactly once each by two
    // concurrent workers.
    w.advance(40);
    let (x, y) = tokio::join!(w.service.deliver_due_once(100), other.deliver_due_once(100));
    assert_eq!(x.unwrap().succeeded + y.unwrap().succeeded, total);
    let seen = rx.seen();
    assert_eq!(seen.len(), total);
    let distinct: std::collections::BTreeSet<String> = seen
        .iter()
        .map(|r| header(r, signing::HEADER_DELIVERY_ID))
        .collect();
    assert_eq!(distinct.len(), total, "each delivery sent once");
    assert_eq!(c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE status <> 'SUCCEEDED' OR attempt_count <> 2").await, 0);
    // A claim that crashed at the attempt cap is dead-lettered, not reclaimed forever.
    publish(&c, &w.producers.calling, &call_requested(scope)).await;
    c.db.conn
        .execute_unprepared(
            "UPDATE devplat_webhook_deliveries SET status = 'DELIVERING', attempt_count = 3, \
           lease_until = now() - interval '1 hour' WHERE status = 'PENDING'",
        )
        .await
        .unwrap();
    let r = w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(r.failed, 1, "{r:?}");
    assert_eq!(c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE failure_category = 'LEASE_EXPIRED_AT_CAP'").await, 1);
}

/// Shadow mode: events are ingested and projected, deliveries recorded SHADOW, nothing is
/// sent — and switching to active later never sends the shadow backlog.
#[tokio::test]
async fn shadow_mode_records_deliveries_and_sends_nothing() {
    let mut s = settings();
    s.mode = DeliveryMode::Shadow;
    let Some((c, w, scope, d)) = world(s).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (sub, _) = subscribe(&c, &d, &rx.url, &["call.requested"]).await;
    let r = publish(&c, &w.producers.calling, &call_requested(scope)).await;
    assert!(r.json["eventId"].is_string());
    let delivery = one(&c, "SELECT id::text FROM devplat_webhook_deliveries").await;
    assert_eq!(delivery_state(&c, &delivery).await, "SHADOW:0::");
    let rep = w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(rep.succeeded + rep.retried + rep.failed, 0);
    // A replay is recorded too, and a delivery left pending is shadowed by the worker.
    let r = c
        .req(
            As::Machine(&d),
            Method::POST,
            &format!("/v1/webhook-deliveries/{delivery}/replay"),
            None,
        )
        .await;
    assert_eq!(
        (r.status, r.json["status"].clone()),
        (StatusCode::CREATED, json!("SHADOW"))
    );
    conforms(&format!("{WH}WebhookDelivery"), &r.json);
    c.db.conn.execute_unprepared(&format!(
        "INSERT INTO devplat_webhook_deliveries (id, subscription_id, event_id, tenant_id, application_id, \
           environment_id, status, attempt_count, next_attempt_at, created_at, updated_at) \
         SELECT gen_random_uuid(), subscription_id, event_id, tenant_id, application_id, environment_id, \
                'PENDING', 0, now(), now(), now() FROM devplat_webhook_deliveries WHERE id = '{delivery}'"
    )).await.expect_err("one automatic delivery per (subscription, event)");
    let (tenant, app, env) = scope;
    c.db.conn.execute_unprepared(&format!(
        "INSERT INTO devplat_webhook_deliveries (id, subscription_id, event_id, tenant_id, application_id, \
           environment_id, replay_of, status, attempt_count, next_attempt_at, created_at, updated_at) \
         SELECT gen_random_uuid(), '{sub}', event_id, '{tenant}', '{app}', '{env}', '{delivery}', 'PENDING', 0, now(), now(), now() \
           FROM devplat_webhook_events"
    )).await.unwrap();
    assert_eq!(w.service.deliver_due_once(100).await.unwrap().shadowed, 1);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE status <> 'SHADOW'")
            .await,
        0
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_attempts")
            .await,
        0
    );
    // The cut-over: an ACTIVE worker on the same data sends none of the shadow backlog.
    let active = WebhookService::new(c.db.conn.clone(), settings());
    assert_eq!(
        active.deliver_due_once(100).await.unwrap(),
        Default::default()
    );
    assert!(rx.seen().is_empty(), "shadow mode never sends");
}

/// History is immutable: events and attempts are append-only, deliveries and
/// subscriptions are never deleted, terminal deliveries never change, and a signing secret
/// can only be stored sealed. Startup refuses a key ring that cannot open stored seals.
#[tokio::test]
async fn history_is_immutable_and_secrets_are_only_sealed() {
    let Some((c, w, scope, d)) = world(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (sub, secret) = subscribe(&c, &d, &rx.url, &["call.requested"]).await;
    publish(&c, &w.producers.calling, &call_requested(scope)).await;
    w.service.deliver_due_once(100).await.unwrap();
    for sql in [
        "UPDATE devplat_webhook_events SET event_type = 'call.ended'".to_string(),
        "DELETE FROM devplat_webhook_events".to_string(),
        "TRUNCATE devplat_webhook_events CASCADE".to_string(),
        "UPDATE devplat_webhook_attempts SET http_status = 200".to_string(),
        "DELETE FROM devplat_webhook_attempts".to_string(),
        "DELETE FROM devplat_webhook_deliveries".to_string(),
        "UPDATE devplat_webhook_deliveries SET status = 'FAILED'".to_string(),
        "UPDATE devplat_webhook_deliveries SET subscription_id = gen_random_uuid()".to_string(),
        format!("DELETE FROM devplat_webhook_subscriptions WHERE id = '{sub}'"),
        format!("UPDATE devplat_webhook_subscriptions SET environment_id = gen_random_uuid() WHERE id = '{sub}'"),
        format!("UPDATE devplat_webhook_subscriptions SET secret_sealed = '{secret}' WHERE id = '{sub}'"),
        format!("UPDATE devplat_webhook_subscriptions SET secret_sealed = 'v2.k0.{secret}' WHERE id = '{sub}'"),
        format!("UPDATE devplat_webhook_subscriptions SET secret_key_id = 'k9' WHERE id = '{sub}'"),
    ] {
        assert!(c.db.conn.execute_unprepared(&sql).await.is_err(), "{sql}");
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_attempts")
            .await,
        1
    );
    // Startup coverage: a ring without the stored key id is refused.
    use cf_webhooks::secret_box::SealingKey;
    let mut other = settings();
    other.key_ring = Some(Arc::new(
        SecretBox::new(SealingKey::from_bytes("k9", &key_bytes(7)).unwrap(), None).unwrap(),
    ));
    let err = developer_platform::webhooks::require_signable(&c.db.conn, &other)
        .await
        .unwrap_err();
    assert!(err.contains("k0") && !err.contains(&secret));
    developer_platform::webhooks::require_signable(&c.db.conn, &settings())
        .await
        .unwrap();
    // A delivery sealed under an unknown key is refused and retried, never sent unsigned.
    publish(&c, &w.producers.calling, &call_requested(scope)).await;
    let svc = WebhookService::new(c.db.conn.clone(), other);
    let before = rx.seen().len();
    assert_eq!(svc.deliver_due_once(100).await.unwrap().retried, 1);
    assert_eq!(rx.seen().len(), before);
    assert_eq!(c.count("SELECT count(*) FROM devplat_webhook_deliveries WHERE failure_category = 'SIGNING_KEY_UNKNOWN'").await, 1);
}

// ---------------------------------------------------------------------------
// Developer Console.
// ---------------------------------------------------------------------------

fn id_of(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

/// Invites `email` with `role` and has them accept; returns their session token.
async fn join(c: &Control, owner: &str, org: Uuid, email: &str, role: &str) -> String {
    let (token, _) = c.sessions.sign_in(email);
    let inv = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{org}/invitations"),
            Some(json!({ "email": email, "role": role })),
        )
        .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.json);
    let acc = c
        .req(
            As::User(&token),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": inv.json["token"] })),
        )
        .await;
    assert_eq!(acc.status, StatusCode::OK, "{}", acc.json);
    token
}

/// Workspace members manage an environment's webhooks by role (OWNER / ADMIN / DEVELOPER
/// manage, ANALYST reads, BILLING nothing), within the organization lifecycle rules, with
/// activity audit and the console views (origin, health, attempts).
#[tokio::test]
async fn console_members_manage_environment_webhooks_by_role() {
    let Some((c, w)) = Control::with_webhooks(settings()).await else {
        return;
    };
    let rx = Receiver::start().await;
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let org = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    let o = id_of(&org.json);
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "App" })),
        )
        .await;
    let pid = id_of(&p.json);
    let e = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{pid}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    conforms(&format!("{CTRL}Environment"), &e.json);
    let env = id_of(&e.json);
    let developer = join(&c, &owner, o, "dev@acme.example", "DEVELOPER").await;
    let analyst = join(&c, &owner, o, "analyst@acme.example", "ANALYST").await;
    let billing = join(&c, &owner, o, "billing@acme.example", "BILLING").await;
    let (stranger, _) = c.sessions.sign_in("stranger@other.example");
    let base = format!("/v1/environments/{env}/webhooks");
    let body = json!({ "url": rx.url, "eventTypes": ["call.requested", "meeting.started"] });
    // A developer creates (idempotently); the replay never carries the secret again.
    let created = c
        .req_with(
            As::User(&developer),
            Method::POST,
            &base,
            Some(body.clone()),
            &[("idempotency-key", "k-1")],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    conforms(&format!("{WH}ConsoleWebhookWithSecret"), &created.json);
    let secret = created.json["signingSecret"].as_str().unwrap().to_string();
    assert_eq!(created.json["webhook"]["environmentKind"], "SANDBOX");
    assert_eq!(created.json["webhook"]["origin"], "NATIVE");
    let replay = c
        .req_with(
            As::User(&developer),
            Method::POST,
            &base,
            Some(body.clone()),
            &[("idempotency-key", "k-1")],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    conforms(&format!("{WH}ConsoleWebhookWithSecret"), &replay.json);
    assert!(replay.json.get("signingSecret").is_none());
    assert_eq!(
        replay.json["webhook"]["webhookId"],
        created.json["webhook"]["webhookId"]
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_webhook_subscriptions")
            .await,
        1
    );
    let stored = one(
        &c,
        "SELECT string_agg(response_body::text, '|') FROM devplat_idempotency_records",
    )
    .await;
    assert!(
        !stored.contains(&secret),
        "one-time secrets are never stored"
    );
    let id = created.json["webhook"]["webhookId"]
        .as_str()
        .unwrap()
        .to_string();
    // Reads: owner, developer, analyst; never billing or a stranger.
    for who in [&owner, &developer, &analyst] {
        let r = c.req(As::User(who), Method::GET, &base, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        conforms(&format!("{WH}ConsoleWebhookList"), &r.json);
        assert!(!r.json.to_string().contains(&secret));
    }
    assert_eq!(
        c.req(As::User(&billing), Method::GET, &base, None)
            .await
            .json["error"]["code"],
        "PERMISSION_DENIED"
    );
    assert_eq!(
        c.req(As::User(&stranger), Method::GET, &base, None)
            .await
            .json["error"]["code"],
        "NOT_FOUND"
    );
    assert_eq!(
        c.req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{}/webhooks", Uuid::new_v4()),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Changes: the analyst is refused (and the refusal audited).
    let r = c
        .req(As::User(&analyst), Method::POST, &base, Some(body.clone()))
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    let r = c
        .req(
            As::User(&analyst),
            Method::PATCH,
            &format!("{base}/{id}"),
            Some(json!({ "expectedVersion": 1, "status": "DISABLED" })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    // A machine credential uses the customer API, not the console.
    let d = c.pa.machine(env, ALL);
    assert_eq!(
        c.req(As::Machine(&d), Method::GET, &base, None).await.json["error"]["code"],
        "PERMISSION_DENIED"
    );
    // ...where it sees the same subscription (same environment).
    let mine = c
        .req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{id}"),
            None,
        )
        .await;
    assert_eq!(mine.status, StatusCode::OK);
    // Deliveries, attempts and replay through the console.
    let scope = (o, pid, env);
    rx.script(&[503]);
    publish(&c, &w.producers.calling, &call_requested(scope)).await;
    w.service.deliver_due_once(100).await.unwrap();
    let dl = c
        .req(
            As::User(&analyst),
            Method::GET,
            &format!("{base}/{id}/deliveries"),
            None,
        )
        .await;
    conforms(&format!("{WH}ConsoleDeliveryList"), &dl.json);
    let delivery = dl.json["items"][0]["deliveryId"]
        .as_str()
        .unwrap()
        .to_string();
    let at = c
        .req(
            As::User(&analyst),
            Method::GET,
            &format!("/v1/environments/{env}/webhook-deliveries/{delivery}/attempts"),
            None,
        )
        .await;
    assert_eq!(at.status, StatusCode::OK, "{}", at.json);
    conforms(&format!("{WH}WebhookAttemptList"), &at.json);
    assert_eq!(at.json["items"][0]["outcome"], "RETRY");
    assert_eq!(at.json["items"][0]["httpStatus"], 503);
    let r = c
        .req(
            As::User(&analyst),
            Method::POST,
            &format!("/v1/environments/{env}/webhook-deliveries/{delivery}/replay"),
            None,
        )
        .await;
    assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    let r = c
        .req(
            As::User(&developer),
            Method::POST,
            &format!("/v1/environments/{env}/webhook-deliveries/{delivery}/replay"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let got = c
        .req(
            As::User(&developer),
            Method::GET,
            &format!("{base}/{id}"),
            None,
        )
        .await;
    conforms(&format!("{WH}ConsoleWebhook"), &got.json);
    assert_eq!(got.json["health"]["consecutiveFailures"], 1);
    // Rotation and deletion by the owner.
    let rot = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("{base}/{id}/rotate-secret"),
            Some(json!({ "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(rot.status, StatusCode::OK, "{}", rot.json);
    assert_ne!(rot.json["signingSecret"].as_str().unwrap(), secret);
    // Lifecycle: a restricted organization reads but changes nothing.
    c.db.conn.execute_unprepared(&format!("UPDATE devplat_organizations SET state = 'RESTRICTED', state_reason = 'BILLING_HOLD' WHERE tenant_id = '{o}'")).await.unwrap();
    let r = c
        .req(
            As::User(&owner),
            Method::DELETE,
            &format!("{base}/{id}?expectedVersion=2"),
            None,
        )
        .await;
    assert_eq!(r.json["error"]["code"], "ORGANIZATION_RESTRICTED");
    assert_eq!(
        c.req(As::User(&owner), Method::GET, &base, None)
            .await
            .status,
        StatusCode::OK
    );
    let r = c
        .req(
            As::Machine(&d),
            Method::POST,
            "/v1/webhooks",
            Some(body.clone()),
        )
        .await;
    assert_eq!(
        r.json["error"]["code"], "ORGANIZATION_RESTRICTED",
        "the machine path honours the lifecycle too"
    );
    c.db.conn.execute_unprepared(&format!("UPDATE devplat_organizations SET state = 'ACTIVE', state_reason = NULL WHERE tenant_id = '{o}'")).await.unwrap();
    let r = c
        .req(
            As::User(&owner),
            Method::DELETE,
            &format!("{base}/{id}?expectedVersion=2"),
            None,
        )
        .await;
    assert_eq!(
        (r.status, r.json["status"].clone()),
        (StatusCode::OK, json!("DELETED"))
    );
    // Audit: the changes, by the humans who made them; denied attempts; no secret, no URL.
    let act = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{o}/activity?limit=100"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}ActivityList"), &act.json);
    let actions: Vec<String> = act.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            format!(
                "{}:{}",
                i["action"].as_str().unwrap(),
                i["outcome"].as_str().unwrap()
            )
        })
        .collect();
    for expected in [
        "webhook.created:SUCCEEDED",
        "webhook.delivery_replayed:SUCCEEDED",
        "webhook.secret_rotated:SUCCEEDED",
        "webhook.deleted:SUCCEEDED",
        "webhook.manage:DENIED",
    ] {
        assert!(
            actions.contains(&expected.to_string()),
            "{expected}: {actions:?}"
        );
    }
    let text = act.json.to_string();
    assert!(!text.contains("whsec_") && !text.contains("127.0.0.1"));
}

/// The event contract and the catalog cannot drift: every public type and every `data`
/// key the projections can emit is in the published schema, and Calling's whole public
/// catalog is served unchanged (the cut-over loses no event type).
#[test]
fn the_event_contract_matches_the_catalog_and_keeps_callings_catalog() {
    use developer_platform::webhooks::catalog;
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
    let read = |rel: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(rel)).unwrap()).unwrap()
    };
    let ours = read("platform-developer/webhooks/v1.schema.json");
    let strings = |v: &Value| -> Vec<String> {
        let mut out: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        out.sort();
        out
    };
    let types = strings(&ours["$defs"]["WebhookEventType"]["enum"]);
    let mut catalog_types: Vec<String> = catalog::CATALOG
        .iter()
        .map(|d| d.name.to_string())
        .collect();
    catalog_types.sort();
    assert_eq!(types, catalog_types);
    let keys =
        strings(&ours["$defs"]["WebhookEvent"]["properties"]["data"]["propertyNames"]["enum"]);
    assert_eq!(
        keys,
        catalog::public_keys()
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
    );
    let calling = read("platform/webhooks/v1.schema.json");
    for t in strings(&calling["$defs"]["WebhookEventType"]["enum"]) {
        assert!(catalog::definition(&t).is_some(), "Calling's {t} is served");
    }
    for k in
        strings(&calling["$defs"]["WebhookEvent"]["properties"]["data"]["propertyNames"]["enum"])
    {
        assert!(keys.contains(&k), "Calling's data key {k}");
    }
    // The customer shapes are Calling's: same required fields.
    for def in [
        "Webhook",
        "WebhookWithSecret",
        "WebhookList",
        "WebhookDelivery",
        "WebhookDeliveryList",
        "CreateWebhookRequest",
        "UpdateWebhookRequest",
        "RotateWebhookSecretRequest",
    ] {
        assert_eq!(
            ours["$defs"][def]["required"], calling["$defs"][def]["required"],
            "{def}"
        );
        let props = |v: &Value| -> Vec<String> {
            let mut k: Vec<String> = v["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            k.sort();
            k
        };
        assert_eq!(
            props(&ours["$defs"][def]),
            props(&calling["$defs"][def]),
            "{def}"
        );
    }
}

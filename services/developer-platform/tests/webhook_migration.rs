//! Section 53 §9.1 / §19.3 — the webhook migration from Calling, the Developer Platform's
//! side, through the REAL router (real operator / producer service tokens), a real
//! PostgreSQL database and a real local receiver. Calling is a scripted export here that
//! speaks Calling's export contract and seals secrets exactly as Calling does
//! (`cf_webhooks::secret_box` under the shared transfer key); the real processes are
//! exercised end to end by `certification/tests/webhook_migration.rs`.

mod common;

use std::{
    collections::BTreeMap,
    io::Write,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use axum::{
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::post,
    Router,
};
use bytes::Bytes;
use cf_webhooks::{
    endpoint::EndpointPolicy,
    secret_box::{SealingKey, SecretBox},
    signing,
};
use chrono::{DateTime, SecondsFormat, Utc};
use common::*;
use developer_platform::webhooks::{
    migration::{
        import::ImportOptions,
        port::{CallingMigrationPort, Entity, ExportPage, SourceError},
        Migration, EXPORT_PURPOSE, ROLLBACK_PURPOSE,
    },
    DeliveryMode, WebhookSettings,
};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Keys, settings, receiver.
// ---------------------------------------------------------------------------

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

/// The transfer key both services hold.
fn transfer() -> Arc<SecretBox> {
    ring("transfer-1", 91)
}

fn settings(mode: DeliveryMode) -> WebhookSettings {
    WebhookSettings {
        enabled: true,
        key_ring: Some(ring("dp-1", 42)),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        mode,
        request_timeout: StdDuration::from_secs(1),
        max_attempts: 3,
        auto_disable_after: 50,
        mutations_frozen: false,
        transfer_key: Some(transfer()),
    }
}

#[derive(Clone)]
struct Received {
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Default)]
struct ReceiverState {
    seen: Vec<Received>,
    /// (status, Retry-After) answers in order, then 200.
    script: Vec<(u16, Option<&'static str>)>,
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
    let mut s = state.lock().unwrap();
    s.seen.push(Received {
        headers,
        body: body.to_vec(),
    });
    let (status, retry) = if s.script.is_empty() {
        (200, None)
    } else {
        s.script.remove(0)
    };
    let mut r = StatusCode::from_u16(status).unwrap().into_response();
    if let Some(v) = retry {
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
}

fn header(r: &Received, name: &str) -> String {
    r.headers.get(name).unwrap().to_str().unwrap().to_string()
}

/// The customer's own verification with `secret` (reference verifier).
fn verify(r: &Received, secret: &str) -> Result<(), signing::VerifyError> {
    let ts = header(r, signing::HEADER_TIMESTAMP);
    signing::verify(
        secret.as_bytes(),
        &header(r, signing::HEADER_SIGNATURE),
        &ts,
        &header(r, signing::HEADER_DELIVERY_ID),
        &r.body,
        ts.parse().unwrap(),
        300,
    )
}

// ---------------------------------------------------------------------------
// A scripted Calling export (Calling's export contract, Calling's sealing).
// ---------------------------------------------------------------------------

fn exact(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Calling's keyed fingerprint (`webhooks::ownership_migration::secret_fingerprint`).
fn fingerprint(secret: &str, id: Uuid) -> String {
    signing::sign(
        secret.as_bytes(),
        0,
        &format!("webhook-migration-fingerprint|{id}"),
        b"cfmeet",
    )
}

fn owner(purpose: &str, id: Uuid) -> String {
    format!("{purpose}|{id}")
}

#[derive(Default)]
struct CallingData {
    subscriptions: Vec<Value>,
    events: Vec<Value>,
    deliveries: Vec<Value>,
    attempts: Vec<Value>,
    delivery_enabled: bool,
    frozen: bool,
    in_flight: i64,
    /// Rollback exports received (subscription items).
    rolled_back: Vec<Value>,
    /// Delivery ids settled by a rollback.
    settled: Vec<Uuid>,
    /// Pages served (crash / resume evidence).
    pages_served: usize,
}

#[derive(Clone, Default)]
struct FakeCalling(Arc<Mutex<CallingData>>);

/// Calling's canonical digest lines (`sha256-canonical-lines-v1`), written independently
/// of both services' implementations.
fn canonical(entity: Entity, r: &Value) -> (String, String) {
    match entity {
        Entity::Subscriptions => {
            let mut types: Vec<String> = serde_json::from_value(r["eventTypes"].clone()).unwrap();
            types.sort();
            let line = json!([
                r["webhookId"],
                r["tenantId"],
                r["applicationId"],
                r["environmentId"],
                r["url"],
                r["description"],
                types,
                r["status"],
                r["secretVersion"],
                r["version"],
                r["createdAt"],
                r["updatedAt"],
                r["deletedAt"]
            ]);
            (
                r["webhookId"].as_str().unwrap().to_string(),
                line.to_string(),
            )
        }
        Entity::Events => {
            let line = json!([
                r["eventId"],
                r["producer"],
                r["sourceEventId"],
                r["eventType"],
                r["tenantId"],
                r["applicationId"],
                r["environmentId"],
                r["occurredAt"],
                r["createdAt"],
                r["bodySha256"]
            ]);
            (r["eventId"].as_str().unwrap().to_string(), line.to_string())
        }
        Entity::Deliveries => {
            let line = json!([
                r["deliveryId"],
                r["webhookId"],
                r["eventId"],
                r["replayOf"],
                r["status"],
                r["attemptCount"],
                r["nextAttemptAt"],
                r["lastHttpStatus"],
                r["failureCategory"],
                r["createdAt"],
                r["completedAt"]
            ]);
            (
                r["deliveryId"].as_str().unwrap().to_string(),
                line.to_string(),
            )
        }
        Entity::Attempts => {
            let line = json!([
                r["deliveryId"],
                r["attemptNumber"],
                r["outcome"],
                r["httpStatus"],
                r["failureCategory"],
                r["startedAt"],
                r["finishedAt"]
            ]);
            (
                format!(
                    "{}:{:02}",
                    r["deliveryId"].as_str().unwrap(),
                    r["attemptNumber"].as_i64().unwrap()
                ),
                line.to_string(),
            )
        }
    }
}

impl FakeCalling {
    fn rows(d: &CallingData, entity: Entity) -> &Vec<Value> {
        match entity {
            Entity::Subscriptions => &d.subscriptions,
            Entity::Events => &d.events,
            Entity::Deliveries => &d.deliveries,
            Entity::Attempts => &d.attempts,
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut CallingData) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

#[async_trait::async_trait]
impl CallingMigrationPort for FakeCalling {
    async fn inventory(&self) -> Result<Value, SourceError> {
        let d = self.0.lock().unwrap();
        let open = d
            .deliveries
            .iter()
            .filter(|x| {
                ["PENDING", "RETRY_SCHEDULED", "DELIVERING"]
                    .contains(&x["status"].as_str().unwrap())
            })
            .count();
        Ok(json!({
            "generatedAt": exact(Utc::now()),
            "flags": { "mutationsFrozen": d.frozen, "deliveryEnabled": d.delivery_enabled,
                       "authority": "active", "signingKeyRingConfigured": true, "transferKeyId": "transfer-1" },
            "subscriptions": { "total": d.subscriptions.len(), "byStatus": {}, "unopenableSecrets": 0, "unopenableDeletedSecrets": 0 },
            "events": { "total": d.events.len(), "byProducer": {} },
            "deliveries": { "total": d.deliveries.len(), "byStatus": {}, "open": open, "inFlight": d.in_flight },
            "attempts": { "total": d.attempts.len() },
        }))
    }

    async fn digest(&self) -> Result<Value, SourceError> {
        let d = self.0.lock().unwrap();
        let mut out = json!({ "algorithm": "sha256-canonical-lines-v1" });
        for e in Entity::ALL {
            let lines: BTreeMap<String, String> =
                Self::rows(&d, e).iter().map(|r| canonical(e, r)).collect();
            let mut h = Sha256::new();
            for l in lines.values() {
                h.update(l.as_bytes());
                h.update(b"\n");
            }
            out[e.as_str()] = json!({ "count": lines.len(), "digest": hex::encode(h.finalize()) });
        }
        Ok(out)
    }

    async fn page(
        &self,
        entity: Entity,
        cursor: Option<&str>,
        limit: u32,
        open_only: bool,
    ) -> Result<ExportPage, SourceError> {
        let mut d = self.0.lock().unwrap();
        d.pages_served += 1;
        let rows: Vec<Value> = Self::rows(&d, entity)
            .iter()
            .filter(|r| {
                !open_only
                    || ["PENDING", "RETRY_SCHEDULED", "DELIVERING"]
                        .contains(&r["status"].as_str().unwrap())
            })
            .cloned()
            .collect();
        let start: usize = cursor.map(|c| c.parse().unwrap()).unwrap_or(0);
        let end = (start + limit as usize).min(rows.len());
        Ok(ExportPage {
            data: rows[start..end].to_vec(),
            next_cursor: (end < rows.len()).then(|| end.to_string()),
        })
    }

    async fn rollback_subscriptions(
        &self,
        subscriptions: Vec<Value>,
    ) -> Result<Value, SourceError> {
        let mut d = self.0.lock().unwrap();
        let n = subscriptions.len();
        d.rolled_back.extend(subscriptions);
        Ok(json!({ "applied": n, "unchanged": 0, "conflicts": [] }))
    }

    async fn settle_deliveries(&self, ids: &[Uuid]) -> Result<Value, SourceError> {
        let mut d = self.0.lock().unwrap();
        d.settled.extend_from_slice(ids);
        Ok(json!({ "requested": ids.len(), "settled": ids.len() }))
    }
}

type Scope3 = (Uuid, Uuid, Uuid);

/// A Calling subscription record whose secret is sealed for the export exactly as Calling
/// does (or UNOPENABLE).
fn calling_subscription(
    scope: Scope3,
    url: &str,
    types: &[&str],
    status: &str,
    secret: Option<&str>,
    seal_with: &SecretBox,
) -> (Uuid, Value) {
    let id = Uuid::new_v4();
    let at = exact(Utc::now() - chrono::Duration::days(3));
    let secret_json = match secret {
        Some(s) => json!({
            "state": "TRANSFER_SEALED", "transferKeyId": seal_with.current_kid(),
            "transferSealed": seal_with.seal(s, &owner(EXPORT_PURPOSE, id)),
            "fingerprint": fingerprint(s, id), "unopenableReason": null,
        }),
        None => json!({ "state": "UNOPENABLE", "transferKeyId": null, "transferSealed": null,
                        "fingerprint": null, "unopenableReason": "SIGNING_KEY_UNKNOWN" }),
    };
    let record = json!({
        "webhookId": id, "tenantId": scope.0, "applicationId": scope.1, "environmentId": scope.2,
        "environmentKind": "SANDBOX", "url": url, "description": "from calling",
        "eventTypes": types, "status": status, "secretVersion": 1, "version": 2,
        "createdByCredentialId": Uuid::new_v4(), "createdAt": at, "updatedAt": at,
        "deletedAt": if status == "DELETED" { Value::String(at.clone()) } else { Value::Null },
        "secret": secret_json,
    });
    (id, record)
}

fn calling_event(scope: Scope3, source: &str, event_type: &str) -> Value {
    let event_id = format!("whe_{}", Uuid::new_v4().simple());
    let body = json!({ "id": event_id, "type": event_type, "apiVersion": "v1",
                       "occurredAt": "2026-10-20T10:00:00.000Z",
                       "data": { "callId": Uuid::new_v4() } });
    // The fake's body hash is over Postgres's JSONB text, exactly as both services hash it.
    json!({
        "eventId": event_id, "producer": "cfmeet.calling", "sourceEventId": source,
        "eventType": event_type, "tenantId": scope.0, "applicationId": scope.1,
        "environmentId": scope.2, "occurredAt": "2026-10-20T10:00:00.000000Z",
        "createdAt": exact(Utc::now() - chrono::Duration::days(2)),
        "body": body, "bodySha256": "",
    })
}

fn calling_delivery(
    event: &Value,
    sub: Uuid,
    status: &str,
    attempts: i32,
    replay_of: Option<&str>,
) -> Value {
    let created = exact(Utc::now() - chrono::Duration::days(1));
    let open = ["PENDING", "RETRY_SCHEDULED", "DELIVERING"].contains(&status);
    json!({
        "deliveryId": Uuid::new_v4(), "webhookId": sub, "eventId": event["eventId"],
        "producer": event["producer"], "sourceEventId": event["sourceEventId"],
        "tenantId": event["tenantId"], "applicationId": event["applicationId"],
        "environmentId": event["environmentId"], "replayOf": replay_of,
        "status": status, "attemptCount": attempts,
        "nextAttemptAt": exact(Utc::now() - chrono::Duration::minutes(5)),
        "leaseUntil": null,
        "lastHttpStatus": if attempts > 0 { json!(if status == "SUCCEEDED" { 200 } else { 503 }) } else { Value::Null },
        "failureCategory": match status { "FAILED" => json!("HTTP_4XX"), "RETRY_SCHEDULED" => json!("HTTP_5XX"), _ => Value::Null },
        "createdAt": created, "updatedAt": created,
        "completedAt": if open { Value::Null } else { Value::String(created) },
    })
}

fn calling_attempt(delivery: &Value, n: i32, outcome: &str, http: i32) -> Value {
    let at = exact(Utc::now() - chrono::Duration::hours(20));
    json!({ "deliveryId": delivery["deliveryId"], "attemptNumber": n, "outcome": outcome,
            "httpStatus": http, "failureCategory": if outcome == "SUCCEEDED" { Value::Null } else { json!("HTTP_5XX") },
            "startedAt": at, "finishedAt": at })
}

/// A Calling envelope of `source` (its outbox event id) that projects to call.requested.
fn call_requested(scope: Scope3, source: &str) -> Value {
    json!({
        "event_id": source,
        "event_type": "calling.call-requested",
        "schema_version": 1,
        "occurred_at": "2026-10-23T10:00:00Z",
        "producer_service": "cfmeet.calling",
        "runtime_environment": "test",
        "correlation_id": "corr_0a1b2c3d4e5f60718293a4b5c6d7e8f9",
        "ownership": { "tenant_id": scope.0, "application_id": scope.1, "application_environment_id": scope.2 },
        "payload": { "event_type": "CALL_REQUESTED", "call_id": Uuid::new_v4(), "call_version": 1,
                     "data": { "reason": "OUTBOUND" } },
    })
}

// ---------------------------------------------------------------------------
// World.
// ---------------------------------------------------------------------------

struct World {
    c: Control,
    w: Webhooks,
    calling: FakeCalling,
    scope: Scope3,
    rx: Receiver,
}

impl World {
    async fn new(s: WebhookSettings) -> Option<Self> {
        let calling = FakeCalling::default();
        calling.with(|d| d.delivery_enabled = true);
        let port: Arc<dyn CallingMigrationPort> = Arc::new(calling.clone());
        let (c, w) =
            Control::with_webhooks_configured(s, move |state| state.with_calling_export(port))
                .await?;
        let id = Uuid::new_v4();
        let (tenant, app) = (Uuid::new_v4(), Uuid::new_v4());
        c.pa.envs.lock().unwrap().insert(
            id,
            MemEnv {
                tenant,
                app,
                kind: "SANDBOX".into(),
                status: "ACTIVE".into(),
            },
        );
        let rx = Receiver::start().await;
        Some(Self {
            c,
            w,
            calling,
            scope: (tenant, app, id),
            rx,
        })
    }

    async fn op(&self, method: Method, path: &str, body: Option<Value>) -> Response {
        self.c
            .operator(
                method,
                &format!("/internal/v1/operator/webhook-migration/{path}"),
                body,
            )
            .await
    }

    /// The library entry point (tests inject a crash with `fail_after_pages`).
    async fn import_with(
        &self,
        opts: ImportOptions,
    ) -> Result<Value, developer_platform::http::error::ApiError> {
        let m = Migration {
            db: &self.c.db.conn,
            webhooks: &self.w.service,
            calling: &self.calling,
            platform_access: self.c.pa.as_ref(),
            operator: "ops.tester",
            request_id: "req_test".into(),
            correlation: cf_service_auth::CorrelationId::generate(),
        };
        m.import(opts).await
    }

    async fn one(&self, sql: &str) -> String {
        self.c
            .db
            .conn
            .query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
            .await
            .unwrap()
            .map(|r| r.try_get_by_index::<String>(0).unwrap_or_default())
            .unwrap_or_default()
    }

    async fn deliver(&self) -> developer_platform::webhooks::worker::DeliveryReport {
        self.w.service.deliver_due_once(100).await.unwrap()
    }

    /// Fixes each fake event's `bodySha256` to the hash Postgres computes of its JSONB text.
    async fn hash_bodies(&self) {
        let events = self.calling.with(|d| d.events.clone());
        let mut hashed = Vec::new();
        for mut e in events {
            let h = self
                .one(&format!(
                    "SELECT encode(sha256(convert_to(('{}'::jsonb)::text, 'UTF8')), 'hex')",
                    e["body"].to_string().replace('\'', "''")
                ))
                .await;
            e["bodySha256"] = json!(h);
            hashed.push(e);
        }
        self.calling.with(|d| d.events = hashed);
    }

    /// Every devplat webhook row (and migration bookkeeping) as text.
    async fn dump(&self) -> String {
        let mut out = String::new();
        for t in [
            "devplat_webhook_subscriptions",
            "devplat_webhook_events",
            "devplat_webhook_deliveries",
            "devplat_webhook_attempts",
            "devplat_webhook_migration_runs",
            "devplat_webhook_migration_subscriptions",
            "devplat_webhook_migration_audit",
            "devplat_webhook_migration_state",
            "devplat_activity_events",
        ] {
            out.push_str(
                &self
                    .one(&format!("SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY row_to_json(t)::text), '') FROM {t} t"))
                    .await,
            );
        }
        out
    }

    async fn counts(&self) -> String {
        self.one(
            "SELECT (SELECT count(*) FROM devplat_webhook_subscriptions) || ',' || \
                    (SELECT count(*) FROM devplat_webhook_events) || ',' || \
                    (SELECT count(*) FROM devplat_webhook_deliveries) || ',' || \
                    (SELECT count(*) FROM devplat_webhook_attempts) || ',' || \
                    (SELECT count(*) FROM devplat_webhook_migration_runs) || ',' || \
                    (SELECT count(*) FROM devplat_webhook_migration_subscriptions) || ',' || \
                    (SELECT count(*) FROM devplat_webhook_migration_audit) || ',' || \
                    (SELECT count(*) FROM devplat_webhook_migration_state)",
        )
        .await
    }
}

/// The standard Calling data set: an ACTIVE, a DISABLED and a DELETED subscription;
/// delivered, dead-lettered, retrying, pending and replayed deliveries; attempts.
struct Seeded {
    active: Uuid,
    active_secret: String,
    secrets: Vec<String>,
    /// A delivery still open at Calling (held mirror after import).
    pending: Value,
    retrying: Value,
    succeeded: Value,
}

fn seed(w: &World) -> Seeded {
    let t = transfer();
    let active_secret = signing::generate_secret();
    let disabled_secret = signing::generate_secret();
    let deleted_secret = signing::generate_secret();
    let (active, a) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested", "call.ringing"],
        "ACTIVE",
        Some(&active_secret),
        &t,
    );
    let (disabled, b) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "DISABLED",
        Some(&disabled_secret),
        &t,
    );
    let (_, c) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "DELETED",
        Some(&deleted_secret),
        &t,
    );
    let e1 = calling_event(
        w.scope,
        &format!("evt_{}", Uuid::new_v4().simple()),
        "call.requested",
    );
    let e2 = calling_event(
        w.scope,
        &format!("evt_{}", Uuid::new_v4().simple()),
        "call.ringing",
    );
    let e3 = calling_event(
        w.scope,
        &format!("evt_{}", Uuid::new_v4().simple()),
        "call.requested",
    );
    let succeeded = calling_delivery(&e1, active, "SUCCEEDED", 1, None);
    let failed = calling_delivery(&e1, disabled, "FAILED", 1, None);
    let retrying = calling_delivery(&e2, active, "RETRY_SCHEDULED", 1, None);
    let pending = calling_delivery(&e3, active, "PENDING", 0, None);
    let replay = calling_delivery(
        &e1,
        active,
        "SUCCEEDED",
        1,
        succeeded["deliveryId"].as_str(),
    );
    let attempts = vec![
        calling_attempt(&succeeded, 1, "SUCCEEDED", 200),
        calling_attempt(&failed, 1, "FAILED", 410),
        calling_attempt(&retrying, 1, "RETRY", 503),
        calling_attempt(&replay, 1, "SUCCEEDED", 200),
    ];
    w.calling.with(|d| {
        d.subscriptions = vec![a, b, c];
        d.events = vec![e1, e2, e3];
        // The replay first in the page: the importer orders it after its original.
        d.deliveries = vec![
            replay,
            succeeded.clone(),
            failed,
            retrying.clone(),
            pending.clone(),
        ];
        d.attempts = attempts;
    });
    Seeded {
        active,
        active_secret: active_secret.clone(),
        secrets: vec![active_secret, disabled_secret, deleted_secret],
        pending,
        retrying,
        succeeded,
    }
}

fn assert_no_secret(what: &str, text: &str, secrets: &[String]) {
    for s in secrets {
        assert!(
            !text.contains(s.as_str()),
            "{what} contains a plaintext secret"
        );
        assert!(
            !text.contains(s.trim_start_matches("whsec_")),
            "{what} contains a secret's random part"
        );
    }
}

/// Moves the fake Calling to "stopped sending" (the cutover's Calling side).
fn stop_calling(w: &World) {
    w.calling.with(|d| {
        d.delivery_enabled = false;
        d.frozen = true;
        d.in_flight = 0;
    });
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// A clean Calling: the dry run and the import find nothing, the run completes (one page
/// per entity), reconcile is clean; operator routes need the permission and a named
/// operator.
#[tokio::test]
async fn an_empty_calling_imports_nothing_and_reconciles_clean() {
    let Some(w) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    let r = w.op(Method::GET, "inventory", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["dryRun"], true);
    assert_eq!(r.json["wouldImport"]["subscriptions"]["seen"], 0);
    let r = w.op(Method::POST, "import", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["state"], "COMPLETED");
    assert_eq!(r.json["pages"], 4);
    let r = w.op(Method::GET, "reconcile", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["clean"], true, "{}", r.json);
    let r = w.op(Method::GET, "status", None).await;
    assert_eq!(r.json["phase"], "PREPARING");
    // Without a named operator / without the permission.
    let signed =
        w.c.peers
            .gateway
            .sign(&cf_service_auth::OutboundServiceRequest::new_operation(
                cf_service_auth::ServiceId::developer_platform(),
            ))
            .unwrap();
    let mut headers = HeaderMap::new();
    signed.apply_to(&mut headers);
    headers.insert("x-cf-operator", "ops.tester".parse().unwrap());
    let r =
        w.c.raw(
            Method::GET,
            "/internal/v1/operator/webhook-migration/status".into(),
            headers,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let signed =
        w.c.operator
            .sign(&cf_service_auth::OutboundServiceRequest::new_operation(
                cf_service_auth::ServiceId::developer_platform(),
            ))
            .unwrap();
    let mut headers = HeaderMap::new();
    signed.apply_to(&mut headers);
    let r =
        w.c.raw(
            Method::GET,
            "/internal/v1/operator/webhook-migration/status".into(),
            headers,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["error"]["code"], "OPERATOR_REQUIRED");
}

/// The dry run applies everything inside a transaction that is rolled back: it reports
/// what would be imported (and the rotation-required subscriptions) and writes nothing.
#[tokio::test]
async fn the_dry_run_reports_and_writes_nothing() {
    let Some(w) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    seed(&w);
    let (unopenable, rec) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "ACTIVE",
        None,
        &transfer(),
    );
    w.calling.with(|d| d.subscriptions.push(rec));
    w.hash_bodies().await;
    let before = w.dump().await;
    let r = w.op(Method::GET, "inventory?pageSize=2", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let would = &r.json["wouldImport"];
    assert_eq!(would["subscriptions"]["inserted"], 4);
    assert_eq!(would["events"]["inserted"], 3);
    assert_eq!(would["deliveries"]["inserted"], 5);
    assert_eq!(would["deliveries"]["held"], 2);
    assert_eq!(would["attempts"]["inserted"], 4);
    assert_eq!(r.json["rotationRequired"], 1);
    assert_eq!(r.json["conflicts"], json!([]));
    assert_eq!(w.dump().await, before, "the dry run wrote nothing");
    assert_eq!(w.counts().await, "0,0,0,0,0,0,0,0");
    let _ = unopenable;
}

/// Import preserves every id, status, replay reference and attempt as immutable history
/// with origin MIGRATED_FROM_CALLING; Calling's open deliveries are held mirrors; the
/// secret is re-sealed under this deployment's key ring; importing again changes nothing
/// (no duplicate) and reconcile is clean.
#[tokio::test]
async fn the_import_preserves_history_and_importing_twice_changes_nothing() {
    let Some(w) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    let s = seed(&w);
    w.hash_bodies().await;
    let r = w
        .op(Method::POST, "import", Some(json!({ "pageSize": 2 })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["state"], "COMPLETED", "{}", r.json);
    assert_eq!(r.json["resumed"], false);
    assert_eq!(r.json["conflicts"], json!([]), "{}", r.json);
    let counts = w.counts().await;
    assert!(counts.starts_with("3,3,5,4,1,3,"), "{counts}");
    assert_eq!(
        w.one(&format!(
            "SELECT origin || ':' || status || ':' || version || ':' || secret_state || ':' || \
                    secret_key_id || ':' || created_by FROM devplat_webhook_subscriptions WHERE id = '{}'",
            s.active
        ))
        .await,
        "MIGRATED_FROM_CALLING:ACTIVE:2:VALID:dp-1:migration:calling"
    );
    // The seal opens with this deployment's key ring to the ORIGINAL secret.
    let sealed = w
        .one(&format!(
            "SELECT secret_sealed FROM devplat_webhook_subscriptions WHERE id = '{}'",
            s.active
        ))
        .await;
    assert_eq!(
        ring("dp-1", 42)
            .open(&sealed, &s.active.to_string())
            .unwrap(),
        s.active_secret
    );
    for (d, expected) in [
        (&s.pending, "PENDING:true"),
        (&s.retrying, "RETRY_SCHEDULED:true"),
        (&s.succeeded, "SUCCEEDED:false"),
    ] {
        assert_eq!(
            w.one(&format!(
                "SELECT status || ':' || migration_hold FROM devplat_webhook_deliveries \
                  WHERE id = '{}' AND origin = 'MIGRATED_FROM_CALLING'",
                d["deliveryId"].as_str().unwrap()
            ))
            .await,
            expected
        );
    }
    assert_eq!(
        w.one(&format!(
            "SELECT count(*)::text FROM devplat_webhook_deliveries WHERE replay_of = '{}'",
            s.succeeded["deliveryId"].as_str().unwrap()
        ))
        .await,
        "1"
    );
    // Held mirrors are neither shadowed nor sent.
    let report = w.deliver().await;
    assert_eq!(report.shadowed, 0, "{report:?}");
    assert!(w.rx.seen().is_empty());
    // Customers read the migrated subscription with Calling's version (continuity of
    // expectedVersion).
    let d = w.c.pa.machine(
        w.scope.2,
        &["webhooks.read", "webhooks.manage", "calls.read"],
    );
    let r =
        w.c.req(
            As::Machine(&d),
            Method::GET,
            &format!("/v1/webhooks/{}", s.active),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["version"], 2);
    // Twice: nothing changes (not even the seals: unchanged secrets are not re-sealed).
    let seals = w
        .one("SELECT string_agg(secret_sealed, ',' ORDER BY id) FROM devplat_webhook_subscriptions")
        .await;
    let r = w
        .op(Method::POST, "import", Some(json!({ "pageSize": 3 })))
        .await;
    assert_eq!(r.json["state"], "COMPLETED");
    for e in ["subscriptions", "events", "deliveries", "attempts"] {
        assert_eq!(r.json["counts"][e]["inserted"], 0, "{e}: {}", r.json);
        assert_eq!(r.json["counts"][e]["updated"], 0, "{e}: {}", r.json);
    }
    let after = w.counts().await;
    assert_eq!(
        after.split(',').take(4).collect::<Vec<_>>(),
        counts.split(',').take(4).collect::<Vec<_>>()
    );
    assert_eq!(
        w.one(
            "SELECT string_agg(secret_sealed, ',' ORDER BY id) FROM devplat_webhook_subscriptions"
        )
        .await,
        seals
    );
    let r = w.op(Method::GET, "reconcile", None).await;
    assert_eq!(r.json["clean"], true, "{}", r.json);
}

/// A crash after N committed pages leaves the run RUNNING at its checkpoint; the next
/// import resumes it (no page applied twice) and ends with exactly the state of an
/// uninterrupted import.
#[tokio::test]
async fn a_crashed_import_resumes_from_its_checkpoint_with_an_identical_result() {
    let Some(a) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    seed(&a);
    // Calling's export order across pages: an original before its replay.
    a.calling.with(|d| {
        let replay = d.deliveries.remove(0);
        d.deliveries.push(replay);
    });
    a.hash_bodies().await;
    let r = a
        .import_with(ImportOptions {
            page_size: 1,
            fail_after_pages: Some(4),
        })
        .await;
    assert!(r.is_err(), "the simulated crash");
    assert_eq!(
        a.one("SELECT state || ':' || pages FROM devplat_webhook_migration_runs")
            .await,
        "RUNNING:4"
    );
    assert_eq!(
        a.one("SELECT count(*)::text FROM devplat_webhook_subscriptions")
            .await,
        "3"
    );
    let served = a.calling.with(|d| d.pages_served);
    let r = a
        .op(Method::POST, "import", Some(json!({ "pageSize": 1 })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["resumed"], true);
    assert_eq!(r.json["state"], "COMPLETED");
    assert_eq!(
        a.one("SELECT count(*)::text FROM devplat_webhook_migration_runs")
            .await,
        "1"
    );
    // Resumed from the checkpoint: the subscription pages were not fetched again.
    let resumed_pages = a.calling.with(|d| d.pages_served) - served;
    assert!(resumed_pages < 15, "{resumed_pages} pages after the resume");
    for e in ["subscriptions", "events", "deliveries", "attempts"] {
        assert_eq!(r.json["counts"][e]["conflicts"], 0, "{e}");
        assert_eq!(
            r.json["counts"][e]["unchanged"], 0,
            "{e}: no page applied twice"
        );
    }
    // Identical to an uninterrupted import of the same data.
    let Some(b) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    b.calling.with(|d| {
        let src = a.calling.0.lock().unwrap();
        d.subscriptions = src.subscriptions.clone();
        d.events = src.events.clone();
        d.deliveries = src.deliveries.clone();
        d.attempts = src.attempts.clone();
    });
    let r = b
        .op(Method::POST, "import", Some(json!({ "pageSize": 7 })))
        .await;
    assert_eq!(r.json["state"], "COMPLETED", "{}", r.json);
    let ra = a.op(Method::GET, "reconcile", None).await.json;
    let rb = b.op(Method::GET, "reconcile", None).await.json;
    assert_eq!(ra["clean"], true, "{ra}");
    assert_eq!(ra["digests"], rb["digests"], "identical result");
}

/// Secret continuity: after the takeover this platform delivers Calling's held delivery
/// (same delivery id, same event id) and new events, and the ORIGINAL secret — known only
/// to the customer and to this test — verifies every signature.
#[tokio::test]
async fn the_original_secret_verifies_deliveries_after_the_takeover() {
    let Some(w) = World::new(settings(DeliveryMode::Active)).await else {
        return;
    };
    let s = seed(&w);
    w.hash_bodies().await;
    stop_calling(&w);
    let r = w.op(Method::POST, "import", Some(json!({}))).await;
    assert_eq!(r.json["state"], "COMPLETED", "{}", r.json);
    // Before the takeover, nothing to a migrated subscription is sent (Calling sends).
    let source = format!("evt_{}", Uuid::new_v4().simple());
    let p =
        w.c.internal(
            &w.w.producers.calling,
            Method::POST,
            "/internal/v1/platform/events",
            Some(call_requested(w.scope, &source)),
        )
        .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.json);
    w.deliver().await;
    assert!(w.rx.seen().is_empty(), "gated until the takeover");
    let r = w.op(Method::POST, "takeover", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["phase"], "CUT_OVER");
    w.w.advance(3600);
    w.deliver().await;
    let seen = w.rx.seen();
    let ids: Vec<String> = seen
        .iter()
        .map(|r| header(r, signing::HEADER_DELIVERY_ID))
        .collect();
    assert!(
        ids.contains(&s.pending["deliveryId"].as_str().unwrap().to_string()),
        "{ids:?}"
    );
    assert!(
        ids.contains(&s.retrying["deliveryId"].as_str().unwrap().to_string()),
        "{ids:?}"
    );
    for r in &seen {
        verify(r, &s.active_secret).expect("the original secret verifies");
        assert!(verify(r, &signing::generate_secret()).is_err());
    }
    let pending = seen
        .iter()
        .find(|r| {
            header(r, signing::HEADER_DELIVERY_ID) == s.pending["deliveryId"].as_str().unwrap()
        })
        .unwrap();
    assert_eq!(
        header(pending, signing::HEADER_EVENT_ID),
        s.pending["eventId"].as_str().unwrap()
    );
    // The retrying delivery continued Calling's attempt count.
    assert_eq!(
        w.one(&format!(
            "SELECT status || ':' || attempt_count FROM devplat_webhook_deliveries WHERE id = '{}'",
            s.retrying["deliveryId"].as_str().unwrap()
        ))
        .await,
        "SUCCEEDED:2"
    );
    // The native event after the takeover: delivered once, original secret.
    assert_eq!(seen.len(), 3, "two released mirrors + one native");
}

/// A secret Calling could not open, or a transfer seal that does not open here (another
/// transfer key, a tampered seal, a fingerprint mismatch), never gets a substitute: the
/// subscription is imported DISABLED / ROTATION_REQUIRED without any secret, cannot be
/// enabled until the customer rotates, and blocks the cutover until acknowledged.
#[tokio::test]
async fn untransferable_secrets_require_rotation_and_are_never_substituted() {
    let Some(w) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    let other_transfer = ring("transfer-1", 17);
    let (unopenable, a) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "ACTIVE",
        None,
        &transfer(),
    );
    let (wrong_key, b) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "ACTIVE",
        Some("whsec_wrongkeyAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        &other_transfer,
    );
    let (bad_fp, mut c) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "DISABLED",
        Some("whsec_fingerprintAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        &transfer(),
    );
    c["secret"]["fingerprint"] = json!(fingerprint("whsec_somethingelse", bad_fp));
    let (deleted, d) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "DELETED",
        None,
        &transfer(),
    );
    w.calling.with(|x| x.subscriptions = vec![a, b, c, d]);
    let r = w.op(Method::POST, "import", Some(json!({}))).await;
    assert_eq!(r.json["state"], "COMPLETED", "{}", r.json);
    assert_eq!(r.json["counts"]["subscriptions"]["rotationRequired"], 4);
    for (id, status, outcome) in [
        (unopenable, "DISABLED", "ROTATION_REQUIRED_UNOPENABLE"),
        (wrong_key, "DISABLED", "ROTATION_REQUIRED_TRANSFER"),
        (bad_fp, "DISABLED", "ROTATION_REQUIRED_TRANSFER"),
        (deleted, "DELETED", "ROTATION_REQUIRED_UNOPENABLE"),
    ] {
        assert_eq!(
            w.one(&format!(
                "SELECT s.status || ':' || s.secret_state || ':' || (s.secret_sealed IS NULL) || ':' || \
                        (s.secret_key_id IS NULL) || ':' || COALESCE(s.disabled_reason, '-') || ':' || m.secret_outcome \
                   FROM devplat_webhook_subscriptions s JOIN devplat_webhook_migration_subscriptions m \
                     ON m.subscription_id = s.id WHERE s.id = '{id}'"
            ))
            .await,
            format!(
                "{status}:ROTATION_REQUIRED:true:true:{}:{outcome}",
                if status == "DELETED" { "-" } else { "SECRET_ROTATION_REQUIRED" }
            )
        );
    }
    // The database itself refuses an ACTIVE subscription without a secret.
    assert!(w
        .c
        .db
        .conn
        .execute_unprepared(&format!("UPDATE devplat_webhook_subscriptions SET status = 'ACTIVE', disabled_reason = NULL WHERE id = '{unopenable}'"))
        .await
        .is_err());
    // The customer cannot enable it before rotating; after rotating they can.
    let m = w.c.pa.machine(
        w.scope.2,
        &["webhooks.read", "webhooks.manage", "calls.read"],
    );
    let path = format!("/v1/webhooks/{unopenable}");
    let r =
        w.c.req(
            As::Machine(&m),
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 2, "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");
    let r =
        w.c.req(
            As::Machine(&m),
            Method::POST,
            &format!("{path}/rotate-secret"),
            Some(json!({ "expectedVersion": 2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let fresh = r.json["signingSecret"].as_str().unwrap().to_string();
    assert!(fresh.starts_with("whsec_"));
    let r =
        w.c.req(
            As::Machine(&m),
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 3, "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    // The cutover check names the unacknowledged rotations until the operator acknowledges.
    let r = w.op(Method::GET, "cutover-check", None).await;
    let rot = r.json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "rotationsAcknowledged")
        .unwrap()
        .clone();
    assert_eq!(rot["ok"], false);
    assert_eq!(rot["detail"], 2, "{rot}");
    let r = w
        .op(
            Method::POST,
            "rotation-acknowledgements",
            Some(json!({ "webhookIds": [wrong_key, bad_fp] })),
        )
        .await;
    assert_eq!(r.json["acknowledged"], 2, "{}", r.json);
    let r = w.op(Method::GET, "cutover-check", None).await;
    let rot = r.json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "rotationsAcknowledged")
        .unwrap()
        .clone();
    assert_eq!(rot["ok"], true, "{rot}");
    // Without the transfer key the import is refused outright.
    let Some(nokey) = World::new(WebhookSettings {
        transfer_key: None,
        ..settings(DeliveryMode::Shadow)
    })
    .await
    else {
        return;
    };
    let r = nokey.op(Method::POST, "import", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");
    assert_eq!(nokey.counts().await, "0,0,0,0,0,0,0,0");
}

/// No plaintext secret anywhere: every Developer Platform row, every operator response and
/// every log line written during import, reconcile, takeover and delivery.
#[tokio::test]
async fn no_plaintext_secret_reaches_rows_responses_or_logs() {
    let Some(w) = World::new(settings(DeliveryMode::Active)).await else {
        return;
    };
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
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
        type Writer = Logs;
        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }
    let logs = Logs::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish(),
    );
    let s = seed(&w);
    w.hash_bodies().await;
    stop_calling(&w);
    let mut responses = String::new();
    for (m, p) in [
        (Method::GET, "inventory"),
        (Method::POST, "import"),
        (Method::GET, "reconcile"),
        (Method::GET, "shadow-report"),
        (Method::GET, "cutover-check"),
        (Method::POST, "takeover"),
        (Method::GET, "status"),
    ] {
        let body = (m == Method::POST).then(|| json!({}));
        let r = w.op(m, p, body).await;
        assert!(r.status.is_success(), "{p}: {} {}", r.status, r.json);
        responses.push_str(&r.json.to_string());
    }
    w.w.advance(3600);
    w.deliver().await;
    assert!(!w.rx.seen().is_empty());
    assert_no_secret("operator responses", &responses, &s.secrets);
    assert_no_secret("database", &w.dump().await, &s.secrets);
    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).to_string();
    assert!(
        text.contains("webhook migration"),
        "the commands are logged"
    );
    assert_no_secret("logs", &text, &s.secrets);
}

/// Reconcile names each drifted record precisely: a field changed at Calling, a record
/// missing here, a record changed here.
#[tokio::test]
async fn reconcile_detects_injected_drift_precisely() {
    let Some(w) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    let s = seed(&w);
    w.hash_bodies().await;
    w.op(Method::POST, "import", Some(json!({}))).await;
    assert_eq!(
        w.op(Method::GET, "reconcile", None).await.json["clean"],
        true
    );
    // Calling side: a changed URL and a new event.
    let extra = calling_event(
        w.scope,
        &format!("evt_{}", Uuid::new_v4().simple()),
        "call.requested",
    );
    let extra_id = extra["eventId"].as_str().unwrap().to_string();
    w.calling.with(|d| {
        d.subscriptions[0]["url"] = json!("https://changed.example.com/hook");
        d.events.push(extra);
    });
    w.hash_bodies().await;
    // Developer Platform side: an attempt that Calling does not have.
    w.c.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO devplat_webhook_attempts (delivery_id, attempt_number, started_at, finished_at, outcome) \
             VALUES ('{}', 9, now(), now(), 'RETRY')",
            s.pending["deliveryId"].as_str().unwrap()
        ))
        .await
        .unwrap();
    let r = w.op(Method::GET, "reconcile", None).await;
    assert_eq!(r.json["clean"], false);
    let drift = r.json["drift"].as_array().unwrap();
    let first = w.calling.with(|d| {
        d.subscriptions[0]["webhookId"]
            .as_str()
            .unwrap()
            .to_string()
    });
    assert!(
        drift.iter().any(|d| d["entity"] == "subscriptions"
            && d["id"] == first.as_str()
            && d["kind"] == "DIFFERENT"
            && d["fields"] == json!(["url"])),
        "{drift:?}"
    );
    assert!(
        drift.iter().any(|d| d["entity"] == "events"
            && d["id"] == extra_id.as_str()
            && d["kind"] == "MISSING_IN_DEVELOPER_PLATFORM"),
        "{drift:?}"
    );
    assert!(
        drift
            .iter()
            .any(|d| d["entity"] == "attempts" && d["kind"] == "EXTRA_IN_DEVELOPER_PLATFORM"),
        "{drift:?}"
    );
    assert_eq!(r.json["digests"]["deliveries"]["match"], true);
    assert_eq!(r.json["driftCount"], 3);
}

/// Shadow mode never double-delivers: an event both services ingest is delivered by
/// Calling and only recorded SHADOW here; the shadow report shows parity and that nothing
/// was sent here. In active mode before the takeover the native delivery to a migrated
/// subscription stays recorded and unsent.
#[tokio::test]
async fn shadow_mode_never_double_delivers_and_reports_parity() {
    let Some(w) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    let s = seed(&w);
    w.hash_bodies().await;
    w.op(Method::POST, "import", Some(json!({}))).await;
    // The same committed event reaches both services.
    let source = format!("evt_{}", Uuid::new_v4().simple());
    let p =
        w.c.internal(
            &w.w.producers.calling,
            Method::POST,
            "/internal/v1/platform/events",
            Some(call_requested(w.scope, &source)),
        )
        .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.json);
    let mut ev = calling_event(w.scope, &source, "call.requested");
    ev["createdAt"] = json!(exact(Utc::now()));
    let delivered = calling_delivery(&ev, s.active, "SUCCEEDED", 1, None);
    w.calling.with(|d| {
        d.events.push(ev);
        d.deliveries.push(delivered);
    });
    for _ in 0..3 {
        w.deliver().await;
        w.w.advance(600);
    }
    assert!(
        w.rx.seen().is_empty(),
        "the Developer Platform sent nothing"
    );
    assert_eq!(
        w.one(&format!(
            "SELECT d.status FROM devplat_webhook_deliveries d JOIN devplat_webhook_events e ON e.event_id = d.event_id \
              WHERE e.source_event_id = '{source}' AND e.origin = 'NATIVE' AND d.subscription_id = '{}'",
            s.active
        ))
        .await,
        "SHADOW"
    );
    let r = w.op(Method::GET, "shadow-report", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["parity"], true, "{}", r.json);
    assert_eq!(r.json["eventsComparedBothSides"], 1);
    assert_eq!(r.json["developerPlatformDeliveriesSent"], 0);
    // A Calling delivery without a counterpart here breaks parity (and is named).
    let mut lonely = calling_event(
        w.scope,
        &format!("evt_{}", Uuid::new_v4().simple()),
        "call.requested",
    );
    lonely["createdAt"] = json!(exact(Utc::now()));
    let p =
        w.c.internal(
            &w.w.producers.calling,
            Method::POST,
            "/internal/v1/platform/events",
            Some(call_requested(
                w.scope,
                lonely["sourceEventId"].as_str().unwrap(),
            )),
        )
        .await;
    assert_eq!(p.status, StatusCode::OK);
    let (other, _) = calling_subscription(
        w.scope,
        &w.rx.url,
        &["call.requested"],
        "ACTIVE",
        Some("whsec_x"),
        &transfer(),
    );
    let d2 = calling_delivery(&lonely, other, "SUCCEEDED", 1, None);
    w.calling.with(|d| {
        d.events.push(lonely);
        d.deliveries.push(d2);
    });
    let r = w.op(Method::GET, "shadow-report", None).await;
    assert_eq!(r.json["parity"], false);
    assert_eq!(r.json["missingInDeveloperPlatformCount"], 1, "{}", r.json);
}

/// The takeover requires Calling to have stopped (and this platform to deliver), then
/// sends each logical delivery exactly once: Calling's open delivery wins over a never
/// attempted native duplicate, and a native delivery Calling already delivered is never
/// re-sent.
#[tokio::test]
async fn the_takeover_requires_calling_stopped_and_delivers_each_event_once() {
    let Some(w) = World::new(settings(DeliveryMode::Active)).await else {
        return;
    };
    let s = seed(&w);
    w.hash_bodies().await;
    w.op(Method::POST, "import", Some(json!({}))).await;
    // Calling still delivers: refused.
    let r = w.op(Method::POST, "takeover", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    stop_calling(&w);
    // Calling stopped, but the last import was taken while it still delivered: refused.
    let r = w.op(Method::POST, "takeover", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // Events the native pipeline ingested while gated (active mode, PREPARING) that
    // Calling ALSO recorded: A still open at Calling (its mirror wins, the native
    // duplicate is cancelled), B already delivered by Calling (never re-sent here); and a
    // fresh event only this platform has (sent after the takeover).
    let (a_src, b_src, fresh) = (
        format!("evt_{}", Uuid::new_v4().simple()),
        format!("evt_{}", Uuid::new_v4().simple()),
        format!("evt_{}", Uuid::new_v4().simple()),
    );
    for src in [&a_src, &b_src, &fresh] {
        let p =
            w.c.internal(
                &w.w.producers.calling,
                Method::POST,
                "/internal/v1/platform/events",
                Some(call_requested(w.scope, src)),
            )
            .await;
        assert_eq!(p.status, StatusCode::OK, "{}", p.json);
    }
    w.deliver().await;
    assert!(w.rx.seen().is_empty(), "gated before the takeover");
    let ea = calling_event(w.scope, &a_src, "call.requested");
    let eb = calling_event(w.scope, &b_src, "call.requested");
    let da = calling_delivery(&ea, s.active, "PENDING", 0, None);
    let db = calling_delivery(&eb, s.active, "SUCCEEDED", 1, None);
    let a_event = ea["eventId"].as_str().unwrap().to_string();
    w.calling.with(|d| {
        d.events.extend([ea, eb]);
        d.deliveries.extend([da.clone(), db]);
    });
    w.hash_bodies().await;
    // A late re-publication of Calling history dedupes to Calling's event id.
    let late =
        w.c.internal(
            &w.w.producers.calling,
            Method::POST,
            "/internal/v1/platform/events",
            Some(call_requested(
                w.scope,
                s.succeeded["sourceEventId"].as_str().unwrap(),
            )),
        )
        .await;
    assert_eq!(late.status, StatusCode::OK);
    let r = w.op(Method::POST, "import", Some(json!({}))).await;
    assert_eq!(r.json["state"], "COMPLETED", "{}", r.json);
    assert_eq!(r.json["conflicts"], json!([]), "{}", r.json);
    let r = w.op(Method::POST, "takeover", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let result = &r.json["result"];
    assert_eq!(result["released"], 3, "{result}");
    assert_eq!(result["nativesCancelledForMirror"], 1, "{result}");
    assert_eq!(result["nativesDeliveredByCalling"], 1, "{result}");
    w.w.advance(3600);
    w.deliver().await;
    w.deliver().await;
    let seen = w.rx.seen();
    let mut by_event: BTreeMap<String, usize> = BTreeMap::new();
    for r in &seen {
        *by_event
            .entry(header(r, signing::HEADER_EVENT_ID))
            .or_default() += 1;
        verify(r, &s.active_secret).unwrap();
    }
    assert!(
        by_event.values().all(|n| *n == 1),
        "each event delivered once: {by_event:?}"
    );
    assert_eq!(
        seen.len(),
        4,
        "pending, retrying and A mirrors + the fresh native event"
    );
    assert!(
        by_event.contains_key(&a_event),
        "A carries Calling's event id"
    );
    assert!(seen
        .iter()
        .any(|r| header(r, signing::HEADER_DELIVERY_ID) == da["deliveryId"].as_str().unwrap()));
    // Takeover again: idempotent.
    let r = w.op(Method::POST, "takeover", None).await;
    assert_eq!(r.json["result"]["released"], 0);
    // Shadow mode refuses a takeover.
    let Some(shadow) = World::new(settings(DeliveryMode::Shadow)).await else {
        return;
    };
    let r = shadow.op(Method::POST, "takeover", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

/// Rollback: subscription changes made here after the cutover (URL, rotated secret) are
/// pushed back to Calling — the secret only transfer-sealed for the rollback direction —
/// and the delivery switch rolls back: nothing is sent here any more, Calling's open
/// deliveries already delivered here are settled there.
#[tokio::test]
async fn the_rollback_round_trips_changes_and_settles_deliveries() {
    let Some(w) = World::new(settings(DeliveryMode::Active)).await else {
        return;
    };
    let s = seed(&w);
    w.hash_bodies().await;
    stop_calling(&w);
    w.op(Method::POST, "import", Some(json!({}))).await;
    assert_eq!(
        w.op(Method::POST, "takeover", None).await.status,
        StatusCode::OK
    );
    w.w.advance(3600);
    w.deliver().await;
    // Changes after the cutover (unfrozen here).
    let m = w.c.pa.machine(
        w.scope.2,
        &["webhooks.read", "webhooks.manage", "calls.read"],
    );
    let path = format!("/v1/webhooks/{}", s.active);
    let r =
        w.c.req(
            As::Machine(&m),
            Method::PATCH,
            &path,
            Some(json!({ "expectedVersion": 2, "url": "https://moved.example.com/hook" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r =
        w.c.req(
            As::Machine(&m),
            Method::POST,
            &format!("{path}/rotate-secret"),
            Some(json!({ "expectedVersion": 3 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let rotated = r.json["signingSecret"].as_str().unwrap().to_string();
    let r = w.op(Method::POST, "rollback-export", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["applied"], 1, "{}", r.json);
    let items = w.calling.with(|d| d.rolled_back.clone());
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["webhookId"], s.active.to_string());
    assert_eq!(item["version"], 4);
    assert_eq!(item["url"], "https://moved.example.com/hook");
    let sealed = item["secret"]["transferSealed"].as_str().unwrap();
    assert!(!item.to_string().contains(&rotated));
    assert_eq!(
        transfer()
            .open(sealed, &owner(ROLLBACK_PURPOSE, s.active))
            .unwrap(),
        rotated
    );
    assert!(transfer()
        .open(sealed, &owner(EXPORT_PURPOSE, s.active))
        .is_err());
    // Exported again: nothing new.
    let r = w.op(Method::POST, "rollback-export", None).await;
    assert_eq!(r.json["candidates"], 0, "{}", r.json);
    // Calling still has the two deliveries open (it stopped sending); the Developer
    // Platform delivered both: they are settled at Calling, and nothing more is sent here.
    let r = w.op(Method::POST, "rollback", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["phase"], "ROLLED_BACK");
    assert_eq!(r.json["completed"], true, "{}", r.json);
    let settled = w.calling.with(|d| d.settled.clone());
    for d in [&s.pending, &s.retrying] {
        assert!(
            settled.contains(&d["deliveryId"].as_str().unwrap().parse().unwrap()),
            "{settled:?}"
        );
    }
    let before = w.rx.seen().len();
    let fresh = format!("evt_{}", Uuid::new_v4().simple());
    w.c.internal(
        &w.w.producers.calling,
        Method::POST,
        "/internal/v1/platform/events",
        Some(call_requested(w.scope, &fresh)),
    )
    .await;
    w.deliver().await;
    assert_eq!(
        w.rx.seen().len(),
        before,
        "nothing is sent after the rollback"
    );
}

/// `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN`: customer and console mutations answer
/// 503 with `CF-Webhook-Migration: frozen`; reads continue.
#[tokio::test]
async fn the_freeze_refuses_mutations_and_keeps_reads() {
    let Some(w) = World::new(WebhookSettings {
        mutations_frozen: true,
        ..settings(DeliveryMode::Shadow)
    })
    .await
    else {
        return;
    };
    let s = seed(&w);
    w.hash_bodies().await;
    w.op(Method::POST, "import", Some(json!({}))).await;
    let m = w.c.pa.machine(
        w.scope.2,
        &["webhooks.read", "webhooks.manage", "calls.read"],
    );
    let path = format!("/v1/webhooks/{}", s.active);
    for (method, p, body) in [
        (
            Method::POST,
            "/v1/webhooks".to_string(),
            Some(json!({ "url": w.rx.url, "eventTypes": ["call.requested"] })),
        ),
        (
            Method::PATCH,
            path.clone(),
            Some(json!({ "expectedVersion": 2, "status": "DISABLED" })),
        ),
        (Method::DELETE, format!("{path}?expectedVersion=2"), None),
        (
            Method::POST,
            format!("{path}/rotate-secret"),
            Some(json!({ "expectedVersion": 2 })),
        ),
        (
            Method::POST,
            format!(
                "/v1/webhook-deliveries/{}/replay",
                s.succeeded["deliveryId"].as_str().unwrap()
            ),
            None,
        ),
    ] {
        let r = w.c.req(As::Machine(&m), method.clone(), &p, body).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {p}: {}",
            r.json
        );
        assert_eq!(r.headers["cf-webhook-migration"], "frozen");
        assert_eq!(r.json["error"]["code"], "SERVICE_UNAVAILABLE");
    }
    let r = w.c.req(As::Machine(&m), Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

/// The receiver's Retry-After (429 / 503) is honoured: the retry is scheduled no earlier.
#[tokio::test]
async fn a_receivers_retry_after_is_honoured() {
    let Some(w) = World::new(settings(DeliveryMode::Active)).await else {
        return;
    };
    let m = w.c.pa.machine(
        w.scope.2,
        &["webhooks.read", "webhooks.manage", "calls.read"],
    );
    let r =
        w.c.req(
            As::Machine(&m),
            Method::POST,
            "/v1/webhooks",
            Some(json!({ "url": w.rx.url, "eventTypes": ["call.requested"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    w.rx.state.lock().unwrap().script = vec![(429, Some("120"))];
    let src = format!("evt_{}", Uuid::new_v4().simple());
    w.c.internal(
        &w.w.producers.calling,
        Method::POST,
        "/internal/v1/platform/events",
        Some(call_requested(w.scope, &src)),
    )
    .await;
    let before = w.w.service.now();
    let report = w.deliver().await;
    assert_eq!(report.retried, 1, "{report:?}");
    let delay: i64 = w
        .one(&format!(
            "SELECT (extract(epoch FROM next_attempt_at - '{}'::timestamptz))::bigint::text FROM devplat_webhook_deliveries",
            before.to_rfc3339()
        ))
        .await
        .parse()
        .unwrap();
    assert!(delay >= 120, "next attempt in {delay}s");
}

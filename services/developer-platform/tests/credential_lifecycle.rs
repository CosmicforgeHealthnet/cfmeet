//! Blueprint Section 15 (PLT-008) — credential lifecycle webhooks through the REAL router
//! (subscriptions), the REAL webhook outbox and delivery worker, a real PostgreSQL
//! database and a local HTTP receiver. Platform Access is the in-memory fake.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use axum::{
    extract::State,
    http::{Method, StatusCode},
    routing::post,
    Router,
};
use bytes::Bytes;
use cf_service_auth::DelegatedPlatformContext;
use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
use chrono::{Duration, Utc};
use common::*;
use developer_platform::{
    control::credential_lifecycle::{
        self, sweep_once, CredentialLifecycleSettings, Notice, EXPIRED, EXPIRING_SOON,
    },
    webhooks::{
        catalog::{self, PRODUCER_DEVELOPER_PLATFORM},
        ingest::EnvelopeFacts,
        DeliveryMode, WebhookSettings,
    },
    worker,
};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const WH: &str = "platform-developer/webhooks/v1.schema.json#/$defs/";

#[derive(Default)]
struct ReceiverState {
    seen: Vec<Vec<u8>>,
    script: Vec<u16>,
}

async fn hook(State(state): State<Arc<Mutex<ReceiverState>>>, body: Bytes) -> StatusCode {
    let mut s = state.lock().unwrap();
    s.seen.push(body.to_vec());
    let status = if s.script.is_empty() {
        200
    } else {
        s.script.remove(0)
    };
    StatusCode::from_u16(status).unwrap()
}

struct Receiver {
    state: Arc<Mutex<ReceiverState>>,
    url: String,
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

    fn bodies(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .seen
            .iter()
            .map(|b| serde_json::from_slice(b).unwrap())
            .collect()
    }

    fn script(&self, statuses: &[u16]) {
        self.state.lock().unwrap().script = statuses.to_vec();
    }
}

fn settings() -> WebhookSettings {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = 9u8.wrapping_add((i as u8).wrapping_mul(7));
    }
    WebhookSettings {
        enabled: true,
        key_ring: Some(Arc::new(SecretBox::from_bytes(&k).unwrap())),
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

fn lifecycle() -> CredentialLifecycleSettings {
    CredentialLifecycleSettings::default()
}

type Scope3 = (Uuid, Uuid, Uuid);

struct World {
    c: Control,
    w: Webhooks,
    scope: Scope3,
    machine: DelegatedPlatformContext,
    sa: Uuid,
}

/// A sandbox environment with one service account.
async fn world() -> Option<World> {
    let (c, w) = Control::with_webhooks(settings()).await?;
    let scope = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    c.pa.envs.lock().unwrap().insert(
        scope.2,
        MemEnv {
            tenant: scope.0,
            app: scope.1,
            kind: "SANDBOX".into(),
            status: "ACTIVE".into(),
        },
    );
    let sa = service_account(&c, scope);
    let machine = c.pa.machine(
        scope.2,
        &["webhooks.read", "webhooks.manage", "credentials.manage"],
    );
    Some(World {
        c,
        w,
        scope,
        machine,
        sa,
    })
}

fn service_account(c: &Control, scope: Scope3) -> Uuid {
    let id = Uuid::new_v4();
    c.pa.service_accounts.lock().unwrap().insert(
        id,
        MemServiceAccount {
            tenant: scope.0,
            app: scope.1,
            env: scope.2,
            name: format!("sa-{}", &id.simple().to_string()[..8]),
            description: None,
            status: "ACTIVE".into(),
            created_at: Utc::now() - Duration::days(60),
        },
    );
    id
}

/// A credential of `sa` in `scope` expiring at `expires_at`.
fn credential(
    c: &Control,
    scope: Scope3,
    sa: Uuid,
    expires_at: chrono::DateTime<Utc>,
    revoked: bool,
    superseded: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    c.pa.credentials.lock().unwrap().insert(
        id,
        MemCredential {
            tenant: scope.0,
            app: scope.1,
            env: scope.2,
            sa,
            scopes: vec!["calls.read".into()],
            label: None,
            revoked,
            suspended: false,
            superseded_by: superseded.then(Uuid::new_v4),
            expires_at,
            created_at: Utc::now() - Duration::days(30),
        },
    );
    id
}

async fn subscribe(wd: &World, url: &str, types: &[&str]) -> String {
    let r =
        wd.c.req(
            As::Machine(&wd.machine),
            Method::POST,
            "/v1/webhooks",
            Some(json!({ "url": url, "eventTypes": types })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    r.json["webhook"]["webhookId"].as_str().unwrap().to_string()
}

async fn sweep(wd: &World) -> credential_lifecycle::SweepReport {
    sweep_once(
        &wd.w.service,
        wd.c.pa.as_ref(),
        &lifecycle(),
        wd.w.service.now(),
    )
    .await
    .unwrap()
}

async fn text(c: &Control, sql: &str) -> String {
    c.db.conn
        .query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

const DAY: i64 = 86_400;

/// Lead times: one notice per crossed threshold (only the smallest when several are
/// crossed at once), then `credential.expired` once; replaced / withdrawn credentials and
/// unsubscribed environments get nothing; every notice is audited and persisted.
#[tokio::test]
async fn expiry_thresholds_emit_one_notice_each_through_the_webhook_outbox() {
    let Some(wd) = world().await else {
        return;
    };
    let rx = Receiver::start().await;
    subscribe(
        &wd,
        &rx.url,
        &["credential.expiring_soon", "credential.expired"],
    )
    .await;
    let now = wd.w.service.now();
    let cred = credential(
        &wd.c,
        wd.scope,
        wd.sa,
        now + Duration::days(10),
        false,
        false,
    );
    // Not inside any lead time, revoked, superseded by rotation: no notice.
    credential(
        &wd.c,
        wd.scope,
        wd.sa,
        now + Duration::days(40),
        false,
        false,
    );
    credential(&wd.c, wd.scope, wd.sa, now + Duration::days(2), true, false);
    credential(&wd.c, wd.scope, wd.sa, now + Duration::days(2), false, true);
    // An environment with credentials but no lifecycle subscription is not swept.
    let other = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    wd.c.pa.envs.lock().unwrap().insert(
        other.2,
        MemEnv {
            tenant: other.0,
            app: other.1,
            kind: "SANDBOX".into(),
            status: "ACTIVE".into(),
        },
    );
    let other_sa = service_account(&wd.c, other);
    credential(
        &wd.c,
        other,
        other_sa,
        now + Duration::days(1),
        false,
        false,
    );

    let r = sweep(&wd).await;
    assert_eq!(
        (
            r.environments,
            r.examined,
            r.notified,
            r.deliveries,
            r.deduplicated
        ),
        (1, 4, 1, 1, 0),
        "{r:?}"
    );
    // The 14-day notice, persisted with its threshold and audited as a SERVICE event.
    assert_eq!(
        text(
            &wd.c,
            "SELECT event_type || ':' || threshold_days FROM devplat_credential_lifecycle_notices"
        )
        .await,
        "credential.expiring_soon:14"
    );
    assert_eq!(
        wd.c.count(&format!(
            "SELECT count(*) FROM devplat_activity_events WHERE tenant_id = '{}' AND actor_kind = 'SERVICE' \
               AND actor_ref = 'system:credential-lifecycle' AND action = 'credential.expiring_soon' \
               AND target_type = 'CREDENTIAL' AND target_id = '{cred}' AND outcome = 'SUCCEEDED' \
               AND summary ->> 'thresholdDays' = '14'",
            wd.scope.0
        ))
        .await,
        1
    );
    // Delivered signed through the outbox; the body is the public contract, ids only.
    assert_eq!(
        wd.w.service.deliver_due_once(100).await.unwrap().succeeded,
        1
    );
    let bodies = rx.bodies();
    assert_eq!(bodies.len(), 1);
    let e = &bodies[0];
    conforms(&format!("{WH}WebhookEvent"), e);
    assert_eq!(e["type"], "credential.expiring_soon");
    assert_eq!(e["data"]["credentialId"], json!(cred));
    assert_eq!(e["data"]["serviceAccountId"], json!(wd.sa));
    assert_eq!(e["data"]["environmentId"], json!(wd.scope.2));
    assert_eq!(e["data"]["status"], "ACTIVE");
    assert_eq!(e["data"]["thresholdDays"], 14);
    assert!(e["data"]["expiresAtEpochSeconds"].as_i64().unwrap() > now.timestamp());

    // Same threshold again: deduplicated, nothing new anywhere.
    let r = sweep(&wd).await;
    assert_eq!((r.notified, r.deduplicated), (0, 1), "{r:?}");
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        1
    );

    // Four days later (6 days left): the 7-day notice, once.
    wd.w.advance(4 * DAY);
    assert_eq!(sweep(&wd).await.notified, 1);
    assert_eq!(sweep(&wd).await.notified, 0);
    // Past the expiry: `credential.expired` once (threshold 0); the 1-day notice it
    // skipped is never sent afterwards.
    wd.w.advance(6 * DAY + 3600);
    assert_eq!(sweep(&wd).await.notified, 1);
    assert_eq!(sweep(&wd).await.notified, 0);
    assert_eq!(
        text(
            &wd.c,
            "SELECT string_agg(event_type || ':' || threshold_days, ',' ORDER BY notified_at) \
               FROM devplat_credential_lifecycle_notices"
        )
        .await,
        "credential.expiring_soon:14,credential.expiring_soon:7,credential.expired:0"
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_webhook_events WHERE event_type IN ('credential.expiring_soon', 'credential.expired')")
            .await,
        3
    );
    wd.w.service.deliver_due_once(100).await.unwrap();
    let mut types: Vec<String> = rx
        .bodies()
        .iter()
        .map(|b| format!("{}:{}", b["type"].as_str().unwrap(), b["data"]["status"]))
        .collect();
    types.sort();
    assert_eq!(
        types,
        [
            "credential.expired:\"EXPIRED\"",
            "credential.expiring_soon:\"ACTIVE\"",
            "credential.expiring_soon:\"ACTIVE\""
        ]
    );
    // Expired long ago (outside the lookback): never announced.
    let ancient = credential(
        &wd.c,
        wd.scope,
        wd.sa,
        wd.w.service.now() - Duration::days(30),
        false,
        false,
    );
    sweep(&wd).await;
    assert_eq!(
        wd.c.count(&format!(
            "SELECT count(*) FROM devplat_credential_lifecycle_notices WHERE credential_id = '{ancient}'"
        ))
        .await,
        0
    );
    // The notice table is append-only.
    assert!(wd
        .c
        .db
        .conn
        .execute_unprepared("DELETE FROM devplat_credential_lifecycle_notices")
        .await
        .is_err());
}

/// Concurrent workers emit a notice exactly once; a crash after the outbox write but
/// before the notice row is recovered without a second delivery.
#[tokio::test]
async fn notices_are_deduplicated_across_workers_and_crash_recovery() {
    let Some(wd) = world().await else {
        return;
    };
    let rx = Receiver::start().await;
    subscribe(&wd, &rx.url, &["credential.expiring_soon"]).await;
    let now = wd.w.service.now();
    for _ in 0..3 {
        credential(
            &wd.c,
            wd.scope,
            wd.sa,
            now + Duration::days(5),
            false,
            false,
        );
    }
    let (a, b) = tokio::join!(sweep(&wd), sweep(&wd));
    assert_eq!(a.notified + b.notified, 3, "{a:?} {b:?}");
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_credential_lifecycle_notices")
            .await,
        3
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_webhook_events")
            .await,
        3
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        3
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_activity_events WHERE actor_ref = 'system:credential-lifecycle'")
            .await,
        3
    );

    // Crash simulation: the outbox already holds the notice's event (same deterministic
    // source id), the notice row does not exist.
    let expires_at = now + Duration::days(3);
    let cred = credential(&wd.c, wd.scope, wd.sa, expires_at, false, false);
    let view = wd.c.pa.credentials.lock().unwrap()[&cred].clone();
    let exp = view.expires_at;
    let notice: Notice =
        credential_lifecycle::classify("ACTIVE", exp, Some(view.created_at), &lifecycle(), now)
            .unwrap();
    assert_eq!(notice.threshold_days, 7);
    let envelope = json!({
        "event_type": "developer-platform.credential.expiring-soon",
        "payload": {
            "credential_id": cred, "service_account_id": wd.sa, "environment_id": wd.scope.2,
            "status": "ACTIVE", "threshold_days": notice.threshold_days,
            "expires_at_epoch_seconds": exp.timestamp(),
        }
    });
    let event = catalog::project(PRODUCER_DEVELOPER_PLATFORM, &envelope)
        .unwrap()
        .unwrap();
    let first =
        wd.w.service
            .ingest_public_event(
                PRODUCER_DEVELOPER_PLATFORM,
                &EnvelopeFacts {
                    source_event_id: credential_lifecycle::source_event_id(cred, &notice, exp),
                    scope: wd.scope,
                    occurred_at: notice.occurred_at,
                    correlation_id: None,
                },
                &event,
            )
            .await
            .unwrap();
    assert_eq!(first.deliveries, 1);
    let r = sweep(&wd).await;
    assert_eq!(
        (r.notified, r.deliveries),
        (1, 0),
        "recorded, not re-delivered: {r:?}"
    );
    assert_eq!(
        text(
            &wd.c,
            &format!(
                "SELECT webhook_event_id FROM devplat_credential_lifecycle_notices WHERE credential_id = '{cred}'"
            )
        )
        .await,
        first.event_id.unwrap()
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_webhook_deliveries")
            .await,
        4
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_event_rejections")
            .await,
        0,
        "the re-publication is the same event, not a conflicting replay"
    );
}

/// Platform Access outages leave nothing recorded and are retried by the next sweep;
/// failed deliveries of a notice are retried by the webhook outbox until delivered.
#[tokio::test]
async fn outages_and_failed_deliveries_are_retried() {
    let Some(wd) = world().await else {
        return;
    };
    let rx = Receiver::start().await;
    subscribe(&wd, &rx.url, &["credential.expired"]).await;
    let now = wd.w.service.now();
    credential(
        &wd.c,
        wd.scope,
        wd.sa,
        now - Duration::hours(2),
        false,
        false,
    );
    wd.c.pa
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r = sweep(&wd).await;
    assert_eq!((r.unavailable, r.notified), (1, 0), "{r:?}");
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_credential_lifecycle_notices")
            .await,
        0
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_webhook_events")
            .await,
        0
    );
    wd.c.pa
        .down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(sweep(&wd).await.notified, 1);
    // The endpoint fails twice, then accepts: the outbox retries with backoff.
    rx.script(&[503, 500]);
    let delivery = text(&wd.c, "SELECT id::text FROM devplat_webhook_deliveries").await;
    let state = || async {
        text(
            &wd.c,
            &format!(
                "SELECT status || ':' || attempt_count FROM devplat_webhook_deliveries WHERE id = '{delivery}'"
            ),
        )
        .await
    };
    assert_eq!(wd.w.service.deliver_due_once(100).await.unwrap().retried, 1);
    assert_eq!(state().await, "RETRY_SCHEDULED:1");
    wd.w.advance(7 * 3600);
    wd.w.service.deliver_due_once(100).await.unwrap();
    assert_eq!(state().await, "RETRY_SCHEDULED:2");
    wd.w.advance(7 * 3600);
    assert_eq!(
        wd.w.service.deliver_due_once(100).await.unwrap().succeeded,
        1
    );
    assert_eq!(state().await, "SUCCEEDED:3");
    let bodies = rx.bodies();
    assert_eq!(bodies.len(), 3);
    assert!(
        bodies.iter().all(|b| b == &bodies[0]),
        "same event every attempt"
    );
    assert_eq!(bodies[0]["type"], "credential.expired");
    // Retries never produce a second notice.
    assert_eq!(sweep(&wd).await.notified, 0);
}

/// Subscribing to lifecycle events needs `credentials.manage` besides `webhooks.manage`
/// (catalog scope); the worker runs the sweep at most once per interval across
/// instances, and not at all when disabled.
#[tokio::test]
async fn subscriptions_need_the_credentials_scope_and_the_worker_sweeps_once_per_interval() {
    let Some(wd) = world().await else {
        return;
    };
    let manage_only = wd.c.pa.machine(wd.scope.2, &["webhooks.manage"]);
    for t in [EXPIRING_SOON, EXPIRED] {
        let r =
            wd.c.req(
                As::Machine(&manage_only),
                Method::POST,
                "/v1/webhooks",
                Some(json!({ "url": "http://127.0.0.1:9/h", "eventTypes": [t] })),
            )
            .await;
        assert_eq!(
            (r.status, r.json["error"]["code"].clone()),
            (StatusCode::FORBIDDEN, json!("INSUFFICIENT_SCOPE")),
            "{t}"
        );
    }
    let rx = Receiver::start().await;
    subscribe(&wd, &rx.url, &[EXPIRING_SOON]).await;
    let now = wd.w.service.now();
    credential(
        &wd.c,
        wd.scope,
        wd.sa,
        now + Duration::days(1),
        false,
        false,
    );
    let off = CredentialLifecycleSettings::disabled();
    assert!(
        worker::credential_lifecycle_pass(&wd.w.service, wd.c.pa.as_ref(), &off)
            .await
            .is_none()
    );
    assert_eq!(
        wd.c.count("SELECT count(*) FROM devplat_worker_schedules")
            .await,
        0
    );
    let on = lifecycle();
    let (a, b) = tokio::join!(
        worker::credential_lifecycle_pass(&wd.w.service, wd.c.pa.as_ref(), &on),
        worker::credential_lifecycle_pass(&wd.w.service, wd.c.pa.as_ref(), &on),
    );
    assert_eq!(a.iter().chain(b.iter()).map(|r| r.notified).sum::<u64>(), 1);
    assert!(a.is_some() != b.is_some(), "exactly one instance swept");
    assert_eq!(
        text(
            &wd.c,
            "SELECT last_outcome FROM devplat_worker_schedules WHERE job = 'credential-lifecycle'"
        )
        .await,
        "SUCCEEDED"
    );
    // Not due before the interval; due after it.
    credential(
        &wd.c,
        wd.scope,
        wd.sa,
        now + Duration::hours(30),
        false,
        false,
    );
    assert!(
        worker::credential_lifecycle_pass(&wd.w.service, wd.c.pa.as_ref(), &on)
            .await
            .is_none()
    );
    wd.w.advance(3601);
    let r = worker::credential_lifecycle_pass(&wd.w.service, wd.c.pa.as_ref(), &on)
        .await
        .unwrap();
    assert_eq!(r.notified, 1);
    // The full worker pass runs the due sweep before delivering.
    wd.w.advance(3601);
    credential(
        &wd.c,
        wd.scope,
        wd.sa,
        now + Duration::hours(40),
        false,
        false,
    );
    worker::run_pass_with_jobs(
        &wd.c.db.conn,
        "worker-lifecycle",
        "test",
        Some(&wd.w.service),
        Some(wd.c.pa.as_ref()),
        &worker::PeriodicJobs {
            credential_lifecycle: on,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rx.bodies().len(), 3, "all three notices delivered");
}

//! §108: high-volume relay / context access through the real internal HTTP surface
//! (signed service tokens, the real router, PostgreSQL), measured and checked for
//! correctness. Throughput and latency are REPORTED, never asserted (no flaky
//! thresholds); every scenario asserts exact correctness.
//!
//! Each scenario prints one line:
//!
//! ```text
//! LOAD <scenario> env=<pgMAJOR-cpuN> duration_ms=… concurrency=… ops=… throughput_per_s=…
//!      p50_ms=… p95_ms=… p99_ms=… errors=… error_rate=… max_db_connections=… lock_waits=…
//! ```
//!
//! `max_db_connections` is the highest number of backends connected to the test
//! database seen in `pg_stat_activity` samples (every 10 ms, from a separate admin
//! connection); `lock_waits` is the number of not-granted locks observed in `pg_locks`
//! samples of the test database (summed over samples: 0 means no sample ever caught a
//! waiter).
//!
//! Scenarios and documented targets (reference: 4 vCPU runner, PostgreSQL 16, debug
//! build, pool of 20 connections; targets are guidance for regressions, not assertions):
//!
//! | scenario | work | target |
//! |---|---|---|
//! | `context_session_create` | create a context + its session (2 commands) | ≥ 30 ops/s, p99 < 2 s |
//! | `protected_authorization` | concurrent PUBLIC_RELAY decisions about a protected contact | ≥ 75 ops/s, p99 < 1.5 s |
//! | `relay_number_allocation_contention` | 64 sessions race for an 8-number pool | exactly 8 assignments, 0 errors |
//! | `outbox_drain` | 4 publishers drain every committed event | ≥ 500 events/s, each exactly once |

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind;
use common::*;
use privacy_relay::events::EventPublisher;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const CONTACT: &str = "+44 20 7946 0123";

struct Stats {
    duration_ms: u128,
    concurrency: usize,
    ops: usize,
    latencies_ms: Vec<f64>,
    errors: usize,
    max_db_connections: i64,
    lock_waits: i64,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn report(scenario: &str, env: &str, mut s: Stats) {
    s.latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let secs = (s.duration_ms as f64 / 1000.0).max(0.001);
    println!(
        "LOAD {scenario} env={env} duration_ms={} concurrency={} ops={} throughput_per_s={:.1} \
         p50_ms={:.1} p95_ms={:.1} p99_ms={:.1} errors={} error_rate={:.4} max_db_connections={} lock_waits={}",
        s.duration_ms,
        s.concurrency,
        s.ops,
        s.ops as f64 / secs,
        percentile(&s.latencies_ms, 50.0),
        percentile(&s.latencies_ms, 95.0),
        percentile(&s.latencies_ms, 99.0),
        s.errors,
        if s.ops == 0 { 0.0 } else { s.errors as f64 / s.ops as f64 },
        s.max_db_connections,
        s.lock_waits,
    );
}

/// Samples connection usage and lock waits of `dbname` until `done`.
async fn sample(admin: DatabaseConnection, dbname: String, done: Arc<AtomicBool>) -> (i64, i64) {
    let (mut max_conn, mut waits) = (0i64, 0i64);
    let q = |sql: String| Statement::from_string(sea_orm::DbBackend::Postgres, sql);
    loop {
        let finished = done.load(Ordering::SeqCst);
        if let Ok(Some(row)) = admin
            .query_one(q(format!(
                "SELECT (SELECT count(*) FROM pg_stat_activity WHERE datname = '{dbname}')::bigint, \
                        (SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
                          WHERE NOT l.granted AND a.datname = '{dbname}')::bigint"
            )))
            .await
        {
            max_conn = max_conn.max(row.try_get_by_index::<i64>(0).unwrap_or(0));
            waits += row.try_get_by_index::<i64>(1).unwrap_or(0);
        }
        if finished {
            return (max_conn, waits);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Runs `ops` operations with `concurrency` workers; `op(i)` returns whether it was a
/// correct outcome. Latency is measured per operation.
async fn run<F, Fut>(h: &Harness, concurrency: usize, ops: usize, op: F) -> Stats
where
    F: Fn(usize) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    let admin = Database::connect(admin_url().unwrap()).await.unwrap();
    let dbname = h.db.url.rsplit('/').next().unwrap().to_string();
    let done = Arc::new(AtomicBool::new(false));
    let sampler = tokio::spawn(sample(admin, dbname, done.clone()));
    let next = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let latencies = Arc::new(Mutex::new(Vec::with_capacity(ops)));
    let op = Arc::new(op);
    let start = Instant::now();
    let mut workers = Vec::new();
    for _ in 0..concurrency {
        let (next, errors, latencies, op) =
            (next.clone(), errors.clone(), latencies.clone(), op.clone());
        workers.push(tokio::spawn(async move {
            loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= ops {
                    break;
                }
                let t = Instant::now();
                let ok = op(i).await;
                latencies
                    .lock()
                    .unwrap()
                    .push(t.elapsed().as_secs_f64() * 1000.0);
                if !ok {
                    errors.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }
    for w in workers {
        w.await.unwrap();
    }
    let duration_ms = start.elapsed().as_millis();
    done.store(true, Ordering::SeqCst);
    let (max_db_connections, lock_waits) = sampler.await.unwrap();
    let latencies_ms = latencies.lock().unwrap().clone();
    Stats {
        duration_ms,
        concurrency,
        ops,
        latencies_ms,
        errors: errors.load(Ordering::SeqCst),
        max_db_connections,
        lock_waits,
    }
}

async fn environment(h: &Harness) -> String {
    let version = scalar(
        h.conn(),
        "SELECT (current_setting('server_version_num')::int / 10000)::bigint",
    )
    .await;
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    format!("pg{version}-cpu{cpus}")
}

/// A verified protected contact and a CALLBACK + PUBLIC relay context / session with it.
async fn protected_session(h: &Harness, c: &Consumer, contact: &str) -> (Uuid, String) {
    let principal = Uuid::new_v4();
    let mut req = context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"]);
    let ctx = h.create_context(c, req).await;
    let session = h
        .create_session(
            c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    (principal, session)
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

#[async_trait]
impl EventPublisher for Recorder {
    async fn publish(&self, _: &str, envelope: &Value) -> Result<(), &'static str> {
        self.0
            .lock()
            .unwrap()
            .push(envelope["event_id"].as_str().unwrap().to_string());
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_access_under_load_is_correct_and_measured() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let h = Arc::new(h);
    let env = environment(&h).await;
    let c = Consumer::new(ApplicationEnvironmentKind::Production);

    // 1. Context + session creation throughput.
    let (concurrency, ops) = (16, 400);
    let hh = h.clone();
    let cc = c.clone();
    let stats = run(&h, concurrency, ops, move |_| {
        let (h, c) = (hh.clone(), cc.clone());
        async move {
            let r = h
                .command(
                    "/internal/v1/contexts",
                    &c,
                    context_request(&principals(2), "BIDIRECTIONAL", &[]),
                )
                .await;
            if r.status != StatusCode::CREATED {
                return false;
            }
            let id = r.json["context"]["contextId"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let s = h
                .command(
                    &format!("/internal/v1/contexts/{id}/sessions"),
                    &c,
                    json!({ "relayStrategy": "APP_ONLY" }),
                )
                .await;
            s.status == StatusCode::CREATED
        }
    })
    .await;
    assert_eq!(stats.errors, 0);
    assert_eq!(
        h.count("SELECT count(*) FROM communication_contexts").await,
        ops as i64
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_sessions").await,
        ops as i64
    );
    report("context_session_create", &env, stats);

    // 2. Protected authorization decisions under concurrency (one hot context).
    let contact = h.create_contact(&c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &contact, "VERIFIED", None).await;
    let (principal, session) = protected_session(&h, &c, &contact).await;
    let (concurrency, ops) = (32, 1000);
    let (hh, cc, ss, ct) = (h.clone(), c.clone(), session.clone(), contact.clone());
    let stats = run(&h, concurrency, ops, move |_| {
        let (h, c, session, contact) = (hh.clone(), cc.clone(), ss.clone(), ct.clone());
        async move {
            let r = h
                .post(
                    "/internal/v1/communication/authorize",
                    &c,
                    json!({ "relaySessionId": session, "source": { "principalId": principal },
                            "targets": [{ "contactReferenceId": contact }], "channel": "VOICE",
                            "route": "PUBLIC_RELAY", "purpose": "CALL_CREATE" }),
                )
                .await;
            r.status == StatusCode::OK && r.json["decision"] == "AUTHORIZED"
        }
    })
    .await;
    assert_eq!(stats.errors, 0);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM relay_authorization_decisions WHERE session_id = '{session}' AND decision = 'AUTHORIZED'"
        ))
        .await,
        ops as i64,
        "every decision recorded once"
    );
    report("protected_authorization", &env, stats);

    // 3. Relay-number allocation contention: pool smaller than demand.
    let (pool, demand) = (8usize, 64usize);
    for i in 0..pool {
        h.add_relay_number(
            &format!("+44 20 7946 {:04}", 600 + i),
            &format!("pb-load-{i}"),
        )
        .await;
    }
    let mut sessions = Vec::new();
    for _ in 0..demand {
        sessions.push(protected_session(&h, &c, &contact).await.1);
    }
    let sessions = Arc::new(sessions);
    let outcomes = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
    let (hh, cc, ss, oo) = (h.clone(), c.clone(), sessions.clone(), outcomes.clone());
    let stats = run(&h, demand, demand, move |i| {
        let (h, c, sessions, outcomes) = (hh.clone(), cc.clone(), ss.clone(), oo.clone());
        async move {
            let r = h.assign_relay_number(&c, &sessions[i]).await;
            let key = match (r.status, r.code()) {
                (StatusCode::CREATED, _) => "ASSIGNED".to_string(),
                (StatusCode::CONFLICT, "RELAY_NUMBER_UNAVAILABLE") => "UNAVAILABLE".to_string(),
                (s, code) => format!("{s}:{code}"),
            };
            let ok = key == "ASSIGNED" || key == "UNAVAILABLE";
            *outcomes.lock().unwrap().entry(key).or_default() += 1;
            ok
        }
    })
    .await;
    let outcomes = outcomes.lock().unwrap().clone();
    assert_eq!(stats.errors, 0, "{outcomes:?}");
    assert_eq!(
        outcomes,
        BTreeMap::from([
            ("ASSIGNED".to_string(), pool),
            ("UNAVAILABLE".to_string(), demand - pool)
        ]),
        "exactly-once allocation"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments WHERE status = 'ACTIVE'")
            .await,
        pool as i64
    );
    assert_eq!(
        h.count("SELECT count(DISTINCT relay_number_id) FROM relay_number_assignments WHERE status = 'ACTIVE'")
            .await,
        pool as i64,
        "no number lent twice"
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM relay_number_inventory WHERE operational_status <> 'ASSIGNED'"
        )
        .await,
        0
    );
    report("relay_number_allocation_contention", &env, stats);

    // 4. Outbox drain: every committed event of the scenarios above, exactly once.
    let pending = h
        .count("SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING'")
        .await as usize;
    assert!(pending >= 2 * 400, "{pending}");
    let recorder = Arc::new(Recorder::default());
    let publishers: Arc<Vec<_>> = Arc::new(
        (0..4)
            .map(|_| h.sibling().with_event_publisher(recorder.clone()))
            .collect(),
    );
    let drained = Arc::new(AtomicUsize::new(0));
    // Each "op" is one publication batch of up to 100 events by one of 4 instances;
    // ops = batches until the backlog is empty (reported as events below).
    let batches = pending.div_ceil(100) + 4;
    let (pp, dd) = (publishers.clone(), drained.clone());
    let mut stats = run(&h, 4, batches, move |i| {
        let (publishers, drained) = (pp.clone(), dd.clone());
        async move {
            match publishers[i % 4].publish_outbox(100).await {
                Ok(n) => {
                    drained.fetch_add(n, Ordering::SeqCst);
                    true
                }
                Err(_) => false,
            }
        }
    })
    .await;
    assert_eq!(stats.errors, 0);
    let published = recorder.0.lock().unwrap().clone();
    let distinct: BTreeSet<&String> = published.iter().collect();
    assert_eq!(drained.load(Ordering::SeqCst), pending);
    assert_eq!(
        (published.len(), distinct.len()),
        (pending, pending),
        "exactly once"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_outbox WHERE status <> 'PUBLISHED'")
            .await,
        0
    );
    // Report events (not batches) as the unit of work; latency is per batch.
    stats.ops = pending;
    report("outbox_drain", &env, stats);
}

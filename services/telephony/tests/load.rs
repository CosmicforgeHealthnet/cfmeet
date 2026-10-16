//! Load / contention measurement (Part E §212, Part I §108) against PostgreSQL with the
//! deterministic interconnect: command intake and execution (two workers), provider
//! event ingestion and a full duplicate replay, and reconciliation batches with two
//! instances — plus, where the local SIP certification edge exists (Asterisk + SIPp),
//! a modest real SIP signalling scenario. Sizes are modest so the suite stays fast; the
//! invariants — one paid effect per command, one record per provider event, one repair
//! per leg, no outbox row from a duplicate — are asserted.
//!
//! Every scenario prints one line:
//!
//! ```text
//! LOAD <scenario> env=<label/pgVERSION/nprocN> duration_ms= concurrency= ops= throughput_per_s=
//!      p50_ms= p95_ms= p99_ms= errors= error_rate= max_db_connections= lock_waits= worker_lag_ms=
//! ```
//!
//! Latency percentiles are per operation (request, event, leg). `max_db_connections`
//! is the most backends seen on the test database, `lock_waits` the sum of ungranted
//! locks over all samples (pg_stat_activity / pg_locks sampled every 10 ms during the
//! scenario). `worker_lag_ms` is the longest a unit of durable work waited for a worker:
//! a command for its execution, an ingested event for its delivery to Calling, a due
//! leg for its repair, a SIP leg for its execution — 0 where no worker is involved.
//! `env` is `LOAD_ENV` (default `local`) with the PostgreSQL version and CPU count.
//!
//! The SIP scenario is gated like `sip_certification.rs`: without `asterisk` / `sipp` it
//! prints SKIPPED, unless `TELEPHONY_SIP_CERT_REQUIRED=1`, which makes it fail.

mod common;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use axum::http::{Method, StatusCode};
use common::Harness;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::json;
use telephony::domain::{EndReason, NormalizedEvent};
use uuid::Uuid;

const LEGS: usize = 200;

// ---------------------------------------------------------------------------------------
// Measurement.
// ---------------------------------------------------------------------------------------

/// Samples the test database's backends and lock waits in the background.
struct DbSampler {
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<(i64, i64)>>,
    task: tokio::task::JoinHandle<()>,
}

impl DbSampler {
    async fn start(url: &str) -> Self {
        let mut options = sea_orm::ConnectOptions::new(url.to_string());
        options.max_connections(1).sqlx_logging(false);
        let conn: DatabaseConnection = Database::connect(options).await.unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new((0_i64, 0_i64)));
        let (s, st) = (stop.clone(), stats.clone());
        let task = tokio::spawn(async move {
            while !s.load(Ordering::SeqCst) {
                if let Ok(Some(row)) = conn
                    .query_one(Statement::from_string(
                        sea_orm::DbBackend::Postgres,
                        "SELECT (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
                                   AND pid <> pg_backend_pid())::bigint, \
                                (SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
                                   WHERE NOT l.granted AND a.datname = current_database())::bigint"
                            .to_string(),
                    ))
                    .await
                {
                    let backends: i64 = row.try_get_by_index(0).unwrap_or(0);
                    let waits: i64 = row.try_get_by_index(1).unwrap_or(0);
                    let mut g = st.lock().unwrap();
                    g.0 = g.0.max(backends);
                    g.1 += waits;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            conn.close().await.ok();
        });
        Self { stop, stats, task }
    }

    /// The (max backends, lock waits) since the last call, and a fresh window.
    fn take(&self) -> (i64, i64) {
        std::mem::take(&mut *self.stats.lock().unwrap())
    }

    async fn finish(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.task.await;
    }
}

struct Scenario {
    name: &'static str,
    concurrency: usize,
    duration: Duration,
    latencies_ms: Vec<f64>,
    errors: usize,
    worker_lag_ms: f64,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

impl Scenario {
    fn print(mut self, env: &str, sampler: &DbSampler) {
        let (max_db_connections, lock_waits) = sampler.take();
        self.latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let ops = self.latencies_ms.len();
        let secs = self.duration.as_secs_f64().max(1e-9);
        println!(
            "LOAD {} env={env} duration_ms={:.0} concurrency={} ops={ops} throughput_per_s={:.1} \
             p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} errors={} error_rate={:.4} \
             max_db_connections={max_db_connections} lock_waits={lock_waits} worker_lag_ms={:.0}",
            self.name,
            self.duration.as_secs_f64() * 1000.0,
            self.concurrency,
            ops as f64 / secs,
            percentile(&self.latencies_ms, 0.50),
            percentile(&self.latencies_ms, 0.95),
            percentile(&self.latencies_ms, 0.99),
            self.errors,
            if ops == 0 {
                0.0
            } else {
                self.errors as f64 / ops as f64
            },
            self.worker_lag_ms,
        );
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

async fn environment(h: &Harness) -> String {
    let version = h
        .text("SELECT current_setting('server_version')")
        .await
        .unwrap_or_default();
    let version: String = version
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string();
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let label = std::env::var("LOAD_ENV").unwrap_or_else(|_| "local".into());
    format!("{label}/pg{version}/nproc{cpus}")
}

/// Leg ids whose ORIGINATE left PENDING / CLAIMED (it executed).
async fn executed_legs(h: &Harness) -> Vec<Uuid> {
    let rows =
        h.db.conn
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT telephony_leg_id FROM telephony_commands WHERE kind = 'ORIGINATE' \
             AND status NOT IN ('PENDING', 'CLAIMED', 'RUNNING')"
                    .to_string(),
            ))
            .await
            .unwrap();
    rows.into_iter()
        .map(|r| r.try_get_by_index::<Uuid>(0).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_event_and_reconciliation_throughput() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let env = environment(&h).await;
    let sampler = DbSampler::start(&h.db.url).await;
    // Raise the routes' configured capacity for the run.
    h.sql("UPDATE telecom_routes SET max_concurrent_legs = 1000")
        .await;
    h.sql("UPDATE telecom_providers SET max_concurrent_legs = 1000")
        .await;

    // 1. Durable intake: LEGS originate commands, 50 in flight at a time.
    let bodies: Vec<_> = (0..LEGS)
        .map(|_| {
            let contact = h.privacy.add_contact("+2348031234567", "NG");
            h.originate_body(contact, Uuid::new_v4(), 1)
        })
        .collect();
    sampler.take();
    let started = Instant::now();
    let mut legs = Vec::new();
    let mut latencies = Vec::new();
    let mut errors = 0;
    for chunk in bodies.chunks(50) {
        let rs = futures::future::join_all(chunk.iter().map(|b| async {
            let t = Instant::now();
            let r = h
                .calling(Method::POST, "/internal/v1/legs/originate", Some(b.clone()))
                .await;
            (ms(t.elapsed()), r)
        }))
        .await;
        for (latency, r) in rs {
            latencies.push(latency);
            if r.status != StatusCode::ACCEPTED {
                errors += 1;
            }
            assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
            legs.push(
                r.json["leg"]["telephonyLegId"]
                    .as_str()
                    .unwrap()
                    .parse::<Uuid>()
                    .unwrap(),
            );
        }
    }
    Scenario {
        name: "leg_setup_intake",
        concurrency: 50,
        duration: started.elapsed(),
        latencies_ms: latencies,
        errors,
        worker_lag_ms: 0.0,
    }
    .print(&env, &sampler);

    // Execution by two worker instances.
    let other = h.svc.clone().as_instance("w-load-2");
    let started = Instant::now();
    let mut done: HashMap<Uuid, f64> = HashMap::new();
    let mut errors = 0;
    while h.provider_a.requests().len() + h.provider_b.requests().len() < LEGS {
        let (a, b) = tokio::join!(h.svc.run_commands_once(10), other.run_commands_once(10));
        errors += usize::from(a.is_err()) + usize::from(b.is_err());
        a.unwrap();
        b.unwrap();
        let at = ms(started.elapsed());
        for leg in executed_legs(&h).await {
            done.entry(leg).or_insert(at);
        }
        assert!(started.elapsed().as_secs() < 120, "execution stalled");
    }
    let execution = started.elapsed();
    let effects = h.provider_a.effects().len() + h.provider_b.effects().len();
    assert_eq!(effects, LEGS, "exactly one paid effect per command");
    for leg in executed_legs(&h).await {
        done.entry(leg).or_insert(ms(execution));
    }
    let latencies: Vec<f64> = done.values().copied().collect();
    let lag = latencies.iter().copied().fold(0.0, f64::max);
    Scenario {
        name: "command_execution",
        concurrency: 2,
        duration: execution,
        latencies_ms: latencies,
        errors,
        worker_lag_ms: lag,
    }
    .print(&env, &sampler);

    // 2. Provider event ingestion: RINGING / ANSWERED / CONNECTED for every leg, then a
    // full duplicate replay of all of them.
    let mut events = Vec::new();
    for leg in &legs {
        let correlation = h.correlation_of(*leg).await;
        let provider = if h.provider_a.call(&correlation).is_some() {
            "dp-a"
        } else {
            "dp-b"
        };
        let p = if provider == "dp-a" {
            &h.provider_a
        } else {
            &h.provider_b
        };
        for e in [
            NormalizedEvent::LegRinging,
            NormalizedEvent::LegAnswered,
            NormalizedEvent::LegConnected,
        ] {
            events.push((provider, p.progress(&correlation, e, h.now())));
        }
    }
    // The executions' own events are delivered first (a clean outbox).
    while h.svc.deliver_outbox_once(500).await.unwrap().0 > 0 {}
    sampler.take();
    let started = Instant::now();
    let mut latencies = Vec::new();
    let mut errors = 0;
    for chunk in events.chunks(60) {
        let rs = futures::future::join_all(chunk.iter().map(|(p, e)| async {
            let t = Instant::now();
            let r = h.svc.ingest_provider_event(p, e.clone()).await;
            (ms(t.elapsed()), r)
        }))
        .await;
        for (latency, r) in rs {
            latencies.push(latency);
            errors += usize::from(r.is_err());
            r.unwrap();
        }
    }
    let ingest = started.elapsed();
    // Worker lag: the ingested events' delivery to Calling by two outbox workers.
    let drain = Instant::now();
    loop {
        let (a, b) = tokio::join!(
            h.svc.deliver_outbox_once(100),
            other.deliver_outbox_once(100)
        );
        if a.unwrap().0 + b.unwrap().0 == 0
            && h.scalar("SELECT count(*) FROM telephony_outbox WHERE delivered_at IS NULL")
                .await
                == 0
        {
            break;
        }
        assert!(drain.elapsed().as_secs() < 120, "outbox drain stalled");
    }
    let outbox_lag = ms(started.elapsed());
    Scenario {
        name: "provider_event_ingestion",
        concurrency: 60,
        duration: ingest,
        latencies_ms: latencies,
        errors,
        worker_lag_ms: outbox_lag,
    }
    .print(&env, &sampler);

    let outbox_rows = h.scalar("SELECT count(*) FROM telephony_outbox").await;
    let started = Instant::now();
    let mut latencies = Vec::new();
    let mut errors = 0;
    for chunk in events.chunks(60) {
        let rs = futures::future::join_all(chunk.iter().map(|(p, e)| async {
            let t = Instant::now();
            let r = h.svc.ingest_provider_event(p, e.clone()).await;
            (ms(t.elapsed()), r)
        }))
        .await;
        for (latency, r) in rs {
            latencies.push(latency);
            let duplicate = r.as_ref().is_ok_and(|d| d.as_str() == "DUPLICATE");
            errors += usize::from(!duplicate);
            assert_eq!(r.unwrap().as_str(), "DUPLICATE");
        }
    }
    let replay = started.elapsed();
    assert_eq!(
        h.scalar("SELECT count(*) FROM provider_events").await,
        events.len() as i64,
        "one record per provider event"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_outbox").await,
        outbox_rows,
        "a duplicate never produces an event for Calling"
    );
    Scenario {
        name: "provider_event_duplicate_dedupe",
        concurrency: 60,
        duration: replay,
        latencies_ms: latencies,
        errors,
        worker_lag_ms: 0.0,
    }
    .print(&env, &sampler);

    // 3. Reconciliation: every provider call ends silently; two reconcilers repair.
    for leg in &legs {
        let correlation = h.correlation_of(*leg).await;
        h.provider_a
            .end_silently(&correlation, EndReason::Completed);
        h.provider_b
            .end_silently(&correlation, EndReason::Completed);
    }
    h.advance(601);
    sampler.take();
    let started = Instant::now();
    let mut rounds = 0;
    let mut errors = 0;
    let mut repaired: HashMap<Uuid, f64> = HashMap::new();
    loop {
        let (a, b) = tokio::join!(h.svc.reconcile_once(50), other.reconcile_once(50));
        errors += usize::from(a.is_err()) + usize::from(b.is_err());
        let n = a.unwrap() + b.unwrap();
        rounds += 1;
        let at = ms(started.elapsed());
        let ended =
            h.db.conn
                .query_all(Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    "SELECT id FROM telephony_legs WHERE status = 'ENDED'".to_string(),
                ))
                .await
                .unwrap();
        for row in ended {
            repaired
                .entry(row.try_get_by_index::<Uuid>(0).unwrap())
                .or_insert(at);
        }
        if n == 0 {
            break;
        }
        assert!(rounds < 100, "reconciliation did not converge");
    }
    let reconcile = started.elapsed();
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_legs WHERE status = 'ENDED'")
            .await,
        LEGS as i64
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_reconciliation_runs WHERE action = 'MARKED_ENDED'"
        )
        .await,
        LEGS as i64,
        "exactly one effective repair per leg"
    );
    let latencies: Vec<f64> = repaired.values().copied().collect();
    let lag = latencies.iter().copied().fold(0.0, f64::max);
    Scenario {
        name: "reconciliation",
        concurrency: 2,
        duration: reconcile,
        latencies_ms: latencies,
        errors,
        worker_lag_ms: lag,
    }
    .print(&env, &sampler);
    sampler.finish().await;
}

// ---------------------------------------------------------------------------------------
// SIP signalling load on the local certification edge (Asterisk + SIPp).
// ---------------------------------------------------------------------------------------

const SIP_CALLS: usize = 20;
const ARI_URL: &str = "http://127.0.0.1:8088";
const ARI_USER: &str = "cfmeet-cert";
const ARI_PASSWORD: &str = "certification-only-ari-password-0123456789";

fn cert_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../infra/telephony/certification")
        .canonicalize()
        .unwrap()
}

fn available(binary: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {binary}")])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A child process killed on drop (Asterisk, SIPp).
struct Proc(Child, PathBuf);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn start_asterisk() -> Proc {
    let run = std::env::temp_dir().join(format!("cfmeet-sip-load-{}", Uuid::new_v4().simple()));
    for d in ["etc", "varlib", "spool", "run", "log", "agi", "moh"] {
        std::fs::create_dir_all(run.join(d)).unwrap();
    }
    std::fs::write(run.join("moh/hold-tone.sln"), vec![0u8; 16000]).unwrap();
    for entry in std::fs::read_dir(cert_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "conf") {
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("@RUNDIR@", run.to_str().unwrap());
            std::fs::write(run.join("etc").join(path.file_name().unwrap()), text).unwrap();
        }
    }
    let log = std::fs::File::create(run.join("console.log")).unwrap();
    let child = Command::new("asterisk")
        .args([
            "-C",
            run.join("etc/asterisk.conf").to_str().unwrap(),
            "-f",
            "-n",
        ])
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .expect("asterisk starts");
    Proc(child, run)
}

/// The simulated carrier A (UAS: 180, 200, waits for BYE) for `calls` calls at once.
fn start_carrier(calls: usize) -> Proc {
    let dir = std::env::temp_dir().join(format!("cfmeet-sipp-load-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new("sipp")
        .args([
            "-sf",
            cert_dir()
                .join("sipp/carrier_uas_answer_simple.xml")
                .to_str()
                .unwrap(),
            "-i",
            "127.0.0.2",
            "-p",
            "5080",
            "-mi",
            "127.0.0.2",
            "-min_rtp_port",
            "17000",
            "-max_rtp_port",
            "17998",
            "-m",
            &calls.to_string(),
            "-l",
            &calls.to_string(),
            "-timeout",
            "120s",
            "-timeout_error",
            "-nostdin",
        ])
        .current_dir(&dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sipp starts");
    Proc(child, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sip_signalling_load_on_the_certification_edge() {
    use telephony::sip::{
        ari::{AriEdge, AriSettings},
        SipMediaEdge,
    };
    let required = std::env::var("TELEPHONY_SIP_CERT_REQUIRED").as_deref() == Ok("1");
    if !(available("asterisk") && available("sipp")) {
        assert!(
            !required,
            "TELEPHONY_SIP_CERT_REQUIRED=1 but asterisk / sipp are not installed"
        );
        println!("SKIPPED: SIP signalling load (asterisk / sipp not installed)");
        return;
    }
    if common::admin_url().is_none() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("telephony=warn"))
        .with_test_writer()
        .try_init();
    let asterisk = start_asterisk();
    let edge = Arc::new(AriEdge::start(AriSettings {
        base_url: ARI_URL.into(),
        username: ARI_USER.into(),
        password: zeroize::Zeroizing::new(ARI_PASSWORD.into()),
        app: "cfmeet-cert".into(),
        request_timeout: Duration::from_secs(5),
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while edge.health().await != telephony::domain::Health::Healthy {
        assert!(
            Instant::now() < deadline,
            "the edge did not come up:\n{}",
            std::fs::read_to_string(asterisk.1.join("console.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut h = Harness::with_edge(edge.clone()).await.unwrap();
    h.svc.settings.ring_timeout = Duration::from_secs(20);
    h.svc.settings.provider_request_timeout = Duration::from_secs(5);
    tokio::spawn(h.svc.clone().run_edge_events());
    let env = environment(&h).await;
    let sampler = DbSampler::start(&h.db.url).await;
    let mut carrier = start_carrier(SIP_CALLS);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // SIP_CALLS originations at once through route ng-a (INVITE / 180 / 200 / ACK).
    sampler.take();
    let started = Instant::now();
    let mut legs = Vec::new();
    for i in 0..SIP_CALLS {
        let contact = h
            .privacy
            .add_contact(&format!("+23480312{:05}", 30000 + i), "NG");
        let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
        body["candidateRoutes"] = json!(["ng-a"]);
        let r = h
            .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
        legs.push(
            r.json["leg"]["telephonyLegId"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let other = h.svc.clone().as_instance("w-sip-load-2");
    let mut answered: HashMap<Uuid, f64> = HashMap::new();
    let mut executed: HashMap<Uuid, f64> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while answered.len() < SIP_CALLS && Instant::now() < deadline {
        h.clock.set(chrono::Utc::now());
        let (a, b) = tokio::join!(h.svc.run_commands_once(50), other.run_commands_once(50));
        a.unwrap();
        b.unwrap();
        h.svc.deliver_outbox_once(200).await.unwrap();
        let at = ms(started.elapsed());
        for leg in &legs {
            let status = h.leg_status(*leg).await;
            if status != "PLANNED" {
                executed.entry(*leg).or_insert(at);
            }
            if matches!(status.as_str(), "ANSWERED" | "CONNECTED") {
                answered.entry(*leg).or_insert(at);
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let setup = started.elapsed();
    let errors = SIP_CALLS - answered.len();
    let lag = executed.values().copied().fold(0.0, f64::max);
    Scenario {
        name: "sip_signalling_setup",
        concurrency: SIP_CALLS,
        duration: setup,
        latencies_ms: answered.values().copied().collect(),
        errors,
        worker_lag_ms: lag,
    }
    .print(&env, &sampler);
    if errors > 0 {
        let commands = h.text("SELECT string_agg(kind || ':' || status || ':' || coalesce(result_code, '-') || ':' || attempts, ' | ') FROM telephony_commands").await;
        let legs_now = h.text("SELECT string_agg(status || ':' || coalesce(end_reason, '-'), ' | ') FROM telephony_legs").await;
        let console = std::fs::read_to_string(asterisk.1.join("console.log")).unwrap_or_default();
        let tail: Vec<&str> = console.lines().rev().take(40).collect();
        panic!(
            "{errors} SIP legs not answered\ncommands: {commands:?}\nlegs: {legs_now:?}\nasterisk:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }
    // One INVITE per command: the edge created exactly one channel per leg.
    assert_eq!(
        h.scalar(
            "SELECT count(DISTINCT provider_call_ref) FROM provider_call_bindings WHERE status = 'BOUND'"
        )
        .await,
        SIP_CALLS as i64
    );

    // Tear-down: TERMINATE every leg (BYE / 200).
    sampler.take();
    let started = Instant::now();
    for leg in &legs {
        let r = h
            .calling(
                Method::POST,
                &format!("/internal/v1/legs/{leg}/terminate"),
                Some(json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(),
                             "idempotencyKey": format!("term-{leg}"),
                             "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
                             "reason": "CALL_ENDED" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    }
    let mut ended: HashMap<Uuid, f64> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while ended.len() < SIP_CALLS && Instant::now() < deadline {
        h.clock.set(chrono::Utc::now());
        let (a, b) = tokio::join!(h.svc.run_commands_once(50), other.run_commands_once(50));
        a.unwrap();
        b.unwrap();
        let at = ms(started.elapsed());
        for leg in &legs {
            if h.leg_status(*leg).await == "ENDED" {
                ended.entry(*leg).or_insert(at);
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let teardown = started.elapsed();
    let errors = SIP_CALLS - ended.len();
    let lag = ended.values().copied().fold(0.0, f64::max);
    Scenario {
        name: "sip_signalling_teardown",
        concurrency: SIP_CALLS,
        duration: teardown,
        latencies_ms: ended.values().copied().collect(),
        errors,
        worker_lag_ms: lag,
    }
    .print(&env, &sampler);
    assert_eq!(errors, 0, "every SIP leg ended");
    // The simulated carrier saw every call complete (SIPp exits 0 after SIP_CALLS).
    let deadline = Instant::now() + Duration::from_secs(20);
    let ok = loop {
        if let Ok(Some(status)) = carrier.0.try_wait() {
            break status.success();
        }
        assert!(Instant::now() < deadline, "the carrier did not finish");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(ok, "SIPp reported failed calls");
    sampler.finish().await;
    drop(asterisk);
}

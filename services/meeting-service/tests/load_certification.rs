//! Part M §36–§41, §45 — reproducible CONTROL-PLANE load harness (media load is the
//! separate `livekit_certification::real_livekit_m_media_load_and_large_audience`).
//!
//! Every scenario is `#[ignore]` and runs over real HTTP / WebSocket against either
//!
//! - the in-process service (default): the production router served on a loopback
//!   TCP port, real PostgreSQL + Redis, the production DB pool size (10, overridable
//!   with `LOAD_DB_POOL`) and the mock LiveKit RoomService; or
//! - an external deployment: `LOAD_BASE_URL` + `LOAD_DATABASE_URL` (synthetic user
//!   seeding and invariant checks) + `LOAD_JWT_SECRET` (+ optional `LOAD_SERVER_PID`
//!   for CPU / RSS sampling of a same-host server process).
//!
//! Configuration (environment): `LOAD_JOIN_LEVELS` (default `25,50,100,250,500`),
//! `LOAD_WS_LEVELS` (same default), `LOAD_MEMBER_RATIO` (registered vs guest, default
//! 0.3), `LOAD_WS_ROUNDS` (default 10), `LOAD_SLOW_CLIENT_PCT` (default 5),
//! `LOAD_CHURN_WORKERS` (default 20), `LOAD_CHURN_SECS` (default 60),
//! `LOAD_SCHED_SERIES` (default 300), `LOAD_SCHED_LIVE` (default 200),
//! `LOAD_SCHED_INSTANCES` (default 4), `LOAD_SOAK_SECS` (default 300),
//! `CFMEET_CERT_OUT` (JSON reports). Run with `scripts/certification/run-load.sh`.
//!
//! Each run prints `LOAD_REPORT {json}` lines with p50/p95/p99, throughput, errors,
//! CPU, memory, DB pool / PostgreSQL behaviour and a PASS / PASS WITH OBSERVED LIMIT /
//! FAIL classification. No capacity beyond the levels actually run is implied.

mod common;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use backend::services::auth::JwtService;
use common::*;
use futures_util::StreamExt;
use reqwest::Method;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

// ------------------------------------------------------------------ config ------

fn env_usize(k: &str, d: usize) -> usize {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn env_f64(k: &str, d: f64) -> f64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn env_levels(k: &str) -> Vec<usize> {
    std::env::var(k)
        .unwrap_or_else(|_| "25,50,100,250,500".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

// ------------------------------------------------------------------ target ------

#[derive(Clone)]
enum A {
    None,
    Bearer(String),
    Participant(String),
}

struct Ctx {
    base: String,
    ws_base: String,
    http: reqwest::Client,
    db: DatabaseConnection,
    jwt: JwtService,
    server_pid: Option<u32>,
    in_process: bool,
    pool: Option<u32>,
    _relay: Option<tokio::sync::watch::Sender<bool>>,
    _app: Option<TestApp>,
}

impl Ctx {
    async fn new() -> Option<Self> {
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(1024)
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        if let Ok(base) = std::env::var("LOAD_BASE_URL") {
            let db_url =
                std::env::var("LOAD_DATABASE_URL").expect("LOAD_DATABASE_URL (external mode)");
            let secret = std::env::var("LOAD_JWT_SECRET").expect("LOAD_JWT_SECRET (external mode)");
            let db = sea_orm::Database::connect(db_url).await.expect("database");
            return Some(Self {
                ws_base: base.replacen("http", "ws", 1),
                base,
                http,
                db,
                jwt: JwtService::new(secret),
                server_pid: std::env::var("LOAD_SERVER_PID")
                    .ok()
                    .and_then(|p| p.parse().ok()),
                in_process: false,
                pool: None,
                _relay: None,
                _app: None,
            });
        }
        let pool = env_usize("LOAD_DB_POOL", 10) as u32;
        let db = TestDb::with_pool(pool, Some(Duration::from_secs(30))).await?;
        let app = TestApp::on_with_provider(
            db,
            TestOptions {
                rtc: RtcMode::LiveKitMock,
                flags: TestApp::ghi_flags(),
                ..TestOptions::default()
            },
        )
        .await;
        let addr = app.serve().await;
        // The outbox relay exactly as `main.rs` runs it (Redis fanout publisher + RTC
        // control, 2 s poll), so realtime latency reflects the deployed pipeline.
        let relay = backend::workers::outbox_relay::spawn_outbox_relay(
            backend::workers::outbox_relay::OutboxRelay::with_control(
                app.db.conn.clone(),
                Some(Arc::new(backend::services::realtime::RedisPublisher::new(
                    app.state.redis.clone(),
                ))
                    as Arc<dyn backend::workers::outbox_relay::EventPublisher>),
                Some(Arc::new(backend::services::rtc_control::RtcControl::new(
                    app.state.meeting.core.clone(),
                ))),
            ),
            2,
        );
        Some(Self {
            base: format!("http://{addr}"),
            ws_base: format!("ws://{addr}"),
            http,
            db: app.db.conn.clone(),
            jwt: JwtService::new(std::env::var("JWT_SECRET").unwrap_or_default()),
            server_pid: Some(std::process::id()),
            in_process: true,
            pool: Some(pool),
            _relay: Some(relay),
            _app: Some(app),
        })
    }

    fn token(&self, user: Uuid) -> String {
        match &self._app {
            Some(app) => app
                .state
                .jwt_service
                .generate_access_token(user, "user")
                .unwrap(),
            None => self.jwt.generate_access_token(user, "user").unwrap(),
        }
    }

    /// Synthetic registered users (never real accounts).
    async fn users(&self, n: usize) -> Vec<(Uuid, String)> {
        let mut out = vec![];
        for _ in 0..n {
            let id = Uuid::new_v4();
            exec_sql(
                &self.db,
                &format!(
                    "INSERT INTO users (id, username, email, auth_type, role, status, created_at, updated_at) \
                     VALUES ('{id}', 'load-{s}', 'load-{s}@synthetic.invalid', 'local', 'user', 'active', now(), now())",
                    s = id.simple()
                ),
            )
            .await;
            out.push((id, self.token(id)));
        }
        out
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &A,
        body: Option<Value>,
    ) -> (u16, Value, f64) {
        let mut rb = self.http.request(method, format!("{}{}", self.base, path));
        rb = match auth {
            A::None => rb,
            A::Bearer(t) => rb.bearer_auth(t),
            A::Participant(t) => rb.header("x-participant-token", t),
        };
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let t = Instant::now();
        match rb.send().await {
            Ok(r) => {
                let s = r.status().as_u16();
                let v = r.json::<Value>().await.unwrap_or(Value::Null);
                (s, v, t.elapsed().as_secs_f64() * 1000.0)
            }
            Err(e) => (
                0,
                json!({ "transport": e.to_string() }),
                t.elapsed().as_secs_f64() * 1000.0,
            ),
        }
    }

    async fn post(&self, path: &str, auth: &A, body: Value) -> (u16, Value, f64) {
        self.call(Method::POST, path, auth, Some(body)).await
    }

    async fn live(&self, host: &str, body: Value) -> (Uuid, Uuid) {
        let auth = A::Bearer(host.to_string());
        let (s, sp, _) = self.post("/api/v2/meeting-spaces", &auth, body).await;
        assert_eq!(s, 201, "{sp}");
        let sid = uuid(&sp["id"]);
        let (s, c, _) = self
            .post(
                &format!("/api/v2/meeting-spaces/{sid}/conferences"),
                &auth,
                json!({}),
            )
            .await;
        assert_eq!(s, 201, "{c}");
        let cid = uuid(&c["id"]);
        let (s, v, _) = self
            .post(
                &format!("/api/v2/conferences/{cid}/start"),
                &auth,
                json!({}),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        (sid, cid)
    }

    async fn n(&self, sql: &str) -> i64 {
        self.db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap_or_else(|e| panic!("{e}\n{sql}"))
            .unwrap()
            .try_get_by_index::<i64>(0)
            .unwrap()
    }
}

async fn exec_sql(db: &DatabaseConnection, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

// ------------------------------------------------------------------ metrics -----

fn pct(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() as f64 - 1.0) * q).round() as usize]
}

fn lat(v: &[f64]) -> Value {
    json!({ "n": v.len(), "p50": pct(v, 0.5), "p95": pct(v, 0.95), "p99": pct(v, 0.99), "max": pct(v, 1.0) })
}

fn proc_cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}

fn proc_rss_kb(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// Background sampler: process CPU / RSS, DB pool in-use, PostgreSQL activity and
/// lock waits, plus PostgreSQL commit / rollback / deadlock deltas.
struct Sampler {
    stop: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<Value>,
}

impl Sampler {
    async fn start(ctx: &Ctx) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let db = ctx.db.clone();
        let pid = ctx.server_pid;
        let in_process = ctx.in_process;
        let flag = stop.clone();
        let handle = tokio::spawn(async move {
            let stat = |db: DatabaseConnection| async move {
                let r = db
                    .query_one(Statement::from_string(
                        sea_orm::DbBackend::Postgres,
                        "SELECT xact_commit, xact_rollback, deadlocks FROM pg_stat_database WHERE datname = current_database()".to_string(),
                    ))
                    .await
                    .ok()
                    .flatten();
                r.map(|r| {
                    (
                        r.try_get_by_index::<i64>(0).unwrap_or(0),
                        r.try_get_by_index::<i64>(1).unwrap_or(0),
                        r.try_get_by_index::<i64>(2).unwrap_or(0),
                    )
                })
                .unwrap_or_default()
            };
            let s0 = stat(db.clone()).await;
            let t0 = Instant::now();
            let cpu0 = pid.and_then(proc_cpu_ticks);
            let rss0 = pid.and_then(proc_rss_kb).unwrap_or(0);
            let (mut rss_peak, mut pool_peak, mut pg_active_peak, mut lock_wait_peak) =
                (rss0, 0u32, 0i64, 0i64);
            while !flag.load(Ordering::Relaxed) {
                rss_peak = rss_peak.max(pid.and_then(proc_rss_kb).unwrap_or(0));
                if in_process {
                    let p = db.get_postgres_connection_pool();
                    pool_peak = pool_peak.max(p.size().saturating_sub(p.num_idle() as u32));
                }
                if let Ok(Some(r)) = db
                    .query_one(Statement::from_string(
                        sea_orm::DbBackend::Postgres,
                        "SELECT (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND state = 'active'), \
                                (SELECT count(*) FROM pg_locks WHERE NOT granted)"
                            .to_string(),
                    ))
                    .await
                {
                    pg_active_peak = pg_active_peak.max(r.try_get_by_index::<i64>(0).unwrap_or(0));
                    lock_wait_peak = lock_wait_peak.max(r.try_get_by_index::<i64>(1).unwrap_or(0));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let s1 = stat(db.clone()).await;
            let secs = t0.elapsed().as_secs_f64();
            let cpu = match (cpu0, pid.and_then(proc_cpu_ticks)) {
                (Some(a), Some(b)) => Some(((b - a) as f64 / 100.0) / secs * 100.0),
                _ => None,
            };
            json!({
                "processCpuPctAvg": cpu,
                "processCpuNote": if in_process { "in-process: service + load clients share this process" } else { "external server process" },
                "rssStartMb": rss0 / 1024,
                "rssPeakMb": rss_peak / 1024,
                "rssEndMb": pid.and_then(proc_rss_kb).unwrap_or(0) / 1024,
                "dbPoolInUsePeak": if in_process { Some(pool_peak) } else { None },
                "pgActivePeak": pg_active_peak,
                "pgLockWaitsPeak": lock_wait_peak,
                "pgCommits": s1.0 - s0.0,
                "pgRollbacks": s1.1 - s0.1,
                "pgDeadlocks": s1.2 - s0.2,
            })
        });
        Self { stop, handle }
    }

    async fn finish(self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.await.unwrap()
    }
}

fn host_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0)
}

fn emit(name: &str, report: &Value) {
    println!("LOAD_REPORT {name} {report}");
    if let Ok(dir) = std::env::var("CFMEET_CERT_OUT") {
        let _ = std::fs::write(
            format!("{dir}/load-{name}.json"),
            serde_json::to_vec_pretty(report).unwrap(),
        );
    }
}

fn environment(ctx: &Ctx) -> Value {
    json!({
        "mode": if ctx.in_process { "in-process service over loopback TCP" } else { "external deployment" },
        "base": if ctx.in_process { "loopback" } else { "LOAD_BASE_URL" },
        "dbPool": ctx.pool,
        "hostCpus": host_cpus(),
        "rtcProvider": if ctx.in_process { "mock LiveKit RoomService (control-plane load; media load is separate)" } else { "deployment's provider" },
        "outboxRelay": if ctx.in_process { "production relay (Redis publisher + RTC control, 2 s poll)" } else { "deployment's relay" },
    })
}

// ------------------------------------------------------------------ §38 ----------

/// §38 — simultaneous join authorization + credential issuance at stepped levels.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load certification (scripts/certification/run-load.sh)"]
async fn load_burst_join() {
    let Some(ctx) = Ctx::new().await else { return };
    let ctx = Arc::new(ctx);
    let ratio = env_f64("LOAD_MEMBER_RATIO", 0.3);
    let (_, host) = ctx.users(1).await.remove(0);
    let mut results = vec![];
    for level in env_levels("LOAD_JOIN_LEVELS") {
        let (_, cid) = ctx
            .live(&host, json!({ "title": format!("burst {level}"), "accessPolicy": "PUBLIC", "interactiveCapacity": (level + 10).min(1000) }))
            .await;
        let members = ((level as f64) * ratio).round() as usize;
        let member_tokens = ctx.users(members).await;
        let barrier = Arc::new(tokio::sync::Barrier::new(level));
        let sampler = Sampler::start(&ctx).await;
        let t0 = Instant::now();
        let mut handles = vec![];
        for i in 0..level {
            let ctx = ctx.clone();
            let barrier = barrier.clone();
            // The first `members` joiners are registered members, the rest guests.
            let auth = member_tokens
                .get(i)
                .map_or(A::None, |t| A::Bearer(t.1.clone()));
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                ctx.post(
                    &format!("/api/v2/conferences/{cid}/join"),
                    &auth,
                    json!({ "displayName": format!("load-{i}") }),
                )
                .await
            }));
        }
        let mut lats = vec![];
        let mut errors: BTreeMap<String, usize> = BTreeMap::new();
        let mut credentials = HashSet::new();
        let mut identity_mismatch = 0;
        let mut ok = 0;
        for h in handles {
            let (s, v, ms) = h.await.unwrap();
            lats.push(ms);
            if s == 200 && v["rtcStatus"] == "ISSUED" {
                ok += 1;
                let cred = v["rtc"]["credential"].as_str().unwrap().to_string();
                // The credential is bound to this session's media identity.
                let sid = uuid(&v["participantSession"]["id"]);
                let claims: Value = cred
                    .split('.')
                    .nth(1)
                    .and_then(base64_decode)
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or(Value::Null);
                if claims["sub"] != json!(format!("ps_{}", sid.simple())) {
                    if identity_mismatch == 0 {
                        println!(
                            "credential claims sample: {claims} (session {sid}) cred={}",
                            &cred[..cred.len().min(60)]
                        );
                    }
                    identity_mismatch += 1;
                }
                credentials.insert(cred);
            } else {
                *errors.entry(format!("{s} {}", code(&v))).or_default() += 1;
            }
        }
        let wall = t0.elapsed().as_secs_f64();
        let resources = sampler.finish().await;
        let participants = ctx
            .n(&format!(
                "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
            ))
            .await;
        let dup_users = ctx.n(&format!("SELECT count(*) - count(DISTINCT user_id) FROM conference_participants WHERE conference_id='{cid}' AND user_id IS NOT NULL")).await;
        let sessions = ctx
            .n(&format!(
                "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}'"
            ))
            .await;
        let dup_identity = ctx.n(&format!("SELECT count(*) - count(DISTINCT media_identity) FROM participant_sessions WHERE conference_id='{cid}'")).await;
        let wrong_role = ctx.n(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND role <> 'PARTICIPANT'")).await;
        let invariants_ok = participants == ok as i64
            && sessions == ok as i64
            && dup_users == 0
            && dup_identity == 0
            && wrong_role == 0
            && credentials.len() == ok
            && identity_mismatch == 0
            && resources["pgDeadlocks"] == 0;
        let classification = if !invariants_ok {
            "FAIL"
        } else if errors.is_empty() {
            "PASS"
        } else {
            "PASS WITH OBSERVED LIMIT"
        };
        let r = json!({
            "level": level, "members": members, "guests": level - members,
            "latencyMs": lat(&lats), "throughputJoinsPerSec": ok as f64 / wall, "wallSecs": wall,
            "succeeded": ok, "errors": errors, "errorRate": (level - ok) as f64 / level as f64,
            "invariants": { "participants": participants, "sessions": sessions, "duplicateUsers": dup_users, "duplicateMediaIdentities": dup_identity,
                            "wrongRole": wrong_role, "distinctCredentials": credentials.len(), "credentialIdentityMismatch": identity_mismatch },
            "resources": resources, "classification": classification,
        });
        println!("LOAD burst-join level {level}: {classification} {r}");
        assert!(invariants_ok, "burst join invariants: {r}");
        results.push(r);
    }
    emit(
        "burst-join",
        &json!({ "environment": environment(&ctx), "levels": results }),
    );
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()
}

// ------------------------------------------------------------------ §39 ----------

struct Client {
    frames: Arc<AtomicU64>,
    seqs: Arc<Mutex<Vec<(i64, String, Instant)>>>,
    closed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

async fn ws_client(ctx: &Ctx, cid: Uuid, token: &str, slow: bool) -> Option<Client> {
    let (s, t, _) = ctx
        .post(
            &format!("/api/v2/conferences/{cid}/realtime/ticket"),
            &A::Participant(token.to_string()),
            json!({}),
        )
        .await;
    if s != 200 {
        return None;
    }
    let url = format!(
        "{}/api/v2/conferences/{cid}/realtime?ticket={}",
        ctx.ws_base,
        t["ticket"].as_str()?
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.ok()?;
    let seqs = Arc::new(Mutex::new(vec![]));
    let closed = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let (s2, c2, st2, f2) = (seqs.clone(), closed.clone(), stop.clone(), frames.clone());
    let handle = tokio::spawn(async move {
        loop {
            if slow && !st2.load(Ordering::Relaxed) {
                // Slow consumer: reads nothing until the generator finishes.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                    f2.fetch_add(1, Ordering::Relaxed);
                    if f2.load(Ordering::Relaxed) <= 2 && std::env::var("LOAD_DEBUG").is_ok() {
                        println!("ws frame: {t}");
                    }
                    if let Ok(f) = serde_json::from_str::<Value>(&t) {
                        if f["type"] == "event" {
                            if let Some(seq) = f["event"]["sequence"].as_i64() {
                                let ty = f["event"]["eventType"].as_str().unwrap_or("").to_string();
                                s2.lock().await.push((seq, ty, Instant::now()));
                            }
                        }
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(None) | Ok(Some(Err(_))) => {
                    c2.store(true, Ordering::Relaxed);
                    return;
                }
                Err(_) => {
                    if st2.load(Ordering::Relaxed) {
                        return;
                    }
                }
            }
        }
    });
    Some(Client {
        frames,
        seqs,
        closed,
        stop,
        handle,
    })
}

fn redis_info() -> Value {
    let Ok(url) = std::env::var("TEST_REDIS_URL") else {
        return Value::Null;
    };
    let Ok(client) = redis::Client::open(url) else {
        return Value::Null;
    };
    let Ok(mut con) = client.get_connection() else {
        return Value::Null;
    };
    let info: String = redis::cmd("INFO").query(&mut con).unwrap_or_default();
    let get = |k: &str| {
        info.lines()
            .find(|l| l.starts_with(&format!("{k}:")))
            .and_then(|l| l.split(':').nth(1))
            .map(|v| v.trim().to_string())
    };
    json!({
        "usedMemoryMb": get("used_memory").and_then(|v| v.parse::<f64>().ok()).map(|b| b / 1048576.0),
        "pubsubChannels": get("pubsub_channels"),
        "totalCommandsProcessed": get("total_commands_processed").and_then(|v| v.parse::<i64>().ok()),
    })
}

/// §39 — WebSocket fanout at stepped sizes: participant update, hand raise, chat,
/// poll, role change and conference events; delivery latency (commit → receipt),
/// event loss, disconnects, memory growth, Redis usage; a slice of slow clients.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load certification (scripts/certification/run-load.sh)"]
async fn load_ws_fanout() {
    let Some(ctx) = Ctx::new().await else { return };
    let ctx = Arc::new(ctx);
    let rounds = env_usize("LOAD_WS_ROUNDS", 10);
    let slow_pct = env_usize("LOAD_SLOW_CLIENT_PCT", 5);
    let (_, host) = ctx.users(1).await.remove(0);
    let hauth = A::Bearer(host.clone());
    let mut results = vec![];
    for level in env_levels("LOAD_WS_LEVELS") {
        let (_, cid) = ctx
            .live(&host, json!({ "title": format!("fanout {level}"), "accessPolicy": "PUBLIC", "interactiveCapacity": (level + 10).min(1000) }))
            .await;
        // Guests join (batches of 50).
        let mut guests: Vec<(Uuid, String)> = vec![];
        for chunk in (0..level).collect::<Vec<_>>().chunks(50) {
            let mut hs = vec![];
            for i in chunk {
                let ctx = ctx.clone();
                let i = *i;
                hs.push(tokio::spawn(async move {
                    ctx.post(
                        &format!("/api/v2/conferences/{cid}/join"),
                        &A::None,
                        json!({ "displayName": format!("ws-{i}") }),
                    )
                    .await
                }));
            }
            for h in hs {
                let (s, v, _) = h.await.unwrap();
                assert_eq!(s, 200, "{v}");
                guests.push((
                    uuid(&v["participant"]["id"]),
                    v["participantToken"].as_str().unwrap().to_string(),
                ));
            }
        }
        let redis0 = redis_info();
        let sampler = Sampler::start(&ctx).await;
        let rss_before = ctx.server_pid.and_then(proc_rss_kb).unwrap_or(0);
        let slow_n = level * slow_pct / 100;
        let mut clients = vec![];
        let mut subscribe_failures = 0;
        for (i, (_, tok)) in guests.iter().enumerate() {
            match ws_client(&ctx, cid, tok, i < slow_n).await {
                Some(c) => clients.push(c),
                None => subscribe_failures += 1,
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let rss_subscribed = ctx.server_pid.and_then(proc_rss_kb).unwrap_or(0);
        let seq_sql = format!("SELECT coalesce(max(conference_seq), 0) FROM outbox_events WHERE conference_id='{cid}' AND channel = 'REALTIME'");
        let mut commit_at: Vec<(i64, Instant)> = vec![(ctx.n(&seq_sql).await, Instant::now())];
        let mut action_errors: BTreeMap<String, usize> = BTreeMap::new();
        let poll_body = json!({ "title": "load poll", "resultsVisibility": "AFTER_VOTE", "questions": [{ "type": "SINGLE_CHOICE", "prompt": "A or B?", "options": ["A", "B"] }] });
        let t_gen = Instant::now();
        for r in 0..rounds {
            let (gp, gt) = &guests[r % guests.len()];
            let gauth = A::Participant(gt.clone());
            let lock = if r % 2 == 0 { "lock" } else { "unlock" };
            let role = if r % 2 == 0 { "VIEWER" } else { "PARTICIPANT" };
            let actions: Vec<(&str, Method, String, A, Option<Value>)> = vec![
                (
                    "conference",
                    Method::POST,
                    format!("/api/v2/conferences/{cid}/{lock}"),
                    hauth.clone(),
                    Some(json!({})),
                ),
                (
                    "chat",
                    Method::POST,
                    format!("/api/v2/conferences/{cid}/messages"),
                    gauth.clone(),
                    Some(json!({ "body": format!("load message {r}") })),
                ),
                (
                    "hand",
                    Method::POST,
                    format!("/api/v2/conferences/{cid}/hand-raise"),
                    gauth.clone(),
                    Some(json!({})),
                ),
                (
                    "participant-update",
                    Method::POST,
                    format!("/api/v2/conferences/{cid}/participants/{gp}/audio-lock"),
                    hauth.clone(),
                    Some(json!({})),
                ),
                (
                    "role",
                    Method::PATCH,
                    format!("/api/v2/conferences/{cid}/participants/{gp}/role"),
                    hauth.clone(),
                    Some(json!({ "role": role })),
                ),
                (
                    "poll",
                    Method::POST,
                    format!("/api/v2/conferences/{cid}/polls"),
                    hauth.clone(),
                    Some(poll_body.clone()),
                ),
            ];
            for (name, m, path, auth, body) in actions {
                let (s, v, _) = ctx.call(m, &path, &auth, body).await;
                if !(200..300).contains(&s) {
                    *action_errors
                        .entry(format!("{name} {s} {}", code(&v)))
                        .or_default() += 1;
                }
                if name == "poll" && s == 201 {
                    let pid = uuid(&v["id"]);
                    ctx.post(
                        &format!("/api/v2/conferences/{cid}/polls/{pid}/open"),
                        &hauth,
                        json!({}),
                    )
                    .await;
                }
                commit_at.push((ctx.n(&seq_sql).await, Instant::now()));
            }
        }
        let gen_secs = t_gen.elapsed().as_secs_f64();
        // Longer than the relay poll (2 s) + the socket's own PostgreSQL poll (5 s).
        tokio::time::sleep(Duration::from_secs(7)).await;
        for c in &clients {
            c.stop.store(true, Ordering::Relaxed);
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
        // Map each sequence to the response instant of the action that committed it.
        let seq_time = |s: i64| {
            commit_at
                .windows(2)
                .find(|w| s > w[0].0 && s <= w[1].0)
                .map(|w| w[1].1)
        };
        let broadcast: HashSet<i64> = {
            let rows = ctx
                .db
                .query_all(Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    format!(
                        "SELECT conference_seq FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND conference_seq > {} \
                         AND event_type IN ('conference.locked','conference.unlocked','chat.message_created','poll.opened')",
                        commit_at[0].0
                    ),
                ))
                .await
                .unwrap();
            rows.iter()
                .map(|r| r.try_get_by_index::<i64>(0).unwrap())
                .collect()
        };
        let (mut fast_lat, mut slow_lat) = (vec![], vec![]);
        let (mut lost, mut disconnects, mut frames) = (0usize, 0usize, 0u64);
        for (i, c) in clients.iter().enumerate() {
            let seqs = c.seqs.lock().await;
            let got: HashSet<i64> = seqs.iter().map(|x| x.0).collect();
            lost += broadcast.difference(&got).count();
            for (s, _, at) in seqs.iter() {
                if let Some(t) = seq_time(*s) {
                    let ms = at.saturating_duration_since(t).as_secs_f64() * 1000.0;
                    if i < slow_n {
                        slow_lat.push(ms)
                    } else {
                        fast_lat.push(ms)
                    }
                }
            }
            if c.closed.load(Ordering::Relaxed) {
                disconnects += 1;
            }
            frames += c.frames.load(Ordering::Relaxed);
        }
        let rss_after = ctx.server_pid.and_then(proc_rss_kb).unwrap_or(0);
        for c in clients {
            c.handle.abort();
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let rss_closed = ctx.server_pid.and_then(proc_rss_kb).unwrap_or(0);
        let resources = sampler.finish().await;
        let redis1 = redis_info();
        let expected = broadcast.len() * (level - subscribe_failures);
        let classification = if lost > 0 || subscribe_failures > 0 {
            "FAIL"
        } else if !action_errors.is_empty() || disconnects > 0 {
            "PASS WITH OBSERVED LIMIT"
        } else {
            "PASS"
        };
        let r = json!({
            "clients": level, "slowClients": slow_n, "rounds": rounds, "generatedActions": commit_at.len() - 1, "generationSecs": gen_secs,
            "broadcastEvents": broadcast.len(), "expectedBroadcastDeliveries": expected, "lostBroadcastDeliveries": lost,
            "deliveryLatencyMs": lat(&fast_lat), "slowClientDeliveryLatencyMs": lat(&slow_lat),
            "subscribeFailures": subscribe_failures, "unexpectedDisconnects": disconnects, "framesReceived": frames, "actionErrors": action_errors,
            "rssMb": { "beforeSubscribe": rss_before / 1024, "subscribed": rss_subscribed / 1024, "afterEvents": rss_after / 1024, "afterClose": rss_closed / 1024 },
            "redis": { "before": redis0, "after": redis1 },
            "resources": resources, "classification": classification,
        });
        println!("LOAD ws-fanout {level}: {classification} {r}");
        assert_eq!(lost, 0, "event loss: {r}");
        results.push(r);
    }
    emit(
        "ws-fanout",
        &json!({ "environment": environment(&ctx), "levels": results }),
    );
}

// ------------------------------------------------------------------ §40 ----------

/// §40 — continuous join / wait / admit / deny / leave / rejoin churn with REST
/// readers measuring responsiveness.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load certification (scripts/certification/run-load.sh)"]
async fn load_waiting_room_churn() {
    let Some(ctx) = Ctx::new().await else { return };
    let ctx = Arc::new(ctx);
    let workers = env_usize("LOAD_CHURN_WORKERS", 20);
    let secs = env_usize("LOAD_CHURN_SECS", 60) as u64;
    let (_, host) = ctx.users(1).await.remove(0);
    let (_, cid) = ctx
        .live(&host, json!({ "title": "churn", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE", "interactiveCapacity": 1000 }))
        .await;
    let lats: Arc<Mutex<HashMap<&'static str, Vec<f64>>>> = Arc::new(Mutex::new(HashMap::new()));
    let errors: Arc<Mutex<BTreeMap<String, usize>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let cycles = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(secs);
    let sampler = Sampler::start(&ctx).await;
    let mut hs = vec![];
    for w in 0..workers {
        let (ctx, lats, errors, cycles, host) = (
            ctx.clone(),
            lats.clone(),
            errors.clone(),
            cycles.clone(),
            host.clone(),
        );
        hs.push(tokio::spawn(async move {
            let hauth = A::Bearer(host);
            let rec = |k: &'static str,
                       ms: f64,
                       lats: Arc<Mutex<HashMap<&'static str, Vec<f64>>>>| async move {
                lats.lock().await.entry(k).or_default().push(ms)
            };
            let err = |e: String, errors: Arc<Mutex<BTreeMap<String, usize>>>| async move {
                *errors.lock().await.entry(e).or_default() += 1
            };
            let mut i = 0u64;
            while Instant::now() < deadline {
                i += 1;
                let (s, v, ms) = ctx
                    .post(
                        &format!("/api/v2/conferences/{cid}/join"),
                        &A::None,
                        json!({ "displayName": format!("churn-{w}-{i}") }),
                    )
                    .await;
                rec("join", ms, lats.clone()).await;
                if s != 200 || v["participant"]["admissionState"] != "WAITING" {
                    err(format!("join {s} {}", code(&v)), errors.clone()).await;
                    continue;
                }
                let pid = uuid(&v["participant"]["id"]);
                let tok = v["participantToken"].as_str().unwrap().to_string();
                let admit = i.is_multiple_of(2);
                let path = if admit { "admit" } else { "deny" };
                let (s, v, ms) = ctx
                    .post(
                        &format!("/api/v2/conferences/{cid}/waiting-room/{path}"),
                        &hauth,
                        json!({ "participantIds": [pid] }),
                    )
                    .await;
                rec(if admit { "admit" } else { "deny" }, ms, lats.clone()).await;
                if s != 200 {
                    err(format!("{path} {s} {}", code(&v)), errors.clone()).await;
                    continue;
                }
                let pauth = A::Participant(tok);
                if admit {
                    for round in 0..2 {
                        let (s, v, ms) = ctx
                            .post(
                                &format!("/api/v2/conferences/{cid}/join"),
                                &pauth,
                                json!({}),
                            )
                            .await;
                        rec(
                            if round == 0 {
                                "admitted-join"
                            } else {
                                "rejoin"
                            },
                            ms,
                            lats.clone(),
                        )
                        .await;
                        if s != 200 || v["rtcStatus"] != "ISSUED" {
                            err(format!("rejoin {s} {}", code(&v)), errors.clone()).await;
                            break;
                        }
                        let sid = uuid(&v["participantSession"]["id"]);
                        let (s, v, ms) = ctx
                            .post(
                                &format!("/api/v2/conferences/{cid}/leave"),
                                &pauth,
                                json!({ "participantSessionId": sid }),
                            )
                            .await;
                        rec("leave", ms, lats.clone()).await;
                        if !(200..300).contains(&s) {
                            err(format!("leave {s} {}", code(&v)), errors.clone()).await;
                        }
                    }
                } else {
                    // A denied guest may ask again (DENIED is not BANNED): it waits again,
                    // never receives a credential, and the host denies again.
                    let (s, v, ms) = ctx
                        .post(
                            &format!("/api/v2/conferences/{cid}/join"),
                            &pauth,
                            json!({}),
                        )
                        .await;
                    rec("denied-rerequest", ms, lats.clone()).await;
                    if s == 200 && !v["rtc"].is_null() {
                        err(
                            "denied participant obtained a credential".into(),
                            errors.clone(),
                        )
                        .await;
                    }
                    if s == 200 && v["participant"]["admissionState"] == "WAITING" {
                        let (s, v, ms) = ctx
                            .post(
                                &format!("/api/v2/conferences/{cid}/waiting-room/deny"),
                                &hauth,
                                json!({ "participantIds": [pid] }),
                            )
                            .await;
                        rec("deny", ms, lats.clone()).await;
                        if s != 200 {
                            err(format!("deny {s} {}", code(&v)), errors.clone()).await;
                        }
                    }
                }
                cycles.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    // REST readers: waiting room + participant list responsiveness during churn.
    let reader = {
        let (ctx, lats, host) = (ctx.clone(), lats.clone(), host.clone());
        tokio::spawn(async move {
            let hauth = A::Bearer(host);
            while Instant::now() < deadline {
                let (_, _, ms) = ctx
                    .call(
                        Method::GET,
                        &format!("/api/v2/conferences/{cid}/waiting-room"),
                        &hauth,
                        None,
                    )
                    .await;
                lats.lock()
                    .await
                    .entry("read-waiting-room")
                    .or_default()
                    .push(ms);
                let (_, _, ms) = ctx
                    .call(
                        Method::GET,
                        &format!("/api/v2/conferences/{cid}/participants"),
                        &hauth,
                        None,
                    )
                    .await;
                lats.lock()
                    .await
                    .entry("read-participants")
                    .or_default()
                    .push(ms);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    };
    for h in hs {
        h.await.unwrap();
    }
    reader.await.unwrap();
    let resources = sampler.finish().await;
    let stale_waiting = ctx.n(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND admission_state='WAITING'")).await;
    let dup_guest = ctx.n(&format!("SELECT count(*) - count(DISTINCT guest_identity) FROM conference_participants WHERE conference_id='{cid}' AND guest_identity IS NOT NULL")).await;
    let both = ctx.n(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND admitted_at IS NOT NULL AND denied_at IS NOT NULL")).await;
    let denied_live = ctx.n(&format!(
        "SELECT count(*) FROM participant_sessions s JOIN conference_participants p ON p.id = s.conference_participant_id \
         WHERE p.conference_id='{cid}' AND p.admission_state='DENIED' AND s.state IN ('PENDING','CONNECTED')"
    )).await;
    let live_after_leave = ctx.n(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')")).await;
    let lats = lats.lock().await;
    let errors = errors.lock().await.clone();
    let invariants_ok = stale_waiting == 0
        && dup_guest == 0
        && both == 0
        && denied_live == 0
        && live_after_leave == 0
        && !errors.contains_key("denied participant obtained a credential");
    let classification = if !invariants_ok {
        "FAIL"
    } else if errors.is_empty() {
        "PASS"
    } else {
        "PASS WITH OBSERVED LIMIT"
    };
    let r = json!({
        "environment": environment(&ctx), "workers": workers, "durationSecs": secs, "cycles": cycles.load(Ordering::Relaxed),
        "latencyMs": lats.iter().map(|(k, v)| (k.to_string(), lat(v))).collect::<BTreeMap<_, _>>(),
        "errors": errors,
        "invariants": { "staleWaiting": stale_waiting, "duplicateGuestIdentity": dup_guest, "admittedAndDenied": both, "deniedWithLiveSession": denied_live, "liveSessionsAfterLeave": live_after_leave },
        "resources": resources, "classification": classification,
    });
    emit("waiting-room-churn", &r);
    assert!(invariants_ok, "{r}");
}

// ------------------------------------------------------------------ §41 ----------

/// §41 — scheduler load: many recurring series materialised simultaneously and many
/// LIVE conferences due for auto-end, processed by several scheduler instances; plus
/// the legacy auto-start / auto-end workers racing on due legacy meetings.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load certification (scripts/certification/run-load.sh)"]
async fn load_scheduler() {
    let Some(ctx) = Ctx::new().await else { return };
    let app = ctx
        ._app
        .as_ref()
        .expect("scheduler load runs in-process (it drives the worker code directly)");
    let series = env_usize("LOAD_SCHED_SERIES", 300);
    let live = env_usize("LOAD_SCHED_LIVE", 200);
    let instances = env_usize("LOAD_SCHED_INSTANCES", 4);
    let (_, host) = ctx.users(1).await.remove(0);
    let hauth = A::Bearer(host.clone());
    let start = (chrono::Utc::now() + chrono::Duration::days(1))
        .date_naive()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let mut sids = vec![];
    for i in 0..series {
        let (s, sp, _) = ctx
            .post(
                "/api/v2/meeting-spaces",
                &hauth,
                json!({ "title": format!("series {i}") }),
            )
            .await;
        assert_eq!(s, 201, "{sp}");
        let sid = uuid(&sp["id"]);
        let (s, v, _) = ctx
            .call(Method::PUT, &format!("/api/v2/meeting-spaces/{sid}/recurrence"), &hauth, Some(json!({ "rrule": "FREQ=DAILY;COUNT=10", "startLocal": start, "durationMinutes": 30 })))
            .await;
        assert_eq!(s, 200, "{v}");
        sids.push(sid);
    }
    let occ_sql = format!(
        "SELECT count(*) FROM conference_sessions WHERE meeting_space_id IN ({})",
        sids.iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let occurrences_before = ctx.n(&occ_sql).await;
    // Everything due at once: forget materialisation bookkeeping, then race.
    exec_sql(
        &ctx.db,
        "UPDATE meeting_recurrence_rules SET materialized_until = NULL",
    )
    .await;
    let mut cids = vec![];
    for i in 0..live {
        let (_, cid) = ctx
            .live(
                &host,
                json!({ "title": format!("auto-end {i}"), "accessPolicy": "PUBLIC" }),
            )
            .await;
        cids.push(cid);
    }
    exec_sql(&ctx.db, &format!(
        "UPDATE conference_sessions SET scheduled_start_at = now() - interval '3 hours', scheduled_end_at = now() - interval '2 hours' WHERE id IN ({})",
        cids.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(",")
    )).await;
    // Legacy meetings due for auto-start / auto-end.
    let legacy = series.min(200);
    exec_sql(&ctx.db, &format!(
        "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status, created_at, updated_at) \
         SELECT gen_random_uuid(), 'LS' || lpad(i::text, 6, '0'), u.id, u.id, 'legacy due ' || i, false, \
                (now() - interval '5 minutes')::timestamp, CASE WHEN i % 2 = 0 THEN (now() - interval '1 minute')::timestamp ELSE (now() + interval '1 hour')::timestamp END, \
                (CASE WHEN i % 2 = 0 THEN 'ongoing' ELSE 'scheduled' END)::meeting_status, now()::timestamp, now()::timestamp \
           FROM generate_series(1, {legacy}) i, (SELECT id FROM users WHERE email LIKE 'load-%' LIMIT 1) u"
    )).await;

    let sampler = Sampler::start(&ctx).await;
    let t0 = Instant::now();
    let mut handles = vec![];
    for _ in 0..instances {
        let meeting = app.state.meeting.clone();
        let db = ctx.db.clone();
        handles.push(tokio::spawn(async move {
            // Each instance drains until two consecutive idle ticks (bounded at 100).
            let mut ticks = vec![];
            let mut idle = 0;
            while idle < 2 && ticks.len() < 100 {
                let t = Instant::now();
                let r = backend::workers::ConferenceScheduler::new(meeting.clone())
                    .tick(chrono::Utc::now())
                    .await
                    .unwrap();
                idle = if r.materialized + r.end_requested == 0 {
                    idle + 1
                } else {
                    0
                };
                ticks.push((
                    t.elapsed().as_secs_f64() * 1000.0,
                    r.materialized,
                    r.end_requested,
                ));
            }
            let t = Instant::now();
            let (mut started, mut ended) = (0, 0);
            loop {
                let s = backend::workers::MeetingAutoStartWorker::new(db.clone())
                    .process_batch()
                    .await
                    .unwrap_or(0);
                let e = backend::workers::MeetingAutoEndWorker::new(db.clone())
                    .process_batch()
                    .await
                    .unwrap_or(0);
                started += s;
                ended += e;
                if s + e == 0 {
                    break;
                }
            }
            (ticks, started, ended, t.elapsed().as_secs_f64() * 1000.0)
        }));
    }
    let (mut tick_ms, mut end_requested, mut legacy_started, mut legacy_ended, mut legacy_ms) =
        (vec![], 0u64, 0u64, 0u64, vec![]);
    for h in handles {
        let (ticks, s, e, ms) = h.await.unwrap();
        for (ms, _, ended) in ticks {
            tick_ms.push(ms);
            end_requested += ended;
        }
        legacy_started += s;
        legacy_ended += e;
        legacy_ms.push(ms);
    }
    let wall = t0.elapsed().as_secs_f64();
    let resources = sampler.finish().await;
    let occurrences_after = ctx.n(&occ_sql).await;
    let dup_occ = ctx.n("SELECT count(*) - count(DISTINCT (meeting_space_id, occurrence_key)) FROM conference_sessions").await;
    let terminate_cmds = ctx.n(&format!(
        "SELECT count(*) FROM outbox_events WHERE event_type='rtc.room_terminate_requested' AND conference_id IN ({})",
        cids.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(",")
    )).await;
    let dup_start_logs = ctx.n("SELECT count(*) FROM (SELECT meeting_id FROM session_logs WHERE event_type::text='meeting_start' GROUP BY meeting_id HAVING count(*) > 1) x").await;
    let dup_end_logs = ctx.n("SELECT count(*) FROM (SELECT meeting_id FROM session_logs WHERE event_type::text='meeting_end' GROUP BY meeting_id HAVING count(*) > 1) x").await;
    let invariants_ok = occurrences_after == occurrences_before
        && dup_occ == 0
        && end_requested == live as u64
        && terminate_cmds == live as i64
        && dup_start_logs == 0
        && dup_end_logs == 0
        && legacy_started == (legacy / 2) as u64
        && legacy_ended == (legacy / 2) as u64;
    let r = json!({
        "environment": environment(&ctx),
        "scale": { "recurringSeries": series, "occurrences": occurrences_before, "liveDueForAutoEnd": live, "legacyDue": legacy, "schedulerInstances": instances },
        "tickLatencyMs": lat(&tick_ms), "ticks": tick_ms.len(), "legacyWorkerDrainMs": lat(&legacy_ms), "wallSecs": wall,
        "results": { "occurrencesAfterRace": occurrences_after, "duplicateOccurrences": dup_occ, "autoEnded": end_requested, "terminateCommands": terminate_cmds,
                     "legacyAutoStarted": legacy_started, "legacyAutoEnded": legacy_ended, "duplicateLegacyStart": dup_start_logs, "duplicateLegacyEnd": dup_end_logs },
        "resources": resources, "classification": if invariants_ok { "PASS" } else { "FAIL" },
    });
    emit("scheduler", &r);
    assert!(invariants_ok, "{r}");
}

// ------------------------------------------------------------------ §45 ----------

/// §45 — resource-leak soak: repeated meeting lifecycles (join, realtime, chat,
/// breakout open/close, recording start/stop, leave, end) for `LOAD_SOAK_SECS`,
/// sampling memory, connections and orphan indicators every cycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load certification (scripts/certification/run-load.sh)"]
async fn load_resource_leak_soak() {
    let Some(ctx) = Ctx::new().await else { return };
    let ctx = Arc::new(ctx);
    let secs = env_usize("LOAD_SOAK_SECS", 300) as u64;
    let (_, host) = ctx.users(1).await.remove(0);
    let hauth = A::Bearer(host.clone());
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut samples = vec![];
    let mut cycle = 0u64;
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    let t0 = Instant::now();
    while Instant::now() < deadline {
        cycle += 1;
        let (_, cid) = ctx
            .live(
                &host,
                json!({ "title": format!("soak {cycle}"), "accessPolicy": "PUBLIC" }),
            )
            .await;
        let mut guests = vec![];
        for i in 0..8 {
            let (s, v, _) = ctx
                .post(
                    &format!("/api/v2/conferences/{cid}/join"),
                    &A::None,
                    json!({ "displayName": format!("soak-{i}") }),
                )
                .await;
            if s == 200 {
                guests.push((
                    uuid(&v["participantSession"]["id"]),
                    v["participantToken"].as_str().unwrap().to_string(),
                ));
            }
        }
        let mut clients = vec![];
        for (_, tok) in guests.iter().take(4) {
            if let Some(c) = ws_client(&ctx, cid, tok, false).await {
                clients.push(c);
            }
        }
        for (i, (_, tok)) in guests.iter().enumerate() {
            let (s, v, _) = ctx
                .post(
                    &format!("/api/v2/conferences/{cid}/messages"),
                    &A::Participant(tok.clone()),
                    json!({ "body": format!("soak {i}") }),
                )
                .await;
            if s != 201 && s != 200 {
                *errors.entry(format!("chat {s} {}", code(&v))).or_default() += 1;
            }
        }
        let (s, b, _) = ctx
            .post(
                &format!("/api/v2/conferences/{cid}/breakouts"),
                &hauth,
                json!({ "count": 2 }),
            )
            .await;
        if s == 201 {
            for bid in b["breakouts"].as_array().unwrap() {
                ctx.post(
                    &format!(
                        "/api/v2/conferences/{cid}/breakouts/{}/open",
                        bid["id"].as_str().unwrap()
                    ),
                    &hauth,
                    json!({}),
                )
                .await;
            }
            ctx.post(
                &format!("/api/v2/conferences/{cid}/breakouts/close-all"),
                &hauth,
                json!({}),
            )
            .await;
        }
        let (s, rec, _) = ctx
            .post(
                &format!("/api/v2/conferences/{cid}/recordings"),
                &hauth,
                json!({}),
            )
            .await;
        if s == 202 {
            ctx.post(
                &format!(
                    "/api/v2/conferences/{cid}/recordings/{}/stop",
                    rec["id"].as_str().unwrap_or_default()
                ),
                &hauth,
                json!({}),
            )
            .await;
        }
        for (sid, tok) in guests.iter().skip(4) {
            ctx.post(
                &format!("/api/v2/conferences/{cid}/leave"),
                &A::Participant(tok.clone()),
                json!({ "participantSessionId": sid }),
            )
            .await;
        }
        let (s, v, _) = ctx
            .post(&format!("/api/v2/conferences/{cid}/end"), &hauth, json!({}))
            .await;
        if s != 200 {
            *errors.entry(format!("end {s} {}", code(&v))).or_default() += 1;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        for c in clients {
            c.stop.store(true, Ordering::Relaxed);
            c.handle.abort();
        }
        if cycle.is_multiple_of(5) {
            let rss = ctx.server_pid.and_then(proc_rss_kb).unwrap_or(0) / 1024;
            let sample = json!({
                "t": t0.elapsed().as_secs(),
                "cycle": cycle,
                "rssMb": rss,
                "liveConferences": ctx.n("SELECT count(*) FROM conference_sessions WHERE status IN ('LIVE','ENDING')").await,
                "activeSessions": ctx.n("SELECT count(*) FROM participant_sessions s JOIN conference_sessions c ON c.id = s.conference_id WHERE s.state IN ('PENDING','CONNECTED') AND c.status NOT IN ('LIVE','ENDING')").await,
                "outboxPending": ctx.n("SELECT count(*) FROM outbox_events WHERE status = 'PENDING'").await,
                "expiredLeases": ctx.n("SELECT count(*) FROM worker_leases WHERE expires_at < now() - interval '5 minutes'").await,
                "openBreakoutsInEndedConferences": ctx.n("SELECT count(*) FROM breakout_rooms b JOIN conference_sessions c ON c.id = b.conference_id WHERE b.status = 'OPEN' AND c.status = 'ENDED'").await,
                "recordingsStuck": ctx.n("SELECT count(*) FROM meeting_artifacts WHERE status IN ('REQUESTED','RECORDING','STOPPING') AND updated_at < now() - interval '10 minutes'").await,
                "pgConnections": ctx.n("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()").await,
                "mockRooms": ctx._app.as_ref().and_then(|a| a.lk.as_ref()).map(|l| l.room_names().len()),
            });
            println!("SOAK {sample}");
            samples.push(sample);
        }
    }
    let first = samples.first().cloned().unwrap_or(Value::Null);
    let last = samples.last().cloned().unwrap_or(Value::Null);
    let r = json!({
        "environment": environment(&ctx), "durationSecs": secs, "cycles": cycle, "errors": errors,
        "first": first, "last": last, "samples": samples,
        "note": "the production outbox relay runs during the soak (2 s poll); outboxPending is the live backlog at sample time",
    });
    emit("leak-soak", &r);
}

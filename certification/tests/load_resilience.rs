//! Part VI §20.4 — bounded, repeatable load and resilience profiles on the REAL
//! processes of an isolated certification stack (never a shared or production system).
//!
//! Two journeys, each with its own stack:
//!
//! * [`gateway_load_and_resilience`] — mixed REST throughput across six API families
//!   through the gateway, concurrent first-time authentication of many credentials
//!   (Platform Access cache cold → warm) with a revocation under load, multi-tenant quota
//!   races (exact admission under concurrency), WebSocket connect / reconnect / close
//!   load, a Redis interruption (gateway quota degradation and recovery) and a domain
//!   service outage (safe errors, no rerouting, no collateral damage, recovery time).
//! * [`webhook_usage_load_and_resilience`] — webhook fan-out with a failing endpoint
//!   (retry backlog), replay volume, usage ingestion throughput with full replay, a
//!   daily-rollup rebuild under concurrent ingestion, a Developer Platform database
//!   outage and a worker interruption in the middle of a backlog — then exactly-once
//!   checks: one successful delivery per (subscription, event), one fact per producer
//!   source id, rollups equal to the facts.
//!
//! Every profile states its acceptance criteria BEFORE it runs (they are TEST criteria
//! for this environment, not production SLOs — no SLO is documented for these services)
//! and records what it measured: hardware, request / concurrency volume, throughput,
//! error rate, p50 / p95 / p99 latency, resident memory and CPU time per process, queue
//! depth and recovery time. The report is printed and written as JSON to
//! `CFMEET_LOAD_REPORT_DIR` (default: the system temp directory). Binaries are whatever
//! `CFMEET_BIN_DIR` holds (the CI and evidence runs use unoptimized debug builds), so
//! numbers are floors for this hardware, never capacity claims.
//!
//! `CFMEET_LOAD_SCALE` (default 1) multiplies volumes and durations for longer runs.
//! Same prerequisites and skip rules as `tests/journeys.rs`.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use base64::Engine;
use cf_service_auth::{services, OutboundServiceRequest, RuntimeEnvironment, ServiceId};
use cfmeet_certification::{
    db,
    provision::{self, Auth, Customer, Gateway, Resp, Sandbox, SANDBOX_SCOPES},
    receiver::{Receiver, Reply},
    stack::{self, Stack},
    ws, StackConfig, StackOptions,
};
use rand::RngCore;
use reqwest::Method;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
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

fn scale() -> u64 {
    std::env::var("CFMEET_LOAD_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| (1..=20).contains(v))
        .unwrap_or(1)
}

/// Whether the stack runs optimized binaries (`CFMEET_BIN_DIR` = `…/release`). Latency
/// criteria are strict there; a debug stack (CI) keeps every correctness criterion and
/// only a loose latency bound (unoptimized crypto dominates its latency).
fn optimized(config: &StackConfig) -> bool {
    config.bin_dir.ends_with("release")
}

fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

// =====================================================================================
// Measurement
// =====================================================================================

/// Outcome of one profile phase.
#[derive(Default)]
struct Samples {
    latencies_ms: Vec<f64>,
    by_status: BTreeMap<u16, u64>,
    transport_errors: u64,
}

impl Samples {
    fn record(&mut self, status: Option<u16>, elapsed: Duration) {
        self.latencies_ms.push(elapsed.as_secs_f64() * 1000.0);
        match status {
            Some(s) => *self.by_status.entry(s).or_default() += 1,
            None => self.transport_errors += 1,
        }
    }

    fn total(&self) -> u64 {
        self.by_status.values().sum::<u64>() + self.transport_errors
    }

    fn count(&self, pred: impl Fn(u16) -> bool) -> u64 {
        self.by_status
            .iter()
            .filter(|(s, _)| pred(**s))
            .map(|(_, n)| n)
            .sum()
    }

    fn percentile(&self, p: f64) -> f64 {
        if self.latencies_ms.is_empty() {
            return 0.0;
        }
        let mut v = self.latencies_ms.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let i = ((p / 100.0) * (v.len() as f64 - 1.0)).round() as usize;
        (v[i.min(v.len() - 1)] * 10.0).round() / 10.0
    }

    fn summary(&self, wall: Duration, concurrency: usize) -> Value {
        let total = self.total();
        let ok = self.count(|s| (200..300).contains(&s));
        json!({
            "requests": total,
            "concurrency": concurrency,
            "wallSeconds": (wall.as_secs_f64() * 100.0).round() / 100.0,
            "throughputPerSecond": ((total as f64 / wall.as_secs_f64().max(0.001)) * 10.0).round() / 10.0,
            "success2xx": ok,
            "errorRate": if total == 0 { 0.0 } else { ((total - ok) as f64 / total as f64 * 10000.0).round() / 10000.0 },
            "byStatus": self.by_status.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "transportErrors": self.transport_errors,
            "latencyMs": { "p50": self.percentile(50.0), "p95": self.percentile(95.0), "p99": self.percentile(99.0),
                           "max": self.percentile(100.0) },
        })
    }
}

/// Runs `op(i)` for `i in 0..total` with at most `concurrency` in flight; when `rate` is
/// set, requests start on an open-loop schedule of `rate` per second (no coordinated
/// omission: latency counts from the scheduled start).
async fn drive<F, Fut>(
    total: usize,
    concurrency: usize,
    rate: Option<f64>,
    op: F,
) -> (Samples, Duration)
where
    F: Fn(usize) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<u16>> + Send + 'static,
{
    let op = Arc::new(op);
    let samples = Arc::new(Mutex::new(Samples::default()));
    let sem = Arc::new(Semaphore::new(concurrency));
    let start = Instant::now();
    let mut handles = Vec::with_capacity(total);
    for i in 0..total {
        let scheduled = match rate {
            Some(r) => start + Duration::from_secs_f64(i as f64 / r),
            None => Instant::now(),
        };
        if rate.is_some() {
            tokio::time::sleep_until(scheduled.into()).await;
        }
        let permit = sem.clone().acquire_owned().await.unwrap();
        let (op, samples) = (op.clone(), samples.clone());
        handles.push(tokio::spawn(async move {
            let s = op(i).await;
            samples.lock().unwrap().record(s, scheduled.elapsed());
            drop(permit);
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    let wall = start.elapsed();
    let s = std::mem::take(&mut *samples.lock().unwrap());
    (s, wall)
}

/// `(rss_kib, cpu_seconds)` of a process from /proc.
fn proc_usage(pid: u32) -> Option<(u64, f64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: f64 = fields.get(11)?.parse::<f64>().ok()? + fields.get(12)?.parse::<f64>().ok()?;
    Some((rss, ticks / 100.0))
}

fn resources(stack: &Stack) -> BTreeMap<String, (u64, f64)> {
    stack
        .pids()
        .into_iter()
        .filter_map(|(n, pid)| proc_usage(pid).map(|u| (n, u)))
        .collect()
}

fn resource_delta(
    before: &BTreeMap<String, (u64, f64)>,
    after: &BTreeMap<String, (u64, f64)>,
) -> Value {
    after
        .iter()
        .map(|(n, (rss, cpu))| {
            let cpu0 = before.get(n).map(|b| b.1).unwrap_or(0.0);
            (
                n.clone(),
                json!({ "rssMiB": (*rss as f64 / 1024.0 * 10.0).round() / 10.0,
                        "cpuSeconds": ((cpu - cpu0).max(0.0) * 100.0).round() / 100.0 }),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn hardware() -> Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|c| {
            c.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    let mem = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0);
    json!({
        "cpuModel": cpu,
        "logicalCpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "memoryGiB": (mem as f64 / 1024.0 / 1024.0 * 10.0).round() / 10.0,
        "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim(),
        "binaries": std::env::var("CFMEET_BIN_DIR").unwrap_or_default(),
        "build": "unoptimized debug binaries unless CFMEET_BIN_DIR points at a release build",
        "scale": scale(),
    })
}

/// A profile's report entry plus the pass / fail of its declared criteria.
struct Report {
    name: &'static str,
    profiles: Vec<Value>,
    failures: Vec<String>,
}

impl Report {
    fn new(name: &'static str) -> Self {
        Report {
            name,
            profiles: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// Records a profile: what it measured and whether every criterion held.
    fn profile(&mut self, name: &str, criteria: &[(&str, bool)], measured: Value) {
        let list: Vec<Value> = criteria
            .iter()
            .map(|(c, ok)| json!({ "criterion": c, "passed": ok }))
            .collect();
        for (c, ok) in criteria {
            if !ok {
                self.failures.push(format!("{name}: {c}"));
            }
        }
        eprintln!(
            "LOAD {name}: {}\n      criteria: {}",
            measured,
            serde_json::to_string(&list).unwrap()
        );
        self.profiles
            .push(json!({ "profile": name, "criteria": list, "measured": measured }));
    }

    fn finish(self) {
        let report = json!({
            "suite": self.name,
            "generatedAt": chrono::Utc::now().to_rfc3339(),
            "hardware": hardware(),
            "profiles": self.profiles,
            "failures": self.failures,
        });
        let dir = std::env::var("CFMEET_LOAD_REPORT_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.json", self.name));
        std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
        eprintln!("LOAD REPORT written to {}", path.display());
        assert!(
            report["failures"].as_array().unwrap().is_empty(),
            "load / resilience criteria failed: {}",
            report["failures"]
        );
    }
}

async fn ready(stack: &Stack, service: &str) -> Resp {
    Resp::from(
        stack
            .http
            .get(format!("{}/ready", stack.url(service)))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .unwrap(),
    )
    .await
}

async fn get_status(gw: &Gateway, path: &str, secret: &str) -> Option<u16> {
    let r = gw.get(path, Auth::Credential(secret)).await;
    Some(r.status).filter(|s| *s != 0)
}

/// Waits (polling every 100 ms) until `f` holds; returns the time it took.
async fn recovery<F, Fut>(limit: Duration, mut f: F) -> Option<Duration>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < limit {
        if f().await {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

fn secs(d: Option<Duration>) -> Value {
    d.map(|d| json!((d.as_secs_f64() * 100.0).round() / 100.0))
        .unwrap_or(Value::Null)
}

// =====================================================================================
// 1. Gateway: throughput, authentication, quotas, WebSockets, Redis and domain outages
// =====================================================================================

/// Six API families every certification credential may read (side-effect free).
const FAMILIES: &[&str] = &[
    "/v1/calls",
    "/v1/messaging/policy",
    "/v1/numbers/countries",
    "/v1/sms/messages",
    "/v1/developer-platform/context",
    "/v1/privacy-relay/contexts",
];

/// TEST environments: 300 requests / minute each (the environment cap).
const TEST_RPM: f64 = 300.0;

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

async fn messaging_device(gw: &Gateway, secret: &str, principal: Uuid) -> Uuid {
    let ch = gw
        .acting(
            Method::POST,
            "/v1/messaging/devices/registration-challenges",
            Auth::Credential(secret),
            principal,
            Some(json!({})),
        )
        .await;
    status(&ch, 201, "registration challenge");
    let device_id = ch.uuid("/deviceId");
    let device = cf_messaging_crypto::device::Device::generate(device_id).unwrap();
    let nonce: [u8; 32] = hex::decode(ch.str("/nonce")).unwrap().try_into().unwrap();
    let kp = device
        .registration_key_package(ch.uuid("/challengeId"), &nonce)
        .unwrap();
    let d = gw
        .acting(
            Method::POST,
            "/v1/messaging/devices",
            Auth::Credential(secret),
            principal,
            Some(json!({ "challengeId": ch.str("/challengeId"),
                         "keyPackage": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&kp) })),
        )
        .await;
    status(&d, 201, "device registration");
    device_id
}

/// A TCP proxy between the gateway and Redis: `down()` drops every connection and
/// refuses new ones (a real Redis outage as the gateway sees it), `up()` restores.
struct RedisProxy {
    port: u16,
    up: Arc<AtomicBool>,
    kill: tokio::sync::watch::Sender<u64>,
}

impl RedisProxy {
    async fn start(redis_url: &str) -> RedisProxy {
        let rest = redis_url.trim_start_matches("redis://");
        let host = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
        let host = host.split('/').next().unwrap_or(host);
        let target = if host.contains(':') {
            host.to_string()
        } else {
            format!("{host}:6379")
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let up = Arc::new(AtomicBool::new(true));
        let (kill, _) = tokio::sync::watch::channel(0u64);
        let (up2, kill2) = (up.clone(), kill.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    continue;
                };
                if !up2.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                let target = target.clone();
                let mut killed = kill2.subscribe();
                tokio::spawn(async move {
                    let Ok(mut outbound) = tokio::net::TcpStream::connect(&target).await else {
                        return;
                    };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = killed.changed() => {}
                    }
                });
            }
        });
        RedisProxy { port, up, kill }
    }

    fn url(&self) -> String {
        format!("redis://127.0.0.1:{}", self.port)
    }

    fn down(&self) {
        self.up.store(false, Ordering::SeqCst);
        self.kill.send_modify(|g| *g += 1);
    }

    fn up(&self) {
        self.up.store(true, Ordering::SeqCst);
    }
}

async fn quota_mode(stack: &Stack) -> String {
    Resp::from(
        stack
            .http
            .get(format!("{}/ready", stack.gateway_url()))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .unwrap(),
    )
    .await
    .body["quotas"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Sleeps until the first seconds of the next UTC minute (a fresh quota window).
async fn fresh_minute() {
    let sec = chrono::Utc::now().timestamp() % 60;
    tokio::time::sleep(Duration::from_secs((61 - sec) as u64)).await;
}

#[tokio::test]
async fn gateway_load_and_resilience() {
    let Some(config) = StackConfig::from_env().expect("certification prerequisites") else {
        return;
    };
    let _g = Serial::acquire().await;
    // The gateway reaches Redis through a controllable proxy (the outage drill).
    let proxy = RedisProxy::start(&config.redis_url).await;
    let options =
        StackOptions::default().set(stack::API_GATEWAY, "API_GATEWAY_REDIS_URL", proxy.url());
    let mut stack = Stack::start_with(config.clone(), options)
        .await
        .expect("certification stack");
    let gw = stack.gateway();
    let operator = stack.operator();
    let mut report = Report::new("gateway-load-resilience");
    let k = scale();
    let strict = optimized(&config);
    let (p95_ms, p99_ms) = if strict {
        (500.0, 1500.0)
    } else {
        (10_000.0, 20_000.0)
    };

    // ---- Fixtures: 16 TEST tenants (separate quota windows), 3 credentials each.
    let tenants = 16usize;
    let mut customers: Vec<Customer> = Vec::new();
    for _ in 0..tenants {
        customers.push(
            operator
                .customer("EXTERNAL", "TEST", SANDBOX_SCOPES)
                .await
                .expect("TEST customer"),
        );
    }
    let mut extra: Vec<String> = Vec::new();
    for c in &customers {
        for _ in 0..2 {
            let (_, secret) = operator
                .credential(c.service_account_id, SANDBOX_SCOPES)
                .await
                .unwrap();
            extra.push(secret);
        }
    }

    // ---- P1: concurrent first-time authentication (Platform Access cache cold → warm),
    // with one credential revoked while the burst runs.
    let all_secrets: Vec<String> = customers
        .iter()
        .map(|c| c.secret.clone())
        .chain(extra.iter().cloned())
        .collect();
    let before = resources(&stack);
    let creds = Arc::new(all_secrets.clone());
    let gw1 = gw.clone();
    let (cold, cold_wall) = drive(creds.len(), creds.len(), None, move |i| {
        let (gw, creds) = (gw1.clone(), creds.clone());
        async move { get_status(&gw, "/v1/calls", &creds[i]).await }
    })
    .await;
    let creds = Arc::new(all_secrets.clone());
    let gw1 = gw.clone();
    let rounds = 2 * k as usize;
    let (warm, warm_wall) = drive(creds.len() * rounds, creds.len(), None, move |i| {
        let (gw, creds) = (gw1.clone(), creds.clone());
        async move { get_status(&gw, "/v1/calls", &creds[i % creds.len()]).await }
    })
    .await;
    // Revocation during concurrent traffic is effective on the very next request.
    let victim = customers[0].clone();
    let (victim_id, victim_secret) = operator
        .credential(victim.service_account_id, SANDBOX_SCOPES)
        .await
        .unwrap();
    status(
        &gw.get("/v1/calls", Auth::Credential(&victim_secret)).await,
        200,
        "victim before revocation",
    );
    let stop = Arc::new(AtomicBool::new(false));
    let background = {
        let (gw, stop, secrets) = (gw.clone(), stop.clone(), all_secrets[1..9].to_vec());
        tokio::spawn(async move {
            let mut i = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let _ = gw
                    .get("/v1/calls", Auth::Credential(&secrets[i % secrets.len()]))
                    .await;
                i += 1;
            }
        })
    };
    operator.revoke(victim_id).await.unwrap();
    let after_revoke = gw.get("/v1/calls", Auth::Credential(&victim_secret)).await;
    stop.store(true, Ordering::Relaxed);
    let _ = background.await;
    let after = resources(&stack);
    let cold_ok = cold.count(|s| s == 200) == cold.total();
    let warm_ok = warm.count(|s| s == 200) == warm.total();
    report.profile(
        "concurrent-authentication",
        &[
            ("every cold first request of 48 credentials authenticates (0 errors)", cold_ok),
            ("every warm request authenticates (0 errors)", warm_ok),
            ("warm p95 <= cold p95 (no degradation once connections and Platform Access lookups are warm)", warm.percentile(95.0) <= cold.percentile(95.0) + 5.0),
            ("a credential revoked under concurrent load is refused on the very next request (401)", after_revoke.status == 401),
        ],
        json!({ "credentials": all_secrets.len(), "cold": cold.summary(cold_wall, all_secrets.len()),
                "warm": warm.summary(warm_wall, all_secrets.len()), "revokedNextRequestStatus": after_revoke.status,
                "resources": resource_delta(&before, &after) }),
    );

    // ---- P2: mixed REST throughput across six families, paced under the quotas.
    // Every tenant's minute window was touched by P1; start on a fresh minute.
    let sec = chrono::Utc::now().timestamp() % 60;
    tokio::time::sleep(Duration::from_secs((61 - sec) as u64)).await;
    let duration_s = 30 * k as usize;
    // 90 % of the aggregate capacity (16 × 300 / min), across a 30 s window per scale step.
    let rate = 0.9 * tenants as f64 * TEST_RPM / 60.0;
    let total = (rate * duration_s as f64) as usize;
    let secrets = Arc::new(
        customers
            .iter()
            .map(|c| c.secret.clone())
            .collect::<Vec<_>>(),
    );
    let before = resources(&stack);
    let gw1 = gw.clone();
    let (mixed, mixed_wall) = drive(total, 64, Some(rate), move |i| {
        let (gw, secrets) = (gw1.clone(), secrets.clone());
        async move {
            let path = FAMILIES[i % FAMILIES.len()];
            get_status(&gw, path, &secrets[(i / FAMILIES.len()) % secrets.len()]).await
        }
    })
    .await;
    let after = resources(&stack);
    let err = 1.0 - mixed.count(|s| (200..300).contains(&s)) as f64 / mixed.total().max(1) as f64;
    report.profile(
        "gateway-mixed-rest-throughput",
        &[
            ("error rate <= 1 % (paced at 90 % of the aggregate TEST quota)", err <= 0.01),
            (if strict { "p95 latency <= 500 ms (optimized build)" } else { "p95 latency <= 10 s (debug build: correctness run)" }, mixed.percentile(95.0) <= p95_ms),
            (if strict { "p99 latency <= 1500 ms (optimized build)" } else { "p99 latency <= 20 s (debug build: correctness run)" }, mixed.percentile(99.0) <= p99_ms),
            ("no 5xx", mixed.count(|s| s >= 500) == 0),
        ],
        json!({ "optimizedBinaries": strict, "families": FAMILIES, "tenants": tenants, "targetRatePerSecond": (rate * 10.0).round() / 10.0,
                "result": mixed.summary(mixed_wall, 64), "resources": resource_delta(&before, &after) }),
    );

    // ---- P3: multi-tenant quota race — exact admission under concurrency.
    const LIMIT: u64 = 40;
    let plan = operator
        .publish_plan(
            &format!("load-race-{}", &Uuid::new_v4().simple().to_string()[..6]),
            json!({ "apiRequestsPerMinute": LIMIT, "apiRequestsPerDay": 100000 }),
        )
        .await
        .unwrap();
    // Plans are assigned to Developer Platform organizations (console-created).
    let mut racers: Vec<Customer> = Vec::new();
    for i in 0..5 {
        let sb = provision::developer_sandbox(
            &stack,
            &format!("race-{i}-{}@cert.cfmeet.test", Uuid::new_v4().simple()),
            SANDBOX_SCOPES,
        )
        .await
        .expect("race sandbox");
        let r = operator
            .assign_plan(sb.organization_id, plan, Some(sb.project_id))
            .await
            .unwrap();
        status(&r, 201, "race plan assignment");
        racers.push(sb.customer);
    }
    // The fifth limited tenant is kept for the Redis outage drill.
    let limited = racers.pop().unwrap();
    // The gateway caches entitlements briefly; wait it out, then start on a fresh minute.
    tokio::time::sleep(Duration::from_secs(35)).await;
    let sec = chrono::Utc::now().timestamp() % 60;
    tokio::time::sleep(Duration::from_secs((61 - sec) as u64)).await;
    let attempts_each = 150usize;
    let racer_secrets = Arc::new(racers.iter().map(|c| c.secret.clone()).collect::<Vec<_>>());
    let admitted: Arc<Mutex<BTreeMap<usize, u64>>> = Arc::default();
    let gw1 = gw.clone();
    let adm = admitted.clone();
    let (race, race_wall) = drive(attempts_each * racers.len(), 48, None, move |i| {
        let (gw, secrets, adm) = (gw1.clone(), racer_secrets.clone(), adm.clone());
        async move {
            let t = i % secrets.len();
            let s = get_status(&gw, FAMILIES[i % FAMILIES.len()], &secrets[t]).await;
            if s.is_some_and(|s| (200..300).contains(&s)) {
                *adm.lock().unwrap().entry(t).or_default() += 1;
            }
            s
        }
    })
    .await;
    let admitted = admitted.lock().unwrap().clone();
    let exact = (0..racers.len()).all(|t| admitted.get(&t).copied().unwrap_or(0) == LIMIT);
    let only_429 =
        race.count(|s| !(200..300).contains(&s) && s != 429) == 0 && race.transport_errors == 0;
    // A tenant outside the race is untouched by it.
    let bystander = gw
        .get("/v1/calls", Auth::Credential(&customers[13].secret))
        .await;
    report.profile(
        "multi-tenant-quota-race",
        &[
            ("each of 4 tenants admitted EXACTLY its 40/min under 48-way concurrency (no over- or under-admission)", exact),
            ("every refusal is 429 (no 5xx, no transport error)", only_429),
            ("a tenant outside the race is unaffected", bystander.status == 200),
        ],
        json!({ "limitPerMinute": LIMIT, "attemptsPerTenant": attempts_each, "admitted": admitted,
                "result": race.summary(race_wall, 48) }),
    );

    // ---- P4: WebSocket connect / reconnect / close load (Messaging realtime tickets).
    // Messaging's own limits apply per session (30 tickets / minute) and per device (5
    // connections): 10 principals with one device each, 2 connections per device.
    let wsc = &customers[14];
    allow_managed(&gw, &wsc.secret).await;
    let mut tokens = Vec::new();
    for _ in 0..10 {
        let principal = operator.principal(wsc.environment_id).await.unwrap();
        let device = messaging_device(&gw, &wsc.secret, principal).await;
        let ct = gw
            .acting(
                Method::POST,
                "/v1/messaging/client-tokens",
                Auth::Credential(&wsc.secret),
                principal,
                Some(json!({ "deviceId": device })),
            )
            .await;
        status(&ct, 201, "client token");
        tokens.push(ct.str("/token"));
    }
    let tokens = Arc::new(tokens);
    let base = Arc::new(stack.gateway_url().to_string());
    let conns = 20usize;
    let ws_rounds = 3 * k as usize;
    let connect_ms: Arc<Mutex<Vec<f64>>> = Arc::default();
    let before = resources(&stack);
    let gw1 = gw.clone();
    let cm = connect_ms.clone();
    let (wsr, ws_wall) = drive(conns * ws_rounds, conns, None, move |i| {
        let (gw, tokens, base, cm) = (gw1.clone(), tokens.clone(), base.clone(), cm.clone());
        async move {
            let token = &tokens[i % tokens.len()];
            let t = gw
                .post("/v1/realtime/tickets", Auth::ClientToken(token), json!({}))
                .await;
            if t.status != 201 {
                return Some(t.status);
            }
            let ticket = t.str("/ticket");
            let started = Instant::now();
            let proto = format!("cfmeet.messaging.v1, cfmeet.ticket.{ticket}");
            match ws::open(&format!("{base}/v1/realtime"), Some(&proto), None).await {
                Ok(Ok(mut s)) => {
                    let welcome = s.next_of("welcome", Duration::from_secs(10)).await.is_ok();
                    cm.lock()
                        .unwrap()
                        .push(started.elapsed().as_secs_f64() * 1000.0);
                    let pong = s.ping(Duration::from_secs(10)).await.is_ok();
                    let closed = s.close().await.is_ok();
                    Some(if welcome && pong && closed { 101 } else { 599 })
                }
                Ok(Err((st, _))) => Some(st),
                Err(_) => None,
            }
        }
    })
    .await;
    let after = resources(&stack);
    let mut cms = connect_ms.lock().unwrap().clone();
    cms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = cms
        .get(((cms.len() as f64 - 1.0) * 0.95).round() as usize)
        .copied()
        .unwrap_or(f64::MAX);
    let ws_ok = wsr.count(|s| s == 101);
    report.profile(
        "websocket-connect-reconnect-close",
        &[
            (
                "success (ticket → upgrade → welcome → ping → clean close) >= 99 %",
                ws_ok as f64 >= 0.99 * wsr.total() as f64,
            ),
            (
                if strict {
                    "p95 upgrade-to-welcome <= 1000 ms (optimized build)"
                } else {
                    "p95 upgrade-to-welcome <= 10 s (debug build)"
                },
                p95 <= if strict { 1000.0 } else { 10_000.0 },
            ),
        ],
        json!({ "concurrentConnections": conns, "reconnectRounds": ws_rounds, "cycles": wsr.total(),
                "successfulCycles": ws_ok, "cycle": wsr.summary(ws_wall, conns),
                "connectMs": { "p50": cms.get(cms.len() / 2), "p95": p95, "max": cms.last() },
                "resources": resource_delta(&before, &after) }),
    );

    // ---- P5: Redis outage — degraded per-instance quotas (never unlimited), recovery.
    assert_eq!(
        quota_mode(&stack).await,
        "shared",
        "quota counters start shared"
    );
    fresh_minute().await;
    proxy.down();
    let outage = Duration::from_secs(8);
    let down_at = Instant::now();
    let mut during = Samples::default();
    let mut modes = BTreeSet::new();
    let attempts = 80usize;
    for i in 0..attempts {
        let t = Instant::now();
        let s = get_status(&gw, FAMILIES[i % 5], &limited.secret).await;
        during.record(s, t.elapsed());
        if i % 20 == 0 {
            modes.insert(quota_mode(&stack).await);
        }
        let next = down_at + outage.mul_f64((i + 1) as f64 / attempts as f64);
        tokio::time::sleep_until(next.into()).await;
    }
    modes.insert(quota_mode(&stack).await);
    let admitted_degraded = during.count(|s| (200..300).contains(&s));
    proxy.up();
    // The mode is refreshed by quota calls: probe with traffic while waiting.
    let probe15 = customers[15].secret.clone();
    let back = recovery(Duration::from_secs(90), || {
        let (stack, gw, probe15) = (&stack, gw.clone(), probe15.clone());
        async move {
            let _ = gw
                .get("/v1/numbers/countries", Auth::Credential(&probe15))
                .await;
            quota_mode(stack).await == "shared"
        }
    })
    .await;
    let served = gw
        .get("/v1/calls", Auth::Credential(&customers[15].secret))
        .await;
    report.profile(
        "redis-outage",
        &[
            ("every request during an 8 s Redis outage is answered 200 / 429 (no 5xx, no transport error)", during.count(|s| s == 200 || s == 429) == during.total()),
            ("degraded quotas still bound admission: a 40/min tenant gets at most 40 of 80 attempts", admitted_degraded <= 40),
            ("the gateway keeps serving during the outage (at least one 200)", admitted_degraded >= 1),
            ("the gateway reports degraded quota mode during the outage", modes.contains("degraded")),
            ("shared counters return within 30 s of Redis returning", back.is_some_and(|d| d <= Duration::from_secs(30))),
            ("requests succeed after recovery", served.status == 200),
        ],
        json!({ "outageSeconds": outage.as_secs(), "attempts": attempts, "admittedDuringOutage": admitted_degraded,
                "limitPerMinute": LIMIT, "quotaModesSeen": modes, "during": during.summary(outage, 1),
                "sharedRestoredSeconds": secs(back) }),
    );

    // ---- P6: domain service outage — safe errors, no rerouting, others unaffected.
    // Calling's process also serves the public Privacy Relay API: both go down together
    // (same upstream, never rerouted); the other four families must stay up.
    let probe = customers[7].secret.clone();
    let independent = [
        "/v1/messaging/policy",
        "/v1/numbers/countries",
        "/v1/sms/messages",
        "/v1/developer-platform/context",
    ];
    assert!(stack.stop(stack::CALLING).await, "Calling stopped");
    let down_at = Instant::now();
    let mut calling_down = Samples::default();
    let mut relay_down = Samples::default();
    let mut others = Samples::default();
    let mut codes = BTreeSet::new();
    let mut i = 0usize;
    while down_at.elapsed() < Duration::from_secs(5) {
        let t = Instant::now();
        let r = gw.get("/v1/calls", Auth::Credential(&probe)).await;
        if let Some(c) = r.code() {
            codes.insert(c.to_string());
        }
        assert!(
            !r.text.contains("127.0.0.1"),
            "no upstream address leaks: {}",
            r.text
        );
        calling_down.record(Some(r.status), t.elapsed());
        let t = Instant::now();
        let s = get_status(&gw, "/v1/privacy-relay/contexts", &probe).await;
        relay_down.record(s, t.elapsed());
        let t = Instant::now();
        let s = get_status(&gw, independent[i % independent.len()], &probe).await;
        others.record(s, t.elapsed());
        i += 1;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let outage_s = down_at.elapsed();
    stack
        .restart(stack::CALLING, &[])
        .await
        .expect("Calling restarts");
    let restarted = Instant::now();
    let back = recovery(Duration::from_secs(60), || {
        let (gw, probe) = (gw.clone(), probe.clone());
        async move { gw.get("/v1/calls", Auth::Credential(&probe)).await.status == 200 }
    })
    .await;
    report.profile(
        "domain-service-outage",
        &[
            ("every Calling request during the outage is a safe 502 / 503 / 504 (never 200 from another domain)", calling_down.count(|s| matches!(s, 502..=504)) == calling_down.total()),
            ("Privacy Relay (served by the Calling process) fails safe the same way", relay_down.count(|s| matches!(s, 502..=504)) == relay_down.total()),
            ("the four independent families stay 200 throughout", others.count(|s| s == 200) == others.total()),
            ("Calling routes recover within 10 s of the service being ready", back.is_some_and(|d| d <= Duration::from_secs(10))),
        ],
        json!({ "outageSeconds": (outage_s.as_secs_f64() * 10.0).round() / 10.0, "callingDuringOutage": calling_down.summary(outage_s, 1),
                "errorCodes": codes, "privacyRelayDuringOutage": relay_down.summary(outage_s, 1), "independentFamilies": others.summary(outage_s, 1), "recoverySecondsAfterRestart": secs(back),
                "restartToReadySeconds": (restarted.elapsed().as_secs_f64() * 10.0).round() / 10.0 }),
    );
    // ---- P7: Platform Access outage — fail closed (503, never 200 / 401), readiness
    // reports not ready, recovery after restart.
    let probe = customers[6].secret.clone();
    status(
        &gw.get("/v1/calls", Auth::Credential(&probe)).await,
        200,
        "before the Platform Access outage",
    );
    assert!(
        stack.stop(stack::PLATFORM_ACCESS).await,
        "Platform Access stopped"
    );
    let down_at = Instant::now();
    let mut during = Samples::default();
    while down_at.elapsed() < Duration::from_secs(5) {
        let t = Instant::now();
        let s = get_status(
            &gw,
            FAMILIES[during.total() as usize % FAMILIES.len()],
            &probe,
        )
        .await;
        during.record(s, t.elapsed());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let ready_during = ready(&stack, stack::API_GATEWAY).await;
    stack
        .restart(stack::PLATFORM_ACCESS, &[])
        .await
        .expect("Platform Access restarts");
    let back = recovery(Duration::from_secs(60), || {
        let (gw, probe) = (gw.clone(), probe.clone());
        async move { gw.get("/v1/calls", Auth::Credential(&probe)).await.status == 200 }
    })
    .await;
    let ready_after = ready(&stack, stack::API_GATEWAY).await;
    report.profile(
        "platform-access-outage",
        &[
            ("every request while Platform Access is down fails closed with 503 (never 200, never 401)", during.count(|s| s == 503) == during.total()),
            ("gateway readiness reports not ready (503) during the outage", ready_during.status == 503),
            ("requests succeed within 10 s of Platform Access being ready again", back.is_some_and(|d| d <= Duration::from_secs(10))),
            ("readiness returns to ready", ready_after.status == 200),
        ],
        json!({ "outageSeconds": 5, "during": during.summary(down_at.elapsed(), 1),
                "readyDuringOutage": ready_during.status, "recoverySecondsAfterRestart": secs(back) }),
    );
    stack.teardown().await;
    report.finish();
}

// =====================================================================================
// 2. Webhooks and usage: fan-out, backlog, replay, ingestion, rebuild, outages
// =====================================================================================

fn webhook_options() -> StackOptions {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    let key = base64::engine::general_purpose::STANDARD.encode(b);
    let mut o = StackOptions {
        runtime: Some(RuntimeEnvironment::Local),
        ..StackOptions::default()
    };
    for p in [stack::DEVELOPER_PLATFORM, stack::DEVELOPER_PLATFORM_WORKER] {
        o = o
            .set(p, "DEVELOPER_PLATFORM_WEBHOOKS_ENABLED", "true")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", "active")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY", key.clone())
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID", "load-k1")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_REQUEST_TIMEOUT_MS", "5000")
            .set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
                "true",
            );
    }
    o.set(
        stack::MESSAGING,
        "MESSAGING_PUBLIC_EVENTS_DESTINATION",
        "developer-platform",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED",
        "true",
    )
}

fn envelope(c: &Customer) -> Value {
    json!({
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "messaging.conversation.created",
        "schema_version": 1,
        "occurred_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "producer_service": "cfmeet.messaging",
        "runtime_environment": "local",
        "correlation_id": format!("corr_{}", Uuid::new_v4().simple()),
        "ownership": {
            "tenant_id": c.tenant_id,
            "application_id": c.application_id,
            "application_environment_id": c.environment_id,
        },
        "payload": { "conversation_id": Uuid::new_v4(), "conversation_kind": "GROUP",
                     "security_profile": "MANAGED", "version": 1 },
    })
}

/// A signed internal call to the Developer Platform as `as_service`.
async fn internal(stack: &Stack, as_service: &str, path: &str, body: &Value) -> Option<Resp> {
    let signed = stack
        .signer(as_service)
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::developer_platform(),
        ))
        .expect("signs");
    let mut r = stack
        .http
        .post(format!("{}{path}", stack.url(stack::DEVELOPER_PLATFORM)))
        .timeout(Duration::from_secs(30));
    for (k, v) in signed.headers().iter() {
        r = r.header(k.as_str(), v.as_bytes());
    }
    match r.json(body).send().await {
        Ok(resp) => Some(Resp::from(resp).await),
        Err(_) => None,
    }
}

fn sms_fact(c: &Sandbox, source: &str) -> cf_usage_facts::UsageFact {
    let mut f = cf_usage_facts::UsageFact {
        source_fact_id: source.to_string(),
        usage_type: "SMS_SEGMENT".into(),
        unit: "SEGMENT".into(),
        quantity: 1 + (source.len() as u64 % 3),
        tenant_id: c.organization_id,
        application_id: c.project_id,
        environment_id: c.environment_id,
        environment_kind: Some("SANDBOX".into()),
        resource_ref: format!("sms:{}", Uuid::new_v4()),
        occurred_at: chrono::Utc::now() - chrono::Duration::minutes(5),
        period_start: None,
        period_end: None,
        dimensions: Default::default(),
        digest: String::new(),
    };
    f.dimensions.insert("direction".into(), json!("OUTBOUND"));
    f.normalized().sealed()
}

/// Posts `batches` of usage facts as the SMS Gateway, retrying a batch that fails (the
/// exporter's own behaviour: a batch is retried until acknowledged). Returns
/// `(recorded, duplicates, attempts that failed)`.
async fn ingest_batches(
    stack: &Stack,
    batches: &[Vec<cf_usage_facts::UsageFact>],
    concurrency: usize,
) -> (u64, u64, u64, Samples, Duration) {
    let stack_url = stack.url(stack::DEVELOPER_PLATFORM).to_string();
    let _ = stack_url;
    let recorded = Arc::new(Mutex::new((0u64, 0u64, 0u64)));
    let samples = Arc::new(Mutex::new(Samples::default()));
    let sem = Arc::new(Semaphore::new(concurrency));
    let start = Instant::now();
    let mut handles = Vec::new();
    for b in batches.iter() {
        let permit = sem.clone().acquire_owned().await.unwrap();
        let body = json!({ "facts": b });
        let signer = stack.signer(services::SMS_GATEWAY);
        let http = stack.http.clone();
        let url = format!(
            "{}/internal/v1/usage/facts",
            stack.url(stack::DEVELOPER_PLATFORM)
        );
        let (recorded, samples) = (recorded.clone(), samples.clone());
        handles.push(tokio::spawn(async move {
            loop {
                let signed = signer
                    .sign(&OutboundServiceRequest::new_operation(
                        ServiceId::developer_platform(),
                    ))
                    .expect("signs");
                let mut r = http.post(&url).timeout(Duration::from_secs(30));
                for (k, v) in signed.headers().iter() {
                    r = r.header(k.as_str(), v.as_bytes());
                }
                let t = Instant::now();
                match r.json(&body).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        let v: Value = resp.json().await.unwrap_or_default();
                        samples.lock().unwrap().record(Some(200), t.elapsed());
                        let mut g = recorded.lock().unwrap();
                        g.0 += v["recorded"].as_u64().unwrap_or(0);
                        g.1 += v["duplicates"].as_u64().unwrap_or(0);
                        break;
                    }
                    Ok(resp) if resp.status().is_client_error() => {
                        // A refusal is final (never retried): the profile fails on it.
                        let code = resp.status().as_u16();
                        let text = resp.text().await.unwrap_or_default();
                        eprintln!(
                            "usage batch refused ({code}): {}",
                            text.chars().take(300).collect::<String>()
                        );
                        samples.lock().unwrap().record(Some(code), t.elapsed());
                        recorded.lock().unwrap().2 += 1;
                        break;
                    }
                    Ok(resp) => {
                        samples
                            .lock()
                            .unwrap()
                            .record(Some(resp.status().as_u16()), t.elapsed());
                        recorded.lock().unwrap().2 += 1;
                    }
                    Err(_) => {
                        samples.lock().unwrap().record(None, t.elapsed());
                        recorded.lock().unwrap().2 += 1;
                    }
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            drop(permit);
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    let wall = start.elapsed();
    let (a, b, c) = *recorded.lock().unwrap();
    let s = std::mem::take(&mut *samples.lock().unwrap());
    (a, b, c, s, wall)
}

async fn scalar(db: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
    db::scalar_i64(db, sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Blocks every connection to `database` (terminates existing ones) — a database outage
/// as the service sees it — until [`unblock`].
async fn block(admin: &str, database: &str) {
    let conn = db::connect(admin).await.unwrap();
    conn.execute_unprepared(&format!(
        "ALTER DATABASE {database} WITH ALLOW_CONNECTIONS false; \
         SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{database}' AND pid <> pg_backend_pid();"
    ))
    .await
    .unwrap();
    conn.close().await.ok();
}

async fn unblock(admin: &str, database: &str) {
    let conn = db::connect(admin).await.unwrap();
    conn.execute_unprepared(&format!(
        "ALTER DATABASE {database} WITH ALLOW_CONNECTIONS true"
    ))
    .await
    .unwrap();
    conn.close().await.ok();
}

/// Delivery ids created by manual replays (a replay is a deliberate second delivery of
/// the same event and is excluded from the duplicate checks).
static REPLAYS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// `(subscription, event id) -> successful (2xx) receipts` at the receiver, manual
/// replays excluded.
fn successes(rx: &Receiver, paths: &[String]) -> BTreeMap<(String, String), usize> {
    let replays = REPLAYS.lock().unwrap().clone();
    let mut out: BTreeMap<(String, String), usize> = BTreeMap::new();
    for p in paths {
        for r in rx.received(p) {
            if (200..300).contains(&r.answered) && !replays.contains(r.delivery_id()) {
                *out.entry((p.clone(), r.event_id().to_string()))
                    .or_default() += 1;
            }
        }
    }
    out
}

#[tokio::test]
async fn webhook_usage_load_and_resilience() {
    let Some(config) = StackConfig::from_env().expect("certification prerequisites") else {
        return;
    };
    let _g = Serial::acquire().await;
    let mut stack = Stack::start_with(config.clone(), webhook_options())
        .await
        .expect("certification stack");
    let gw = stack.gateway();
    let mut report = Report::new("webhook-usage-load-resilience");
    let k = scale() as usize;
    let rx = Receiver::start().await.unwrap();
    let sb = provision::developer_sandbox(
        &stack,
        &format!("load-{}@cert.cfmeet.test", Uuid::new_v4().simple()),
        SANDBOX_SCOPES,
    )
    .await
    .expect("sandbox");
    let cred = sb.customer.secret.clone();
    let dp_db = stack.db(stack::DEVELOPER_PLATFORM).await.unwrap();
    let dp_name = stack
        .db_name(stack::DEVELOPER_PLATFORM)
        .expect("DP database");

    // ---- W1: fan-out with one failing endpoint (retry backlog), drained exactly once.
    let subs = 5usize;
    let events = 40 * k;
    let paths: Vec<String> = (0..subs).map(|i| format!("/h{i}")).collect();
    let mut secrets = BTreeMap::new();
    let mut webhook_ids = BTreeMap::new();
    for p in &paths {
        let r = gw
            .post(
                "/v1/webhooks",
                Auth::Credential(&cred),
                json!({ "url": rx.url(p), "eventTypes": ["conversation.created"], "description": "load" }),
            )
            .await;
        status(&r, 201, "subscribe");
        secrets.insert(p.clone(), r.str("/signingSecret"));
        webhook_ids.insert(p.clone(), r.uuid("/webhook/webhookId"));
    }
    // The first quarter of /h0's answers fail (503): a retry backlog.
    rx.script("/h0", (0..events / 4).map(|_| Reply::status(503)).collect());
    let before = resources(&stack);
    let started = Instant::now();
    let stack_ref = &stack;
    let mut ingest_samples = Samples::default();
    let mut event_ids = BTreeSet::new();
    for _ in 0..events {
        let e = envelope(&sb.customer);
        event_ids.insert(e["event_id"].as_str().unwrap().to_string());
        let t = Instant::now();
        let r = internal(
            stack_ref,
            services::MESSAGING,
            "/internal/v1/platform/events",
            &e,
        )
        .await;
        ingest_samples.record(r.as_ref().map(|r| r.status), t.elapsed());
    }
    let ingest_wall = started.elapsed();
    let expected = subs * events;
    let depth: Arc<Mutex<Vec<i64>>> = Arc::default();
    let drained = recovery(Duration::from_secs(240), || {
        let (rx, paths, dp_db, depth) = (&rx, &paths, &dp_db, depth.clone());
        async move {
            let pending = scalar(dp_db, "SELECT count(*) FROM devplat_webhook_deliveries WHERE status IN ('PENDING','RETRY_SCHEDULED','DELIVERING')").await;
            depth.lock().unwrap().push(pending);
            successes(rx, paths).len() >= expected
        }
    })
    .await;
    let depth = depth.lock().unwrap().clone();
    let after = resources(&stack);
    let got = successes(&rx, &paths);
    let dupes = got.values().filter(|n| **n > 1).count();
    let signatures_ok = paths.iter().all(|p| {
        rx.received(p)
            .iter()
            .filter(|r| (200..300).contains(&r.answered))
            .all(|r| r.verify(&secrets[p]).is_ok())
    });
    report.profile(
        "webhook-fan-out-and-retry-backlog",
        &[
            ("every event ingested (202/200)", ingest_samples.count(|s| (200..300).contains(&s)) == events as u64),
            ("all subs × events delivered within 240 s (incl. 30 s-class retry backoff)", drained.is_some()),
            ("exactly one successful delivery per (subscription, event)", got.len() == expected && dupes == 0),
            ("every delivered request carries a valid signature", signatures_ok),
        ],
        json!({ "subscriptions": subs, "events": events, "logicalDeliveries": expected,
                "failingAnswersInjected": events / 4, "ingest": ingest_samples.summary(ingest_wall, 1),
                "drainSeconds": secs(drained), "maxQueueDepth": depth.iter().max(), "duplicates": dupes,
                "resources": resource_delta(&before, &after) }),
    );

    // ---- W2: replay volume — N concurrent replays, each a NEW delivery of the SAME event.
    let list = gw
        .get(
            &format!("/v1/webhooks/{}/deliveries?limit=100", webhook_ids["/h2"]),
            Auth::Credential(&cred),
        )
        .await;
    let replay_ids: Vec<(String, String)> = if list.status == 200 {
        list.body["data"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    }
    .into_iter()
    .filter(|d| d["status"] == "SUCCEEDED")
    .take(30)
    .map(|d| {
        (
            d["deliveryId"].as_str().unwrap_or_default().to_string(),
            d["eventId"].as_str().unwrap_or_default().to_string(),
        )
    })
    .collect();
    let replays = Arc::new(replay_ids.clone());
    let replayed: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
    let (gw1, cred1, rep) = (gw.clone(), cred.clone(), replayed.clone());
    let (rp, rp_wall) = drive(replays.len(), 10, None, move |i| {
        let (gw, cred, replays, rep) = (gw1.clone(), cred1.clone(), replays.clone(), rep.clone());
        async move {
            let (d, e) = &replays[i];
            let r = gw
                .post(
                    &format!("/v1/webhook-deliveries/{d}/replay"),
                    Auth::Credential(&cred),
                    json!({}),
                )
                .await;
            if r.status == 201 {
                rep.lock()
                    .unwrap()
                    .push((d.clone(), r.str("/deliveryId"), e.clone()));
                assert_eq!(r.str("/eventId"), *e);
            }
            Some(r.status)
        }
    })
    .await;
    let replayed = replayed.lock().unwrap().clone();
    let new_ids: BTreeSet<&String> = replayed.iter().map(|(_, n, _)| n).collect();
    REPLAYS
        .lock()
        .unwrap()
        .extend(replayed.iter().map(|(_, n, _)| n.clone()));
    let replay_delivered = recovery(Duration::from_secs(60), || {
        let (rx, replayed) = (&rx, &replayed);
        async move {
            let got: BTreeSet<String> = rx
                .all()
                .iter()
                .map(|r| r.delivery_id().to_string())
                .collect();
            replayed.iter().all(|(_, n, _)| got.contains(n))
        }
    })
    .await;
    report.profile(
        "webhook-replay-volume",
        &[
            ("at least 25 delivered deliveries were available to replay", replay_ids.len() >= 25),
            ("every replay accepted (201) with a new delivery id and the same event id", rp.count(|s| s == 201) == replay_ids.len() as u64 && new_ids.len() == replay_ids.len()),
            ("every replay delivered within 60 s", replay_delivered.is_some()),
        ],
        json!({ "replays": replay_ids.len(), "result": rp.summary(rp_wall, 10), "deliveredSeconds": secs(replay_delivered) }),
    );

    // ---- W3: usage ingestion throughput, then a full replay (zero double counting).
    let sb2 = provision::developer_sandbox(
        &stack,
        &format!("load-usage-{}@cert.cfmeet.test", Uuid::new_v4().simple()),
        &["usage.read"],
    )
    .await
    .expect("second sandbox");
    let envs = [&sb, &sb2];
    let facts_total = 20_000 * k;
    let batch = 500usize;
    let run_id = Uuid::new_v4().simple().to_string();
    let batches: Vec<Vec<cf_usage_facts::UsageFact>> = (0..facts_total / batch)
        .map(|b| {
            (0..batch)
                .map(|i| sms_fact(envs[b % 2], &format!("load:{run_id}:{b}:{i}")))
                .collect()
        })
        .collect();
    let before = resources(&stack);
    let (recorded, dup0, fails0, s1, w1) = ingest_batches(&stack, &batches, 4).await;
    let (rec2, dup2, _, s2, w2) = ingest_batches(&stack, &batches, 4).await;
    let after = resources(&stack);
    let in_ledger = scalar(
        &dp_db,
        &format!(
            "SELECT count(*) FROM devplat_usage_facts WHERE source_fact_id LIKE 'load:{run_id}:%'"
        ),
    )
    .await;
    report.profile(
        "usage-ingestion-throughput-and-replay",
        &[
            ("every fact recorded once on first delivery", recorded == facts_total as u64 && dup0 == 0 && fails0 == 0),
            ("a full replay records nothing new (all duplicates)", rec2 == 0 && dup2 == facts_total as u64),
            ("ledger holds exactly one fact per producer source id", in_ledger == facts_total as i64),
            ("ingestion >= 500 facts / s", facts_total as f64 / w1.as_secs_f64() >= 500.0),
        ],
        json!({ "facts": facts_total, "batchSize": batch, "concurrency": 4,
                "factsPerSecond": ((facts_total as f64 / w1.as_secs_f64()) * 10.0).round() / 10.0,
                "replayFactsPerSecond": ((facts_total as f64 / w2.as_secs_f64()) * 10.0).round() / 10.0,
                "firstPass": s1.summary(w1, 4), "replay": s2.summary(w2, 4),
                "resources": resource_delta(&before, &after) }),
    );

    // ---- W4: daily rollup rebuild under concurrent ingestion.
    let run2 = Uuid::new_v4().simple().to_string();
    let more: Vec<Vec<cf_usage_facts::UsageFact>> = (0..(10_000 * k) / batch)
        .map(|b| {
            (0..batch)
                .map(|i| sms_fact(envs[b % 2], &format!("load:{run2}:{b}:{i}")))
                .collect()
        })
        .collect();
    let operator = stack.operator();
    let today = chrono::Utc::now().date_naive().to_string();
    let rebuild_started = Instant::now();
    let (ingest, rebuild) = tokio::join!(ingest_batches(&stack, &more, 4), async {
        let mut times = Vec::new();
        for _ in 0..3 {
            let t = Instant::now();
            let r = operator
                .dp(
                    Method::POST,
                    "/operator/usage/rollups/rebuild",
                    Some(json!({ "from": today, "to": today })),
                )
                .await
                .unwrap();
            times.push((r.status, t.elapsed().as_secs_f64()));
        }
        times
    });
    let _ = ingest;
    // A final rebuild after ingestion settles: rollups equal the facts.
    let r = operator
        .dp(
            Method::POST,
            "/operator/usage/rollups/rebuild",
            Some(json!({ "from": today, "to": today })),
        )
        .await
        .unwrap();
    let mismatch = scalar(
        &dp_db,
        &format!(
            "SELECT count(*) FROM (SELECT environment_id, usage_type, usage_day, SUM(quantity) q, count(*) n \
               FROM devplat_usage_facts WHERE usage_day = '{today}' GROUP BY 1,2,3) f \
             FULL JOIN (SELECT environment_id, usage_type, usage_day, quantity q, fact_count n \
               FROM devplat_usage_rollups_daily WHERE usage_day = '{today}') r USING (environment_id, usage_type, usage_day) \
             WHERE f.q IS DISTINCT FROM r.q OR f.n IS DISTINCT FROM r.n"
        ),
    )
    .await;
    report.profile(
        "rollup-rebuild-under-load",
        &[
            ("every rebuild during ingestion succeeds", rebuild.iter().all(|(s, _)| *s == 200) && r.status == 200),
            ("rollups equal the facts afterwards (0 mismatching rows)", mismatch == 0),
        ],
        json!({ "concurrentFacts": 10_000 * k, "rebuildsDuringIngestion": rebuild, "mismatchingRows": mismatch,
                "wallSeconds": (rebuild_started.elapsed().as_secs_f64() * 10.0).round() / 10.0 }),
    );

    // ---- W5: Developer Platform database outage during deliveries and ingestion.
    let wave = 20usize;
    // The Developer Platform issues its own public event ids: progress is counted as
    // NEW successful (subscription, event) pairs.
    let before_wave = successes(&rx, &paths).len();
    let mut wave_events = BTreeSet::new();
    for _ in 0..wave {
        let e = envelope(&sb.customer);
        wave_events.insert(e["event_id"].as_str().unwrap().to_string());
        let r = internal(
            &stack,
            services::MESSAGING,
            "/internal/v1/platform/events",
            &e,
        )
        .await;
        assert!(
            r.is_some_and(|r| (200..300).contains(&r.status)),
            "wave event accepted"
        );
    }
    let run3 = Uuid::new_v4().simple().to_string();
    let outage_batches: Vec<Vec<cf_usage_facts::UsageFact>> = (0..8)
        .map(|b| {
            (0..batch)
                .map(|i| sms_fact(&sb, &format!("load:{run3}:{b}:{i}")))
                .collect()
        })
        .collect();
    let outage_for = Duration::from_secs(10);
    block(&config.admin_url, &dp_name).await;
    let blocked_at = Instant::now();
    let ready_during = ready(&stack, stack::DEVELOPER_PLATFORM).await;
    let ingest_task = {
        let stack_ptr: &Stack = &stack;
        ingest_batches(stack_ptr, &outage_batches, 2)
    };
    let unblock_after = async {
        tokio::time::sleep(outage_for).await;
        unblock(&config.admin_url, &dp_name).await;
        Instant::now()
    };
    let ((rec3, dup3, fails3, _, _), unblocked_at) = tokio::join!(ingest_task, unblock_after);
    let ready_back = recovery(Duration::from_secs(60), || {
        let stack = &stack;
        async move { ready(stack, stack::DEVELOPER_PLATFORM).await.status == 200 }
    })
    .await;
    let all_paths = paths.clone();
    let wave_target = before_wave + wave_events.len() * all_paths.len();
    let wave_done = recovery(Duration::from_secs(240), || {
        let (rx, all_paths) = (&rx, &all_paths);
        async move { successes(rx, all_paths).len() >= wave_target }
    })
    .await;
    let got = successes(&rx, &paths);
    let dupes = got.values().filter(|n| **n > 1).count();
    let facts3 = scalar(
        &dp_db,
        &format!(
            "SELECT count(*) FROM devplat_usage_facts WHERE source_fact_id LIKE 'load:{run3}:%'"
        ),
    )
    .await;
    report.profile(
        "developer-platform-database-outage",
        &[
            ("readiness reports not ready (503) while its database is unavailable", ready_during.status == 503),
            ("readiness recovers within 30 s of the database returning", ready_back.is_some_and(|d| d <= Duration::from_secs(30))),
            ("ingestion attempted during the outage is retried and recorded exactly once", rec3 + dup3 == 8 * batch as u64 && facts3 == 8 * batch as i64),
            ("webhook deliveries of the wave complete after recovery", wave_done.is_some()),
            ("no duplicate successful delivery across the outage", dupes == 0),
        ],
        json!({ "outageSeconds": outage_for.as_secs(), "readyDuringOutage": ready_during.status,
                "readyRecoverySeconds": secs(ready_back), "ingestRetriesDuringOutage": fails3,
                "factsRecorded": facts3, "waveDeliveredSecondsAfterUnblock": secs(wave_done),
                "blockedFor": (unblocked_at.duration_since(blocked_at).as_secs_f64() * 10.0).round() / 10.0, "duplicates": dupes }),
    );

    // ---- W6: worker interruption in the middle of a backlog.
    rx.script("/h1", (0..10).map(|_| Reply::status(503)).collect());
    let before_backlog = successes(&rx, &paths).len();
    let mut backlog_events = BTreeSet::new();
    for _ in 0..wave {
        let e = envelope(&sb.customer);
        backlog_events.insert(e["event_id"].as_str().unwrap().to_string());
        internal(
            &stack,
            services::MESSAGING,
            "/internal/v1/platform/events",
            &e,
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        stack.stop(stack::DEVELOPER_PLATFORM_WORKER).await,
        "worker stopped"
    );
    let stopped_pending = scalar(&dp_db, "SELECT count(*) FROM devplat_webhook_deliveries WHERE status IN ('PENDING','RETRY_SCHEDULED','DELIVERING')").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let received_while_stopped = rx.all().len();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let quiet = rx.all().len() == received_while_stopped;
    stack
        .restart(stack::DEVELOPER_PLATFORM_WORKER, &[])
        .await
        .expect("worker restarts");
    let resumed = Instant::now();
    let backlog_target = before_backlog + backlog_events.len() * all_paths.len();
    let backlog_done = recovery(Duration::from_secs(300), || {
        let (rx, all_paths) = (&rx, &all_paths);
        async move { successes(rx, all_paths).len() >= backlog_target }
    })
    .await;
    let got = successes(&rx, &paths);
    let dupes = got.values().filter(|n| **n > 1).count();
    report.profile(
        "worker-interruption",
        &[
            ("nothing is sent while the worker is down", quiet),
            ("the backlog completes after the worker restarts (leases expire, claims resume)", backlog_done.is_some()),
            ("no duplicate successful delivery across the interruption", dupes == 0),
        ],
        json!({ "backlogWhenStopped": stopped_pending, "completedSecondsAfterRestart": secs(backlog_done),
                "restartSeconds": (resumed.elapsed().as_secs_f64() * 10.0).round() / 10.0, "duplicates": dupes }),
    );
    stack.teardown().await;
    report.finish();
}

//! Load evidence reporter (Part K §108). A [`LoadRun`] collects per-operation latency
//! samples and errors while a background sampler watches the test database
//! (`pg_stat_activity` connections of this database, waiting `pg_locks`), then prints ONE
//! machine-readable line per scenario:
//!
//! ```text
//! LOAD <scenario> env=<env> duration_ms=… concurrency=… ops=… throughput_per_s=… p50_ms=…
//!      p95_ms=… p99_ms=… errors=… error_rate=… max_db_connections=… lock_waits=… worker_lag_ms=…
//! ```
//!
//! (on one line). Numbers are evidence, not assertions: scenarios assert correctness and
//! conservative upper bounds only (no timing thresholds that could flake).
//! `env` names the machine: `cpu<N>-pg<version>-<debug|release>`. `worker_lag_ms` is `na`
//! where no worker is involved. `lock_waits` is the sum, over all samples, of lock
//! requests that were waiting in this database (plus deadlocks counted by
//! `pg_stat_database` during the run).

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};

/// Sampling period of the database observer.
const SAMPLE_EVERY: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, PartialEq)]
pub struct LoadReport {
    pub scenario: String,
    pub env: String,
    pub duration_ms: f64,
    pub concurrency: usize,
    pub ops: usize,
    pub throughput_per_s: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub errors: usize,
    pub error_rate: f64,
    pub max_db_connections: i64,
    pub lock_waits: i64,
    pub worker_lag_ms: Option<f64>,
}

impl LoadReport {
    pub fn line(&self) -> String {
        format!(
            "LOAD {} env={} duration_ms={:.1} concurrency={} ops={} throughput_per_s={:.1} \
             p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} errors={} error_rate={:.4} \
             max_db_connections={} lock_waits={} worker_lag_ms={}",
            self.scenario,
            self.env,
            self.duration_ms,
            self.concurrency,
            self.ops,
            self.throughput_per_s,
            self.p50_ms,
            self.p95_ms,
            self.p99_ms,
            self.errors,
            self.error_rate,
            self.max_db_connections,
            self.lock_waits,
            self.worker_lag_ms
                .map(|l| format!("{l:.1}"))
                .unwrap_or_else(|| "na".to_string()),
        )
    }
}

/// Nearest-rank percentile of sorted samples (ms).
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

struct Sampler {
    stop: Arc<AtomicBool>,
    max_connections: Arc<AtomicI64>,
    lock_waits: Arc<AtomicI64>,
    task: tokio::task::JoinHandle<()>,
    conn: DatabaseConnection,
    deadlocks_before: i64,
}

async fn one_i64(conn: &DatabaseConnection, sql: &str) -> i64 {
    conn.query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0)
}

impl Sampler {
    async fn start(db_url: &str) -> Self {
        let mut opts = sea_orm::ConnectOptions::new(db_url.to_string());
        opts.max_connections(1)
            .min_connections(1)
            .sqlx_logging(false);
        let conn = Database::connect(opts).await.expect("sampler connects");
        let deadlocks_before = one_i64(
            &conn,
            "SELECT deadlocks FROM pg_stat_database WHERE datname = current_database()",
        )
        .await;
        let stop = Arc::new(AtomicBool::new(false));
        let max_connections = Arc::new(AtomicI64::new(0));
        let lock_waits = Arc::new(AtomicI64::new(0));
        let task = {
            let (stop, max, waits, conn) = (
                stop.clone(),
                max_connections.clone(),
                lock_waits.clone(),
                conn.clone(),
            );
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(SAMPLE_EVERY);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                while !stop.load(Ordering::SeqCst) {
                    tick.tick().await;
                    // Connections of THIS database other than the sampler's own.
                    let used = one_i64(
                        &conn,
                        "SELECT count(*) FROM pg_stat_activity \
                          WHERE datname = current_database() AND pid <> pg_backend_pid() \
                            AND backend_type = 'client backend'",
                    )
                    .await;
                    max.fetch_max(used, Ordering::SeqCst);
                    let waiting = one_i64(
                        &conn,
                        "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
                          WHERE NOT l.granted AND a.datname = current_database()",
                    )
                    .await;
                    waits.fetch_add(waiting, Ordering::SeqCst);
                }
            })
        };
        Self {
            stop,
            max_connections,
            lock_waits,
            task,
            conn,
            deadlocks_before,
        }
    }

    async fn finish(self) -> (i64, i64) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.task.await;
        let deadlocks = one_i64(
            &self.conn,
            "SELECT deadlocks FROM pg_stat_database WHERE datname = current_database()",
        )
        .await
            - self.deadlocks_before;
        let _ = self.conn.close().await;
        (
            self.max_connections.load(Ordering::SeqCst),
            self.lock_waits.load(Ordering::SeqCst) + deadlocks.max(0),
        )
    }
}

/// The machine a measurement was taken on (no spaces).
pub async fn environment(conn: &DatabaseConnection) -> String {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let version = conn
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SHOW server_version".to_string(),
        ))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get_by_index::<String>(0).ok())
        .unwrap_or_else(|| "unknown".into());
    let version: String = version
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string();
    format!(
        "cpu{cpus}-pg{version}-{}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    )
}

/// One measured scenario.
pub struct LoadRun {
    scenario: String,
    env: String,
    concurrency: usize,
    samples: Mutex<Vec<f64>>,
    errors: AtomicUsize,
    started: Instant,
    sampler: Mutex<Option<Sampler>>,
}

impl LoadRun {
    /// Starts the clock and the database sampler on `db_url` (the test database).
    pub async fn start(scenario: &str, concurrency: usize, db_url: &str) -> Self {
        let sampler = Sampler::start(db_url).await;
        let env = environment(&sampler.conn).await;
        Self {
            scenario: scenario.to_string(),
            env,
            concurrency,
            samples: Mutex::new(Vec::new()),
            errors: AtomicUsize::new(0),
            started: Instant::now(),
            sampler: Mutex::new(Some(sampler)),
        }
    }

    /// Records one operation of `elapsed`.
    pub fn record(&self, elapsed: Duration, ok: bool) {
        self.samples
            .lock()
            .unwrap()
            .push(elapsed.as_secs_f64() * 1000.0);
        if !ok {
            self.errors.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Times `fut` as one operation; `ok` classifies its output.
    pub async fn op<T>(&self, fut: impl Future<Output = T>, ok: impl FnOnce(&T) -> bool) -> T {
        let started = Instant::now();
        let out = fut.await;
        self.record(started.elapsed(), ok(&out));
        out
    }

    pub fn errors(&self) -> usize {
        self.errors.load(Ordering::SeqCst)
    }

    /// Stops the run, prints its LOAD line and returns the report.
    pub async fn finish(self, worker_lag: Option<Duration>) -> LoadReport {
        let elapsed = self.started.elapsed();
        let sampler = self.sampler.lock().unwrap().take();
        let (max_db_connections, lock_waits) = match sampler {
            Some(s) => s.finish().await,
            None => (0, 0),
        };
        let mut samples = self.samples.lock().unwrap().clone();
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let ops = samples.len();
        let errors = self.errors.load(Ordering::SeqCst);
        let secs = elapsed.as_secs_f64().max(1e-9);
        let report = LoadReport {
            scenario: self.scenario,
            env: self.env,
            duration_ms: secs * 1000.0,
            concurrency: self.concurrency,
            ops,
            throughput_per_s: ops as f64 / secs,
            p50_ms: percentile(&samples, 50.0),
            p95_ms: percentile(&samples, 95.0),
            p99_ms: percentile(&samples, 99.0),
            errors,
            error_rate: if ops == 0 {
                0.0
            } else {
                errors as f64 / ops as f64
            },
            max_db_connections,
            lock_waits,
            worker_lag_ms: worker_lag.map(|l| l.as_secs_f64() * 1000.0),
        };
        println!("{}", report.line());
        report
    }
}

/// Runs `make(i)` for `i in 0..n`, at most `concurrency` at a time, each timed as one
/// operation of `run`. Returns the outputs in index order.
pub async fn run_bounded<T, F, Fut>(
    run: &LoadRun,
    n: usize,
    concurrency: usize,
    make: F,
    ok: impl Fn(&T) -> bool + Copy,
) -> Vec<T>
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = T>,
{
    use futures_util::stream::{self, StreamExt};
    let mut out: Vec<(usize, T)> = stream::iter(0..n)
        .map(|i| {
            let fut = make(i);
            async move { (i, run.op(fut, ok).await) }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await;
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, t)| t).collect()
}

//! Platform-wide API request quotas (Part VI §51 / §54 §10.3), enforced at the gateway
//! AFTER Platform Access authentication, per application environment, aggregated across
//! every service family.
//!
//! * Limits: the Developer Platform's effective entitlement snapshot
//!   (`GET /internal/v1/entitlements/environments/{id}`, `developer-entitlement.read`),
//!   cached briefly; while it cannot be read the environment-kind defaults apply (logged).
//! * Counters: one Lua script per request against the shared Redis — an atomic check-
//!   and-increment of the per-minute window and the UTC-day counter — so every gateway
//!   instance sees the same totals. The day counter of an environment is seeded from the
//!   Developer Platform's durable count the first time an instance sees that day (a Redis
//!   restart never resets a daily quota).
//! * Durability: accepted requests are accumulated per (environment, day) and flushed to
//!   the Developer Platform (`POST /internal/v1/usage/api-requests`,
//!   `developer-usage.record`) in idempotent batches; a failed flush is retried.
//! * Redis outage: the gateway degrades to per-instance counters (never unlimited) and
//!   reports `quotas: degraded` in readiness.
//!
//! Business / domain quotas (concurrent calls, spend, numbers, SMS, attachments) stay in
//! the owning services.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use cf_service_auth::{
    client::InternalServiceClient, ApplicationEnvironmentKind, CorrelationId,
    OutboundServiceRequest, ServiceId,
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

/// Effective limits of an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub per_minute: u64,
    pub per_day: u64,
    pub minute_level: &'static str,
    pub day_level: &'static str,
}

impl Limits {
    /// Environment-kind defaults (identical to the Developer Platform's defaults).
    pub fn default_for(kind: ApplicationEnvironmentKind) -> Self {
        let (m, d) = match kind {
            ApplicationEnvironmentKind::Sandbox => (120, 10_000),
            ApplicationEnvironmentKind::Test => (300, 50_000),
            ApplicationEnvironmentKind::Production => (600, 100_000),
        };
        Self {
            per_minute: m,
            per_day: d,
            minute_level: "PLATFORM_DEFAULT",
            day_level: "PLATFORM_DEFAULT",
        }
    }
}

fn level(v: &Value) -> &'static str {
    match v.as_str() {
        Some("ORGANIZATION_PLAN") => "ORGANIZATION_PLAN",
        Some("PROJECT_PLAN") => "PROJECT_PLAN",
        Some("ENVIRONMENT") => "ENVIRONMENT",
        _ => "PLATFORM_DEFAULT",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Minute,
    Day,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed {
        limits: Limits,
        remaining_minute: u64,
        reset_seconds: u64,
    },
    Exceeded {
        window: Window,
        level: &'static str,
        retry_after_seconds: u64,
        limits: Limits,
    },
}

/// Where effective entitlements and durable daily counts live (the Developer Platform).
#[async_trait::async_trait]
pub trait QuotaAuthority: Send + Sync {
    /// `kind` is the environment kind Platform Access confirmed for this request.
    async fn limits(
        &self,
        environment: Uuid,
        kind: ApplicationEnvironmentKind,
        correlation: &CorrelationId,
    ) -> Option<Limits>;
    async fn durable_count(&self, environment: Uuid, day: &str) -> Option<u64>;
    /// Records one idempotent batch (`batch` is reused verbatim on retry).
    async fn record(&self, batch: Uuid, usage: &UsageBatch) -> bool;
}

/// `(environment, UTC day, domain, operation id, status class)`.
type OperationKey = (Uuid, String, String, String, &'static str);

/// One normalized API_OPERATION count (Appendix D): an accepted public operation of an
/// application environment by owning domain, contract operation id and status class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationCount {
    pub environment: Uuid,
    pub day: String,
    pub domain: String,
    pub operation: String,
    pub status_class: &'static str,
    pub count: u64,
}

/// What one durable accounting batch carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageBatch {
    /// `(environment, UTC day, accepted requests)`.
    pub entries: Vec<UsageEntry>,
    pub operations: Vec<OperationCount>,
}

impl UsageBatch {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.operations.is_empty()
    }
}

/// The status class of a final response.
pub fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

/// The Developer Platform over authenticated internal HTTP.
pub struct DeveloperPlatformQuotas {
    client: Arc<InternalServiceClient>,
}

fn kind_name(kind: ApplicationEnvironmentKind) -> &'static str {
    match kind {
        ApplicationEnvironmentKind::Sandbox => "SANDBOX",
        ApplicationEnvironmentKind::Test => "TEST",
        ApplicationEnvironmentKind::Production => "PRODUCTION",
    }
}

impl DeveloperPlatformQuotas {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl QuotaAuthority for DeveloperPlatformQuotas {
    async fn limits(
        &self,
        environment: Uuid,
        kind: ApplicationEnvironmentKind,
        correlation: &CorrelationId,
    ) -> Option<Limits> {
        let req = OutboundServiceRequest::continuing(ServiceId::developer_platform(), correlation);
        let r = self
            .client
            .get_query(
                &req,
                &format!("/internal/v1/entitlements/environments/{environment}"),
                &[("kind", kind_name(kind))],
            )
            .await
            .ok()?;
        let v: Value = r.json().ok()?;
        Some(Limits {
            per_minute: v.get("apiRequestsPerMinute")?.as_u64()?,
            per_day: v.get("apiRequestsPerDay")?.as_u64()?,
            minute_level: level(&v["limitingLevel"]["apiRequestsPerMinute"]),
            day_level: level(&v["limitingLevel"]["apiRequestsPerDay"]),
        })
    }

    async fn durable_count(&self, environment: Uuid, day: &str) -> Option<u64> {
        let req = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        let r = self
            .client
            .get(
                &req,
                &format!("/internal/v1/usage/api-requests/{environment}/{day}"),
            )
            .await
            .ok()?;
        r.json::<Value>().ok()?.get("count")?.as_u64()
    }

    async fn record(&self, batch: Uuid, usage: &UsageBatch) -> bool {
        let req = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        let body = json!({
            "batchId": batch,
            "entries": usage.entries.iter().map(|(e, d, c)| json!({ "environmentId": e, "day": d, "count": c })).collect::<Vec<_>>(),
            "operations": usage.operations.iter().map(|o| json!({
                "environmentId": o.environment, "day": o.day, "domain": o.domain,
                "operationId": o.operation, "statusClass": o.status_class, "count": o.count,
            })).collect::<Vec<_>>(),
        });
        self.client
            .post_json(&req, "/internal/v1/usage/api-requests", &body)
            .await
            .is_ok()
    }
}

const SCRIPT: &str = r#"
if ARGV[5] ~= '-1' then redis.call('SET', KEYS[2], ARGV[5], 'NX', 'EX', ARGV[4]) end
local m = tonumber(redis.call('GET', KEYS[1]) or '0')
local d = tonumber(redis.call('GET', KEYS[2]) or '0')
if m >= tonumber(ARGV[1]) then return {0, m, d, 1} end
if d >= tonumber(ARGV[2]) then return {0, m, d, 2} end
m = redis.call('INCR', KEYS[1])
if m == 1 then redis.call('EXPIRE', KEYS[1], ARGV[3]) end
d = redis.call('INCR', KEYS[2])
if d == 1 then redis.call('EXPIRE', KEYS[2], ARGV[4]) end
return {1, m, d, 0}
"#;

/// `(environment, UTC day, accepted count)`.
type UsageEntry = (Uuid, String, u64);

struct CachedLimits {
    at: Instant,
    limits: Limits,
}

/// The quota engine shared by every request.
pub struct QuotaEngine {
    redis: Option<redis::aio::ConnectionManager>,
    script: redis::Script,
    prefix: String,
    authority: Arc<dyn QuotaAuthority>,
    cache: Mutex<HashMap<Uuid, CachedLimits>>,
    cache_ttl: Duration,
    seeded: Mutex<HashSet<(Uuid, String)>>,
    local: Mutex<HashMap<String, u64>>,
    pending: Mutex<HashMap<(Uuid, String), u64>>,
    pending_operations: Mutex<HashMap<OperationKey, u64>>,
    unacknowledged: Mutex<Option<(Uuid, UsageBatch)>>,
    degraded: std::sync::atomic::AtomicBool,
}

impl QuotaEngine {
    pub fn new(
        redis: Option<redis::aio::ConnectionManager>,
        prefix: String,
        authority: Arc<dyn QuotaAuthority>,
    ) -> Self {
        Self {
            redis,
            script: redis::Script::new(SCRIPT),
            prefix,
            authority,
            cache: Mutex::new(HashMap::new()),
            cache_ttl: Duration::from_secs(30),
            seeded: Mutex::new(HashSet::new()),
            local: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            pending_operations: Mutex::new(HashMap::new()),
            unacknowledged: Mutex::new(None),
            degraded: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// `shared` (Redis), `degraded` (Redis unreachable: per-instance) or `local` (no
    /// Redis configured).
    pub fn mode(&self) -> &'static str {
        match (
            &self.redis,
            self.degraded.load(std::sync::atomic::Ordering::Relaxed),
        ) {
            (None, _) => "local",
            (Some(_), true) => "degraded",
            (Some(_), false) => "shared",
        }
    }

    async fn limits_for(
        &self,
        environment: Uuid,
        kind: ApplicationEnvironmentKind,
        correlation: &CorrelationId,
    ) -> Limits {
        if let Some(c) = self.cache.lock().unwrap().get(&environment) {
            if c.at.elapsed() < self.cache_ttl {
                return c.limits;
            }
        }
        match self.authority.limits(environment, kind, correlation).await {
            Some(limits) => {
                self.cache.lock().unwrap().insert(
                    environment,
                    CachedLimits {
                        at: Instant::now(),
                        limits,
                    },
                );
                limits
            }
            None => {
                // The last known snapshot, else the conservative environment defaults.
                let stale = self
                    .cache
                    .lock()
                    .unwrap()
                    .get(&environment)
                    .map(|c| c.limits);
                tracing::warn!(
                    "entitlements unavailable; using the last snapshot or the environment defaults"
                );
                stale.unwrap_or_else(|| Limits::default_for(kind))
            }
        }
    }

    /// Checks and (when allowed) counts one request of `environment`.
    pub async fn check(
        &self,
        environment: Uuid,
        kind: ApplicationEnvironmentKind,
        correlation: &CorrelationId,
    ) -> Decision {
        let limits = self.limits_for(environment, kind, correlation).await;
        let now = Utc::now();
        let minute = now.timestamp() / 60;
        let day = now.format("%Y-%m-%d").to_string();
        let reset = (60 - (now.timestamp() % 60)).max(1) as u64;
        let until_tomorrow = (86_400 - (now.timestamp() % 86_400)).max(1) as u64;
        if limits.per_minute == 0 || limits.per_day == 0 {
            return Decision::Exceeded {
                window: if limits.per_minute == 0 {
                    Window::Minute
                } else {
                    Window::Day
                },
                level: limits.minute_level,
                retry_after_seconds: reset,
                limits,
            };
        }
        let outcome = match &self.redis {
            Some(conn) => {
                let key = (environment, day.clone());
                let seed = if self.seeded.lock().unwrap().contains(&key) {
                    -1i64
                } else {
                    self.authority
                        .durable_count(environment, &day)
                        .await
                        .map(|c| c as i64)
                        .unwrap_or(-1)
                };
                let mut conn = conn.clone();
                let r: Result<Vec<i64>, _> = self
                    .script
                    .key(format!("{}rpm:{environment}:{minute}", self.prefix))
                    .key(format!("{}rpd:{environment}:{day}", self.prefix))
                    .arg(limits.per_minute)
                    .arg(limits.per_day)
                    .arg(120)
                    .arg(172_800)
                    .arg(seed)
                    .invoke_async(&mut conn)
                    .await;
                match r {
                    Ok(v) if v.len() == 4 => {
                        self.degraded
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        if seed >= 0 {
                            self.seeded.lock().unwrap().insert(key);
                        }
                        Some((v[0] == 1, v[1].max(0) as u64, v[3]))
                    }
                    _ => {
                        if !self
                            .degraded
                            .swap(true, std::sync::atomic::Ordering::Relaxed)
                        {
                            tracing::warn!(
                                "quota store unreachable: per-instance quota counters in use"
                            );
                        }
                        None
                    }
                }
            }
            None => None,
        };
        let (allowed, used_minute, which) = match outcome {
            Some(o) => o,
            None => self.local_check(environment, minute, &day, &limits),
        };
        if allowed {
            *self
                .pending
                .lock()
                .unwrap()
                .entry((environment, day))
                .or_insert(0) += 1;
            Decision::Allowed {
                limits,
                remaining_minute: limits.per_minute.saturating_sub(used_minute),
                reset_seconds: reset,
            }
        } else if which == 2 {
            Decision::Exceeded {
                window: Window::Day,
                level: limits.day_level,
                retry_after_seconds: until_tomorrow,
                limits,
            }
        } else {
            Decision::Exceeded {
                window: Window::Minute,
                level: limits.minute_level,
                retry_after_seconds: reset,
                limits,
            }
        }
    }

    fn local_check(
        &self,
        environment: Uuid,
        minute: i64,
        day: &str,
        limits: &Limits,
    ) -> (bool, u64, i64) {
        let mut local = self.local.lock().unwrap();
        let mk = format!("m:{environment}:{minute}");
        let dk = format!("d:{environment}:{day}");
        let m = *local.get(&mk).unwrap_or(&0);
        let d = *local.get(&dk).unwrap_or(&0);
        if m >= limits.per_minute {
            return (false, m, 1);
        }
        if d >= limits.per_day {
            return (false, m, 2);
        }
        local.insert(mk, m + 1);
        local.insert(dk, d + 1);
        // Bounded memory: drop windows older than this minute / day.
        if local.len() > 100_000 {
            local.retain(|k, _| {
                k.ends_with(&format!(":{minute}")) || k.ends_with(&format!(":{day}"))
            });
        }
        (true, m + 1, 0)
    }

    /// Counts one finally classified public operation of a credential request
    /// (normalized API_OPERATION usage), flushed with the request counters.
    pub fn record_operation(&self, environment: Uuid, domain: &str, operation: &str, status: u16) {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let mut p = self.pending_operations.lock().unwrap();
        // Bounded memory: at most one entry per (environment, day, operation, class).
        if p.len() >= 100_000 {
            tracing::warn!("API operation usage buffer full; counting deferred to the next flush");
            return;
        }
        *p.entry((
            environment,
            day,
            domain.to_string(),
            operation.to_string(),
            status_class(status),
        ))
        .or_insert(0) += 1;
    }

    /// Flushes accumulated accepted counts to the durable store (idempotent batch). A
    /// batch whose outcome is unknown (timeout, outage) is retried under the SAME batch
    /// id until acknowledged, so it is counted exactly once; new counts wait behind it.
    /// Returns the number of entries (request counters + operation counts) recorded.
    pub async fn flush(&self) -> usize {
        let retry = self.unacknowledged.lock().unwrap().clone();
        if let Some((batch, usage)) = retry {
            if !self.authority.record(batch, &usage).await {
                tracing::warn!(
                    "durable API request accounting deferred (developer platform unavailable)"
                );
                return 0;
            }
            *self.unacknowledged.lock().unwrap() = None;
        }
        let usage = UsageBatch {
            entries: {
                let mut p = self.pending.lock().unwrap();
                p.drain().map(|((e, d), c)| (e, d, c)).collect()
            },
            operations: {
                let mut p = self.pending_operations.lock().unwrap();
                p.drain()
                    .map(
                        |((environment, day, domain, operation, status_class), count)| {
                            OperationCount {
                                environment,
                                day,
                                domain,
                                operation,
                                status_class,
                                count,
                            }
                        },
                    )
                    .collect()
            },
        };
        if usage.is_empty() {
            return 0;
        }
        let batch = Uuid::new_v4();
        if self.authority.record(batch, &usage).await {
            usage.entries.len() + usage.operations.len()
        } else {
            tracing::warn!(
                "durable API request accounting deferred (developer platform unavailable)"
            );
            *self.unacknowledged.lock().unwrap() = Some((batch, usage));
            0
        }
    }

    /// Runs [`flush`](Self::flush) every `interval` forever.
    pub async fn run_flusher(self: Arc<Self>, interval: Duration) {
        loop {
            tokio::time::sleep(interval).await;
            self.flush().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Option<Limits>);

    #[async_trait::async_trait]
    impl QuotaAuthority for Fixed {
        async fn limits(
            &self,
            _e: Uuid,
            _k: ApplicationEnvironmentKind,
            _c: &CorrelationId,
        ) -> Option<Limits> {
            self.0
        }
        async fn durable_count(&self, _e: Uuid, _d: &str) -> Option<u64> {
            Some(0)
        }
        async fn record(&self, _b: Uuid, _u: &UsageBatch) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn local_counters_enforce_the_minute_window_per_environment() {
        let limits = Limits {
            per_minute: 3,
            per_day: 100,
            minute_level: "ENVIRONMENT",
            day_level: "ENVIRONMENT",
        };
        let q = QuotaEngine::new(None, "t:".into(), Arc::new(Fixed(Some(limits))));
        let env = Uuid::new_v4();
        let c = CorrelationId::generate();
        let k = ApplicationEnvironmentKind::Sandbox;
        for _ in 0..3 {
            assert!(matches!(
                q.check(env, k, &c).await,
                Decision::Allowed { .. }
            ));
        }
        assert!(matches!(
            q.check(env, k, &c).await,
            Decision::Exceeded {
                window: Window::Minute,
                ..
            }
        ));
        // Another environment has its own budget.
        assert!(matches!(
            q.check(Uuid::new_v4(), k, &c).await,
            Decision::Allowed { .. }
        ));
        assert_eq!(q.mode(), "local");
        assert_eq!(q.flush().await, 2);
    }

    #[tokio::test]
    async fn unavailable_entitlements_fall_back_to_the_environment_defaults() {
        let q = QuotaEngine::new(None, "t:".into(), Arc::new(Fixed(None)));
        match q
            .check(
                Uuid::new_v4(),
                ApplicationEnvironmentKind::Sandbox,
                &CorrelationId::generate(),
            )
            .await
        {
            Decision::Allowed { limits, .. } => assert_eq!(limits.per_minute, 120),
            d => panic!("{d:?}"),
        }
    }

    #[tokio::test]
    async fn a_suspended_snapshot_denies_everything() {
        let limits = Limits {
            per_minute: 0,
            per_day: 0,
            minute_level: "ORGANIZATION_PLAN",
            day_level: "ORGANIZATION_PLAN",
        };
        let q = QuotaEngine::new(None, "t:".into(), Arc::new(Fixed(Some(limits))));
        assert!(matches!(
            q.check(
                Uuid::new_v4(),
                ApplicationEnvironmentKind::Production,
                &CorrelationId::generate()
            )
            .await,
            Decision::Exceeded { .. }
        ));
    }

    /// Fails the first `fail` record calls; remembers every batch id it saw.
    struct Flaky {
        fail: Mutex<usize>,
        seen: Mutex<Vec<(Uuid, u64)>>,
        durable: u64,
    }

    #[async_trait::async_trait]
    impl QuotaAuthority for Flaky {
        async fn limits(
            &self,
            _e: Uuid,
            _k: ApplicationEnvironmentKind,
            _c: &CorrelationId,
        ) -> Option<Limits> {
            Some(Limits {
                per_minute: 1000,
                per_day: 10,
                minute_level: "ENVIRONMENT",
                day_level: "ENVIRONMENT",
            })
        }
        async fn durable_count(&self, _e: Uuid, _d: &str) -> Option<u64> {
            Some(self.durable)
        }
        async fn record(&self, b: Uuid, u: &UsageBatch) -> bool {
            self.seen
                .lock()
                .unwrap()
                .push((b, u.entries.iter().map(|x| x.2).sum()));
            // Fail while the remaining-failures counter is positive.
            let mut remaining = self.fail.lock().unwrap();
            match remaining.checked_sub(1) {
                Some(n) => {
                    *remaining = n;
                    false
                }
                None => true,
            }
        }
    }

    #[tokio::test]
    async fn an_unacknowledged_batch_is_retried_under_the_same_id() {
        let a = Arc::new(Flaky {
            fail: Mutex::new(2),
            seen: Mutex::new(vec![]),
            durable: 0,
        });
        let q = QuotaEngine::new(None, "t:".into(), a.clone());
        let env = Uuid::new_v4();
        let c = CorrelationId::generate();
        for _ in 0..3 {
            q.check(env, ApplicationEnvironmentKind::Sandbox, &c).await;
        }
        assert_eq!(q.flush().await, 0);
        q.check(env, ApplicationEnvironmentKind::Sandbox, &c).await;
        assert_eq!(q.flush().await, 0);
        // Acknowledged now: the retried batch first, then the newer counts in a new batch.
        assert_eq!(q.flush().await, 1);
        let seen = a.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 4);
        assert!(seen[0] == seen[1] && seen[1] == seen[2], "{seen:?}");
        assert_eq!(seen[0].1, 3);
        assert_ne!(seen[3].0, seen[0].0);
        assert_eq!(seen[3].1, 1);
    }

    /// Shared atomic Redis counters: two gateway instances enforce ONE budget, and the
    /// day window is re-seeded from the durable total after a counter-store loss.
    #[tokio::test]
    async fn redis_counters_are_shared_across_instances_and_reseeded() {
        let Ok(url) = std::env::var("TEST_REDIS_URL") else {
            return;
        };
        let client = redis::Client::open(url).unwrap();
        let conn = redis::aio::ConnectionManager::new(client).await.unwrap();
        let prefix = format!("cfgw-test:{}:", Uuid::new_v4());
        let mk = |durable| {
            QuotaEngine::new(
                Some(conn.clone()),
                prefix.clone(),
                Arc::new(Flaky {
                    fail: Mutex::new(0),
                    seen: Mutex::new(vec![]),
                    durable,
                }),
            )
        };
        let (a, b) = (mk(4), mk(4));
        assert_eq!(a.mode(), "shared");
        let env = Uuid::new_v4();
        let c = CorrelationId::generate();
        let k = ApplicationEnvironmentKind::Sandbox;
        // 4 already recorded durably today: 6 left of 10, shared by both instances.
        let mut allowed = 0;
        for i in 0..10 {
            let q = if i % 2 == 0 { &a } else { &b };
            if matches!(q.check(env, k, &c).await, Decision::Allowed { .. }) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 6);
        assert!(matches!(
            a.check(env, k, &c).await,
            Decision::Exceeded {
                window: Window::Day,
                ..
            }
        ));
    }
}

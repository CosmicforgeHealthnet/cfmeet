//! Registered callers, their product boundary and their resource limits.
//!
//! Identity comes ONLY from a verified service token (its issuer); a product name is never
//! read from a request. Every registered client must hold `security-scan.submit` in the
//! receiver-authoritative trust bundle, and every issuer the bundle trusts must be
//! registered here, so neither file can silently widen the other.
//!
//! ```json
//! {"version": 1, "environment": "production", "clients": [
//!   {"issuer": "cfmeet.messaging", "product": "cfmeet",
//!    "maxBytes": 26214400, "maxConcurrent": 2, "maxPerMinute": 120}]}
//! ```

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use cf_service_auth::{InternalPermission, RuntimeEnvironment, ServiceId, TrustPolicy};
use serde::Deserialize;
use tokio::sync::Semaphore;

pub const REGISTRY_VERSION: u32 = 1;
pub const MAX_CLIENTS: usize = 32;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawClient {
    issuer: String,
    product: String,
    max_bytes: u64,
    max_concurrent: usize,
    max_per_minute: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistry {
    version: u32,
    environment: String,
    clients: Vec<RawClient>,
}

/// One registered caller with its own concurrency and rate budget.
#[derive(Debug)]
pub struct Client {
    pub issuer: ServiceId,
    pub product: String,
    pub max_bytes: u64,
    pub max_concurrent: usize,
    pub max_per_minute: u32,
    pub slots: Arc<Semaphore>,
    window: Mutex<(Instant, u32)>,
}

impl Client {
    /// Fixed one-minute window; refuses once the budget is spent.
    pub fn admit(&self, now: Instant) -> bool {
        let mut w = self.window.lock().expect("rate window");
        if now.duration_since(w.0) >= Duration::from_secs(60) {
            *w = (now, 0);
        }
        if w.1 >= self.max_per_minute {
            return false;
        }
        w.1 += 1;
        true
    }
}

#[derive(Debug)]
pub struct Registry {
    clients: BTreeMap<ServiceId, Arc<Client>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("client registry: {0}")]
pub struct RegistryError(pub String);

fn valid_product(p: &str) -> bool {
    (2..=32).contains(&p.len())
        && p.as_bytes()[0].is_ascii_lowercase()
        && p.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Registry {
    pub fn from_json(
        json: &str,
        environment: RuntimeEnvironment,
        policy: &TrustPolicy,
        backend_stream_max_bytes: u64,
        backend_max_concurrency: usize,
    ) -> Result<Self, RegistryError> {
        let e = |m: String| RegistryError(m);
        let raw: RawRegistry = serde_json::from_str(json).map_err(|x| e(x.to_string()))?;
        if raw.version != REGISTRY_VERSION {
            return Err(e(format!("unsupported version {}", raw.version)));
        }
        // Production and staging are separate trust domains: a registry is for exactly one.
        if RuntimeEnvironment::parse(&raw.environment).ok() != Some(environment) {
            return Err(e("environment does not match this gateway".into()));
        }
        if raw.clients.is_empty() || raw.clients.len() > MAX_CLIENTS {
            return Err(e(format!("expected 1–{MAX_CLIENTS} clients")));
        }
        let mut clients = BTreeMap::new();
        for c in raw.clients {
            let issuer = ServiceId::parse(&c.issuer)
                .map_err(|_| e(format!("invalid issuer {}", c.issuer)))?;
            if !valid_product(&c.product) {
                return Err(e(format!("invalid product for {issuer}")));
            }
            if c.max_bytes == 0 || c.max_bytes > backend_stream_max_bytes {
                return Err(e(format!(
                    "maxBytes of {issuer} must be 1–{backend_stream_max_bytes} (the engine's StreamMaxLength)"
                )));
            }
            if c.max_concurrent == 0 || c.max_concurrent > backend_max_concurrency {
                return Err(e(format!(
                    "maxConcurrent of {issuer} must be 1–{backend_max_concurrency}"
                )));
            }
            if c.max_per_minute == 0 || c.max_per_minute > 10_000 {
                return Err(e(format!("maxPerMinute of {issuer} must be 1–10000")));
            }
            let granted = policy.grant_for(&issuer).is_some_and(|g| {
                g.permissions()
                    .contains(InternalPermission::SecurityScanSubmit)
            });
            if !granted {
                return Err(e(format!(
                    "{issuer} is not granted security-scan.submit by the trust bundle"
                )));
            }
            let client = Client {
                issuer: issuer.clone(),
                product: c.product,
                max_bytes: c.max_bytes,
                max_concurrent: c.max_concurrent,
                max_per_minute: c.max_per_minute,
                slots: Arc::new(Semaphore::new(c.max_concurrent)),
                window: Mutex::new((Instant::now(), 0)),
            };
            if clients.insert(issuer.clone(), Arc::new(client)).is_some() {
                return Err(e(format!("{issuer} is registered twice")));
            }
        }
        for grant in policy.grants() {
            if !clients.contains_key(grant.issuer()) {
                return Err(e(format!(
                    "trusted issuer {} has no registry entry",
                    grant.issuer()
                )));
            }
        }
        Ok(Self { clients })
    }

    pub fn get(&self, issuer: &ServiceId) -> Option<Arc<Client>> {
        self.clients.get(issuer).cloned()
    }

    pub fn products(&self) -> impl Iterator<Item = &str> {
        self.clients.values().map(|c| c.product.as_str())
    }
}

/// Single-use service tokens: a verified token id is accepted once within its lifetime.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    seen: Mutex<BTreeMap<(String, String), i64>>,
}

pub const REPLAY_CAPACITY: usize = 50_000;

impl ReplayGuard {
    /// `false` when the token was already used, or when the guard is full of live tokens
    /// (fail closed rather than forgetting).
    pub fn first_use(&self, issuer: &str, token_id: &str, expires_at: i64, now: i64) -> bool {
        let mut seen = self.seen.lock().expect("replay guard");
        if seen.len() >= REPLAY_CAPACITY {
            seen.retain(|_, exp| *exp >= now);
            if seen.len() >= REPLAY_CAPACITY {
                return false;
            }
        }
        let key = (issuer.to_string(), token_id.to_string());
        if seen.contains_key(&key) {
            return false;
        }
        seen.insert(key, expires_at.max(now) + 60);
        true
    }
}

//! Service token replay guard (SEC-012).
//!
//! A token presented on a MUTATING request (`POST`, `PUT`, `PATCH`, `DELETE`) is
//! single-use: the receiver records `(issuer, jti)` until the token can no longer verify
//! (`exp` + clock skew) and refuses a second presentation. Safe methods are not
//! replay-checked (a captured `GET` token gains nothing it did not already have).
//!
//! Signers mint a fresh 128-bit random `jti` for every token and the internal client signs
//! every call afresh (a retry is a new hop with a new request id and a new token), so a
//! legitimate caller never presents the same mutating token twice.
//!
//! [`ReplayGuard`] is the storage seam: [`InMemoryReplayGuard`] (bounded by entry count,
//! per process) is the default; a service with a shared store (e.g. Redis) plugs in an
//! implementation shared by all its replicas.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
};

use crate::{identity::ServiceId, ids::TokenId};

/// Env switch for the replay guard: `on` (default) or `off`.
pub const ENV_REPLAY_GUARD: &str = "SERVICE_AUTH_REPLAY_GUARD";
/// Env override of the default in-memory capacity (entries).
pub const ENV_REPLAY_CACHE_CAPACITY: &str = "SERVICE_AUTH_REPLAY_CACHE_CAPACITY";
/// Default in-memory capacity. At 120 s tokens this covers > 800 mutating calls per
/// second per process before any unexpired entry is evicted.
pub const DEFAULT_REPLAY_CACHE_CAPACITY: usize = 100_000;

/// Identity of one presented token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReplayKey {
    pub issuer: ServiceId,
    pub token_id: TokenId,
}

impl ReplayKey {
    /// Stable string form for external stores (`<issuer>:<jti>`; both are restricted
    /// alphabets without `:` in the jti).
    pub fn storage_key(&self) -> String {
        format!("{}:{}", self.issuer.as_str(), self.token_id.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayVerdict {
    /// First presentation; now recorded.
    FirstUse,
    /// Already presented.
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("replay store unavailable")]
pub struct ReplayStoreError;

pub type ReplayFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ReplayVerdict, ReplayStoreError>> + Send + 'a>>;

/// Atomically records the first use of a token and reports replays.
pub trait ReplayGuard: Send + Sync + fmt::Debug {
    /// Records `key` until `retain_until` (unix seconds; never earlier than the token's
    /// `exp` plus skew) unless already recorded. `now` is the verifier's clock.
    fn check_and_record<'a>(
        &'a self,
        key: &'a ReplayKey,
        retain_until: i64,
        now: i64,
    ) -> ReplayFuture<'a>;
}

#[derive(Debug, Default)]
struct Entries {
    /// key -> (retain_until, sequence)
    by_key: HashMap<ReplayKey, (i64, u64)>,
    /// (retain_until, sequence) -> key: expiry order for pruning and eviction.
    by_expiry: BTreeMap<(i64, u64), ReplayKey>,
    sequence: u64,
    evicted_live: u64,
}

/// Bounded in-memory replay cache (per process). Entries expire at their
/// `retain_until`; when the cache is full of live entries the one closest to expiry is
/// evicted (counted in [`ReplayCacheStats::evicted_live`]) so memory stays bounded and the
/// receiver stays available.
pub struct InMemoryReplayGuard {
    capacity: usize,
    entries: Mutex<Entries>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayCacheStats {
    pub entries: usize,
    pub capacity: usize,
    /// Unexpired entries evicted because the cache was full (a replay window).
    pub evicted_live: u64,
}

impl fmt::Debug for InMemoryReplayGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryReplayGuard")
            .field("stats", &self.stats())
            .finish()
    }
}

impl InMemoryReplayGuard {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Mutex::new(Entries::default()),
        }
    }

    /// The process-wide default guard (shared by every verifier of the process, so a
    /// service with several verifiers still sees each token once). Capacity from
    /// [`ENV_REPLAY_CACHE_CAPACITY`], else [`DEFAULT_REPLAY_CACHE_CAPACITY`].
    pub fn process_default() -> Arc<Self> {
        static DEFAULT: OnceLock<Arc<InMemoryReplayGuard>> = OnceLock::new();
        DEFAULT
            .get_or_init(|| {
                let capacity = std::env::var(ENV_REPLAY_CACHE_CAPACITY)
                    .ok()
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .filter(|c| *c > 0)
                    .unwrap_or(DEFAULT_REPLAY_CACHE_CAPACITY);
                Arc::new(Self::new(capacity))
            })
            .clone()
    }

    pub fn stats(&self) -> ReplayCacheStats {
        let e = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        ReplayCacheStats {
            entries: e.by_key.len(),
            capacity: self.capacity,
            evicted_live: e.evicted_live,
        }
    }

    /// Synchronous check-and-record (the trait method wraps this).
    pub fn first_use(&self, key: &ReplayKey, retain_until: i64, now: i64) -> ReplayVerdict {
        let mut e = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        // Prune expired entries (bounded work: each entry is removed once).
        while let Some((&(until, seq), _)) = e.by_expiry.first_key_value() {
            if until > now {
                break;
            }
            if let Some(k) = e.by_expiry.remove(&(until, seq)) {
                e.by_key.remove(&k);
            }
        }
        if e.by_key.contains_key(key) {
            return ReplayVerdict::Replayed;
        }
        while e.by_key.len() >= self.capacity {
            let Some((_, k)) = e.by_expiry.pop_first() else {
                break;
            };
            e.by_key.remove(&k);
            e.evicted_live += 1;
        }
        e.sequence += 1;
        let seq = e.sequence;
        e.by_key.insert(key.clone(), (retain_until, seq));
        e.by_expiry.insert((retain_until, seq), key.clone());
        ReplayVerdict::FirstUse
    }
}

impl ReplayGuard for InMemoryReplayGuard {
    fn check_and_record<'a>(
        &'a self,
        key: &'a ReplayKey,
        retain_until: i64,
        now: i64,
    ) -> ReplayFuture<'a> {
        let verdict = self.first_use(key, retain_until, now);
        Box::pin(std::future::ready(Ok(verdict)))
    }
}

/// Whether the replay guard is enabled ([`ENV_REPLAY_GUARD`], default on).
pub fn replay_guard_enabled_from_env() -> Result<bool, crate::binding::BindingConfigError> {
    match std::env::var(ENV_REPLAY_GUARD) {
        Ok(v) if !v.trim().is_empty() => match v.trim().to_ascii_lowercase().as_str() {
            "on" => Ok(true),
            "off" => Ok(false),
            _ => Err(crate::binding::BindingConfigError {
                variable: ENV_REPLAY_GUARD,
                expected: "on|off",
            }),
        },
        _ => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ReplayKey {
        ReplayKey {
            issuer: ServiceId::calling(),
            token_id: TokenId::generate(),
        }
    }

    #[test]
    fn second_use_is_a_replay_until_retention_ends() {
        let g = InMemoryReplayGuard::new(10);
        let k = key();
        assert_eq!(g.first_use(&k, 1_150, 1_000), ReplayVerdict::FirstUse);
        assert_eq!(g.first_use(&k, 1_150, 1_000), ReplayVerdict::Replayed);
        assert_eq!(g.first_use(&k, 1_150, 1_149), ReplayVerdict::Replayed);
        // After retention the entry is pruned (the verifier refuses the token as
        // expired by then anyway).
        assert_eq!(g.first_use(&k, 1_300, 1_150), ReplayVerdict::FirstUse);
    }

    #[test]
    fn same_jti_from_another_issuer_is_distinct() {
        let g = InMemoryReplayGuard::new(10);
        let k = key();
        let other = ReplayKey {
            issuer: ServiceId::telephony(),
            token_id: k.token_id.clone(),
        };
        assert_eq!(g.first_use(&k, 100, 0), ReplayVerdict::FirstUse);
        assert_eq!(g.first_use(&other, 100, 0), ReplayVerdict::FirstUse);
    }

    #[test]
    fn cache_is_bounded_and_evicts_expired_first_then_closest_to_expiry() {
        let g = InMemoryReplayGuard::new(3);
        let (a, b, c, d) = (key(), key(), key(), key());
        assert_eq!(g.first_use(&a, 50, 0), ReplayVerdict::FirstUse);
        assert_eq!(g.first_use(&b, 200, 0), ReplayVerdict::FirstUse);
        assert_eq!(g.first_use(&c, 300, 0), ReplayVerdict::FirstUse);
        assert_eq!(g.stats().entries, 3);
        // `a` expired at 50: pruned, no live eviction.
        assert_eq!(g.first_use(&d, 400, 60), ReplayVerdict::FirstUse);
        assert_eq!(g.stats().entries, 3);
        assert_eq!(g.stats().evicted_live, 0);
        // Full of live entries: the one closest to expiry (`b`) is evicted.
        let e = key();
        assert_eq!(g.first_use(&e, 500, 60), ReplayVerdict::FirstUse);
        assert_eq!(
            g.stats(),
            ReplayCacheStats {
                entries: 3,
                capacity: 3,
                evicted_live: 1
            }
        );
        assert_eq!(g.first_use(&c, 300, 60), ReplayVerdict::Replayed);
        assert_eq!(g.first_use(&d, 400, 60), ReplayVerdict::Replayed);
        assert_eq!(g.first_use(&e, 500, 60), ReplayVerdict::Replayed);
    }

    #[test]
    fn storage_key_is_issuer_and_jti() {
        let k = key();
        assert_eq!(
            k.storage_key(),
            format!("cfmeet.calling:{}", k.token_id.as_str())
        );
    }
}

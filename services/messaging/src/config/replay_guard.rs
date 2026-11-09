//! Shared service-token replay guard (SEC-012) on the realtime Redis.
//!
//! When `MESSAGING_REALTIME_REDIS_URL` is configured, every Messaging replica records the
//! `(issuer, jti)` of service tokens presented on mutating internal requests with one
//! atomic `SET key 1 NX EX ttl`, so a token replayed against ANOTHER replica is refused
//! too. The process-local bounded cache is always consulted first (a same-replica replay
//! never needs Redis).
//!
//! Redis is not the authority for anything in Messaging: if it is unreachable the guard
//! degrades to the process-local cache (counted in
//! `messaging_service_replay_store_degraded_total`) instead of refusing internal traffic.
//! Tokens still expire within 300 s, are audience- and request-bound, and travel only
//! over the private network with TLS.

use std::{sync::Arc, time::Duration};

use cf_service_auth::{
    replay::ReplayFuture, InMemoryReplayGuard, ReplayGuard, ReplayKey, ReplayVerdict,
};

/// Key namespace in the shared Redis.
pub const KEY_PREFIX: &str = "cfmeet:svc-jti:";

pub struct RedisReplayGuard {
    client: redis::Client,
    conn: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
    local: Arc<InMemoryReplayGuard>,
}

impl std::fmt::Debug for RedisReplayGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisReplayGuard")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

impl RedisReplayGuard {
    pub fn new(url: &str, local: Arc<InMemoryReplayGuard>) -> Result<Self, String> {
        Ok(Self {
            client: redis::Client::open(url).map_err(|_| "invalid Redis URL".to_string())?,
            conn: tokio::sync::Mutex::new(None),
            local,
        })
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection, ()> {
        let mut g = self.conn.lock().await;
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let c = tokio::time::timeout(
            Duration::from_secs(2),
            self.client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        *g = Some(c.clone());
        Ok(c)
    }

    /// `Some(true)` first use, `Some(false)` already recorded, `None` store unavailable.
    async fn shared_first_use(&self, key: &ReplayKey, ttl_seconds: u64) -> Option<bool> {
        let mut c = self.conn().await.ok()?;
        let mut cmd = redis::cmd("SET");
        cmd.arg(format!("{KEY_PREFIX}{}", key.storage_key()))
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(ttl_seconds);
        let reply: Result<redis::RedisResult<Option<String>>, _> =
            tokio::time::timeout(Duration::from_secs(2), cmd.query_async(&mut c)).await;
        match reply {
            Ok(Ok(reply)) => Some(reply.is_some()),
            _ => {
                *self.conn.lock().await = None;
                None
            }
        }
    }
}

impl ReplayGuard for RedisReplayGuard {
    fn check_and_record<'a>(
        &'a self,
        key: &'a ReplayKey,
        retain_until: i64,
        now: i64,
    ) -> ReplayFuture<'a> {
        Box::pin(async move {
            if self.local.first_use(key, retain_until, now) == ReplayVerdict::Replayed {
                return Ok(ReplayVerdict::Replayed);
            }
            let ttl = u64::try_from(retain_until.saturating_sub(now))
                .unwrap_or(0)
                .max(1);
            match self.shared_first_use(key, ttl).await {
                Some(true) => Ok(ReplayVerdict::FirstUse),
                Some(false) => Ok(ReplayVerdict::Replayed),
                None => {
                    crate::metrics::inc("messaging_service_replay_store_degraded_total", &[]);
                    tracing::warn!(
                        "service-token replay store unavailable; process-local guard only"
                    );
                    Ok(ReplayVerdict::FirstUse)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cf_service_auth::{ServiceId, TokenId};

    /// Two replicas (separate local caches) sharing one Redis: a token used on one is a
    /// replay on the other. Runs when `TEST_REDIS_URL` is set (skipped otherwise).
    #[tokio::test]
    async fn replicas_share_first_use_through_redis() {
        let Ok(url) = std::env::var("TEST_REDIS_URL") else {
            eprintln!("TEST_REDIS_URL not set: skipping");
            return;
        };
        let replica =
            || RedisReplayGuard::new(&url, Arc::new(InMemoryReplayGuard::new(8))).unwrap();
        let (a, b) = (replica(), replica());
        let key = ReplayKey {
            issuer: ServiceId::api_gateway(),
            token_id: TokenId::generate(),
        };
        assert_eq!(
            a.check_and_record(&key, 1_150, 1_000).await,
            Ok(ReplayVerdict::FirstUse)
        );
        assert_eq!(
            b.check_and_record(&key, 1_150, 1_000).await,
            Ok(ReplayVerdict::Replayed)
        );
        let other = ReplayKey {
            issuer: ServiceId::api_gateway(),
            token_id: TokenId::generate(),
        };
        assert_eq!(
            b.check_and_record(&other, 1_150, 1_000).await,
            Ok(ReplayVerdict::FirstUse)
        );
    }

    #[tokio::test]
    async fn degrades_to_the_local_guard_when_redis_is_unreachable() {
        // Nothing listens on port 1: every shared check fails fast.
        let guard = RedisReplayGuard::new(
            "redis://127.0.0.1:1/",
            Arc::new(InMemoryReplayGuard::new(8)),
        )
        .unwrap();
        let key = ReplayKey {
            issuer: ServiceId::api_gateway(),
            token_id: TokenId::generate(),
        };
        assert_eq!(
            guard.check_and_record(&key, 1_150, 1_000).await,
            Ok(ReplayVerdict::FirstUse)
        );
        assert_eq!(
            guard.check_and_record(&key, 1_150, 1_001).await,
            Ok(ReplayVerdict::Replayed)
        );
    }
}

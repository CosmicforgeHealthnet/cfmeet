//! Ephemeral, expiring realtime state (Part V §61–§63): which devices of a principal
//! are currently connected. Never durable history, never a message store; entries
//! expire on their own (a crashed gateway's devices disappear after the TTL).
//!
//! * [`MemoryPresence`] — one instance (tests, local development).
//! * [`RedisPresence`] — shared across gateway instances: one sorted set per principal
//!   (`member = device id`, `score = expiry in ms`), pruned on every access.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use uuid::Uuid;

#[derive(Debug, Clone, thiserror::Error)]
#[error("ephemeral store unavailable: {0}")]
pub struct EphemeralError(pub String);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The presence key of a principal (environment-scoped: no cross-tenant collision).
pub fn principal_key(environment_id: Uuid, principal_id: Uuid) -> String {
    format!(
        "cfmeet:messaging:presence:v1:{}:{}",
        environment_id.simple(),
        principal_id.simple()
    )
}

#[async_trait::async_trait]
pub trait PresenceStore: Send + Sync {
    fn backend(&self) -> &'static str;
    /// Marks `device` connected for `ttl`; returns whether the principal had no
    /// connected device before (it just came online).
    async fn touch(&self, key: &str, device: Uuid, ttl: Duration) -> Result<bool, EphemeralError>;
    /// Removes `device`; returns whether the principal has no connected device left.
    async fn remove(&self, key: &str, device: Uuid) -> Result<bool, EphemeralError>;
    /// Whether each principal key has at least one live device.
    async fn online(&self, keys: &[String]) -> Result<Vec<bool>, EphemeralError>;
}

#[derive(Default)]
pub struct MemoryPresence {
    entries: Mutex<HashMap<String, HashMap<Uuid, i64>>>,
}

impl MemoryPresence {
    fn prune(map: &mut HashMap<Uuid, i64>, now: i64) {
        map.retain(|_, exp| *exp > now);
    }
}

#[async_trait::async_trait]
impl PresenceStore for MemoryPresence {
    fn backend(&self) -> &'static str {
        "memory"
    }

    async fn touch(&self, key: &str, device: Uuid, ttl: Duration) -> Result<bool, EphemeralError> {
        let now = now_ms();
        let mut e = self.entries.lock().unwrap();
        let map = e.entry(key.to_string()).or_default();
        Self::prune(map, now);
        let was_empty = map.is_empty();
        map.insert(device, now + ttl.as_millis() as i64);
        Ok(was_empty)
    }

    async fn remove(&self, key: &str, device: Uuid) -> Result<bool, EphemeralError> {
        let now = now_ms();
        let mut e = self.entries.lock().unwrap();
        let empty = match e.get_mut(key) {
            Some(map) => {
                map.remove(&device);
                Self::prune(map, now);
                map.is_empty()
            }
            None => true,
        };
        if empty {
            e.remove(key);
        }
        Ok(empty)
    }

    async fn online(&self, keys: &[String]) -> Result<Vec<bool>, EphemeralError> {
        let now = now_ms();
        let e = self.entries.lock().unwrap();
        Ok(keys
            .iter()
            .map(|k| e.get(k).is_some_and(|m| m.values().any(|exp| *exp > now)))
            .collect())
    }
}

pub struct RedisPresence {
    client: redis::Client,
    conn: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
}

impl RedisPresence {
    pub fn new(url: &str) -> Result<Self, String> {
        Ok(Self {
            client: redis::Client::open(url).map_err(|_| "invalid Redis URL".to_string())?,
            conn: tokio::sync::Mutex::new(None),
        })
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection, EphemeralError> {
        let mut g = self.conn.lock().await;
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let c = tokio::time::timeout(
            Duration::from_secs(3),
            self.client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| EphemeralError("connect timeout".into()))?
        .map_err(|e| EphemeralError(format!("{:?}", e.kind())))?;
        *g = Some(c.clone());
        Ok(c)
    }

    async fn run<T: redis::FromRedisValue>(
        &self,
        pipe: &redis::Pipeline,
    ) -> Result<T, EphemeralError> {
        let mut c = self.conn().await?;
        match tokio::time::timeout(Duration::from_secs(3), pipe.query_async(&mut c)).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                *self.conn.lock().await = None;
                Err(EphemeralError(format!("{:?}", e.kind())))
            }
            Err(_) => {
                *self.conn.lock().await = None;
                Err(EphemeralError("timeout".into()))
            }
        }
    }
}

#[async_trait::async_trait]
impl PresenceStore for RedisPresence {
    fn backend(&self) -> &'static str {
        "redis"
    }

    async fn touch(&self, key: &str, device: Uuid, ttl: Duration) -> Result<bool, EphemeralError> {
        let now = now_ms();
        let mut p = redis::pipe();
        p.atomic()
            .cmd("ZREMRANGEBYSCORE")
            .arg(key)
            .arg("-inf")
            .arg(now)
            .ignore()
            .cmd("ZCARD")
            .arg(key)
            .cmd("ZADD")
            .arg(key)
            .arg(now + ttl.as_millis() as i64)
            .arg(device.simple().to_string())
            .ignore()
            .cmd("PEXPIRE")
            .arg(key)
            .arg(ttl.as_millis() as i64 * 2)
            .ignore();
        let (count,): (i64,) = self.run(&p).await?;
        Ok(count == 0)
    }

    async fn remove(&self, key: &str, device: Uuid) -> Result<bool, EphemeralError> {
        let now = now_ms();
        let mut p = redis::pipe();
        p.atomic()
            .cmd("ZREM")
            .arg(key)
            .arg(device.simple().to_string())
            .ignore()
            .cmd("ZREMRANGEBYSCORE")
            .arg(key)
            .arg("-inf")
            .arg(now)
            .ignore()
            .cmd("ZCARD")
            .arg(key);
        let (count,): (i64,) = self.run(&p).await?;
        Ok(count == 0)
    }

    async fn online(&self, keys: &[String]) -> Result<Vec<bool>, EphemeralError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let now = now_ms();
        let mut p = redis::pipe();
        for k in keys {
            p.cmd("ZCOUNT").arg(k).arg(now + 1).arg("+inf");
        }
        let counts: Vec<i64> = self.run(&p).await?;
        Ok(counts.into_iter().map(|c| c > 0).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_presence_tracks_devices_and_expires() {
        let s = MemoryPresence::default();
        let k = principal_key(Uuid::new_v4(), Uuid::new_v4());
        let (d1, d2) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(s.touch(&k, d1, Duration::from_secs(60)).await.unwrap());
        assert!(!s.touch(&k, d2, Duration::from_secs(60)).await.unwrap());
        assert!(!s.remove(&k, d1).await.unwrap());
        assert!(s.remove(&k, d2).await.unwrap());
        assert_eq!(
            s.online(std::slice::from_ref(&k)).await.unwrap(),
            vec![false]
        );
        s.touch(&k, d1, Duration::from_millis(1)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(s.online(&[k]).await.unwrap(), vec![false], "expired");
    }
}

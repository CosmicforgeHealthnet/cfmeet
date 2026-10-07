//! Realtime signal fanout (Part C §10). Provider-neutral and delivery-only: PostgreSQL
//! remains the authority, a lost signal never loses state, and signals are published only
//! after the transaction that produced them has committed.
//!
//! * [`RealtimeSignalBus`] — publish a signal for a set of endpoints; subscribe to the
//!   signals of one endpoint on this instance.
//! * [`InMemorySignalBus`] — single-process fanout (tests, single-instance development).
//! * [`PgNotifySignalBus`] — cross-instance fanout over PostgreSQL `LISTEN` / `NOTIFY` on
//!   channel `cfmeet_calling_signals`: every instance listens, publishes go through the
//!   database, each instance delivers to its own local subscribers. No Redis, no Docker.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

/// PostgreSQL channel of Calling's realtime signals.
pub const CHANNEL: &str = "cfmeet_calling_signals";
/// Keep NOTIFY payloads well below PostgreSQL's 8000-byte limit.
const MAX_PAYLOAD_BYTES: usize = 7_000;
/// Per-subscriber buffer; a subscriber that falls this far behind loses signals (and
/// recovers through the API).
const SUBSCRIBER_BUFFER: usize = 256;

/// A signal and the endpoints it is for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalEnvelope {
    pub endpoint_ids: Vec<Uuid>,
    pub signal: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignalBusError {
    #[error("signal bus unavailable")]
    Unavailable,
    #[error("signal too large")]
    TooLarge,
}

/// Receiver of one endpoint's signals on this instance.
pub struct SignalSubscription {
    pub endpoint_id: Uuid,
    receiver: mpsc::Receiver<Value>,
    _guard: SubscriptionGuard,
}

impl SignalSubscription {
    pub async fn recv(&mut self) -> Option<Value> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Option<Value> {
        self.receiver.try_recv().ok()
    }
}

#[async_trait]
pub trait RealtimeSignalBus: Send + Sync + 'static {
    /// Delivers `envelope` to every subscriber of its endpoints, on every instance.
    async fn publish(&self, envelope: SignalEnvelope) -> Result<(), SignalBusError>;
    /// Subscribes to `endpoint_id`'s signals on this instance.
    fn subscribe(&self, endpoint_id: Uuid) -> SignalSubscription;
}

/// Subscribers of each endpoint on this instance: (subscription id, sender).
type Subscribers = HashMap<Uuid, Vec<(u64, mpsc::Sender<Value>)>>;

/// Local (per-instance) subscriber registry.
#[derive(Clone, Default)]
pub struct LocalFanout {
    inner: Arc<Mutex<Subscribers>>,
    next: Arc<std::sync::atomic::AtomicU64>,
}

struct SubscriptionGuard {
    fanout: LocalFanout,
    endpoint_id: Uuid,
    id: u64,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        let mut map = self.fanout.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(list) = map.get_mut(&self.endpoint_id) {
            list.retain(|(id, _)| *id != self.id);
            if list.is_empty() {
                map.remove(&self.endpoint_id);
            }
        }
    }
}

impl LocalFanout {
    pub fn subscribe(&self, endpoint_id: Uuid) -> SignalSubscription {
        let (sender, receiver) = mpsc::channel(SUBSCRIBER_BUFFER);
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(endpoint_id)
            .or_default()
            .push((id, sender));
        SignalSubscription {
            endpoint_id,
            receiver,
            _guard: SubscriptionGuard {
                fanout: self.clone(),
                endpoint_id,
                id,
            },
        }
    }

    /// Delivers to local subscribers; returns how many received the signal.
    pub fn deliver(&self, envelope: &SignalEnvelope) -> usize {
        let map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut delivered = 0;
        for endpoint_id in &envelope.endpoint_ids {
            for (_, sender) in map.get(endpoint_id).into_iter().flatten() {
                if sender.try_send(envelope.signal.clone()).is_ok() {
                    delivered += 1;
                }
            }
        }
        delivered
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(Vec::len)
            .sum()
    }
}

/// Single-process bus.
#[derive(Clone, Default)]
pub struct InMemorySignalBus {
    fanout: LocalFanout,
}

impl InMemorySignalBus {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RealtimeSignalBus for InMemorySignalBus {
    async fn publish(&self, envelope: SignalEnvelope) -> Result<(), SignalBusError> {
        self.fanout.deliver(&envelope);
        Ok(())
    }

    fn subscribe(&self, endpoint_id: Uuid) -> SignalSubscription {
        self.fanout.subscribe(endpoint_id)
    }
}

/// Cross-instance bus over PostgreSQL LISTEN / NOTIFY.
#[derive(Clone)]
pub struct PgNotifySignalBus {
    db: DatabaseConnection,
    fanout: LocalFanout,
}

impl PgNotifySignalBus {
    /// Starts this instance's listener (a dedicated connection that re-connects on
    /// failure) and returns the bus.
    pub async fn start(db: DatabaseConnection) -> Result<Self, SignalBusError> {
        let fanout = LocalFanout::default();
        let pool = db.get_postgres_connection_pool().clone();
        let mut listener = sea_orm::sqlx::postgres::PgListener::connect_with(&pool)
            .await
            .map_err(|_| SignalBusError::Unavailable)?;
        listener
            .listen(CHANNEL)
            .await
            .map_err(|_| SignalBusError::Unavailable)?;
        let local = fanout.clone();
        tokio::spawn(async move {
            loop {
                match listener.recv().await {
                    Ok(notification) => {
                        match serde_json::from_str::<SignalEnvelope>(notification.payload()) {
                            Ok(envelope) => {
                                local.deliver(&envelope);
                            }
                            Err(_) => {
                                tracing::warn!("malformed realtime signal notification ignored")
                            }
                        }
                    }
                    Err(error) => {
                        // PgListener re-connects on the next recv; signals sent meanwhile
                        // are lost and recovered through the API.
                        tracing::warn!(%error, "realtime signal listener interrupted");
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        });
        Ok(Self { db, fanout })
    }

    pub fn local(&self) -> &LocalFanout {
        &self.fanout
    }
}

#[async_trait]
impl RealtimeSignalBus for PgNotifySignalBus {
    async fn publish(&self, envelope: SignalEnvelope) -> Result<(), SignalBusError> {
        // Split large recipient lists so every NOTIFY payload stays bounded.
        let mut batches: Vec<Vec<Uuid>> = vec![Vec::new()];
        for endpoint in &envelope.endpoint_ids {
            let current = batches.last_mut().expect("non-empty");
            current.push(*endpoint);
            let size = serde_json::to_string(&SignalEnvelope {
                endpoint_ids: current.clone(),
                signal: envelope.signal.clone(),
            })
            .map(|s| s.len())
            .unwrap_or(usize::MAX);
            if size > MAX_PAYLOAD_BYTES {
                let last = current.pop().expect("just pushed");
                if current.is_empty() {
                    return Err(SignalBusError::TooLarge);
                }
                batches.push(vec![last]);
            }
        }
        for endpoint_ids in batches.into_iter().filter(|b| !b.is_empty()) {
            let payload = serde_json::to_string(&SignalEnvelope {
                endpoint_ids,
                signal: envelope.signal.clone(),
            })
            .map_err(|_| SignalBusError::TooLarge)?;
            self.db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT pg_notify($1, $2)",
                    vec![CHANNEL.into(), payload.into()],
                ))
                .await
                .map_err(|_| SignalBusError::Unavailable)?;
        }
        Ok(())
    }

    fn subscribe(&self, endpoint_id: Uuid) -> SignalSubscription {
        self.fanout.subscribe(endpoint_id)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn in_memory_bus_delivers_only_to_addressed_endpoints() {
        let bus = InMemorySignalBus::new();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut sa = bus.subscribe(a);
        let mut sb = bus.subscribe(b);
        bus.publish(SignalEnvelope {
            endpoint_ids: vec![a],
            signal: json!({ "type": "call.incoming" }),
        })
        .await
        .unwrap();
        assert_eq!(sa.recv().await.unwrap()["type"], "call.incoming");
        assert!(sb.try_recv().is_none());
        drop(sa);
        assert_eq!(bus.fanout.subscriber_count(), 1);
    }
}

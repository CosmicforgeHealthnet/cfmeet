//! The low-latency realtime broker (Part V §66).
//!
//! The broker is NOT a message store: durable events are committed to PostgreSQL first
//! (outbox + conversation cursor) and the broker only carries POINTERS to them
//! (`conversation id`, `event sequence`) plus ephemeral signals (typing, presence) and
//! control notices (device / session revoked). Losing a broker message never loses a
//! message: every gateway fills gaps from the database by cursor, re-checks heads
//! periodically, and resynchronizes every subscription after a broker reconnect.
//!
//! * [`LocalBroker`] — in-process fan-out (one instance; tests; local development).
//! * [`RedisBroker`] — self-hosted Redis pub/sub across gateway instances
//!   (`MESSAGING_REALTIME_REDIS_URL`). Redis is never the authoritative store.

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

/// Channel name of the realtime fan-out (versioned).
pub const CHANNEL: &str = "cfmeet:messaging:realtime:v1";

/// What travels between gateway instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum BrokerMessage {
    /// A committed durable conversation event (pointer only).
    Durable {
        conversation_id: Uuid,
        event_sequence: i64,
        event_type: String,
        /// The principal the event is about (member events), for authorization changes.
        subject_principal_id: Option<Uuid>,
    },
    /// A committed event addressed to one device (e.g. an MLS Welcome is available).
    Device {
        device_id: Uuid,
        event_type: String,
        reference_id: Uuid,
        conversation_id: Option<Uuid>,
    },
    /// A principal advanced its read cursor: its own sessions follow (§55), whatever the
    /// read-receipt policy shares with other members.
    ReadCursor {
        conversation_id: Uuid,
        principal_id: Uuid,
        read_sequence: i64,
    },
    /// Ephemeral typing signal (never stored).
    Typing {
        conversation_id: Uuid,
        principal_id: Uuid,
        origin_device_id: Uuid,
        started: bool,
        ttl_ms: u64,
    },
    /// Ephemeral presence change of a principal, for the conversations it shares.
    Presence {
        principal_id: Uuid,
        conversation_ids: Vec<Uuid>,
        state: String,
        last_seen_at: Option<String>,
    },
    /// A device was revoked: its connections close.
    DeviceRevoked { device_id: Uuid },
    /// A client session was revoked: its connections close.
    SessionRevoked { session_id: Uuid },
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("realtime broker unavailable: {0}")]
pub struct BrokerError(pub String);

/// What a subscriber receives.
#[derive(Debug, Clone)]
pub enum BrokerDelivery {
    Message(Arc<BrokerMessage>),
    /// Messages may have been lost (lag, reconnect): resynchronize from the database.
    Resync,
}

#[async_trait::async_trait]
pub trait Broker: Send + Sync {
    fn backend(&self) -> &'static str;
    async fn publish(&self, message: &BrokerMessage) -> Result<(), BrokerError>;
    /// A stream of deliveries for this gateway instance.
    fn subscribe(&self) -> mpsc::Receiver<BrokerDelivery>;
}

/// In-process broker.
pub struct LocalBroker {
    tx: broadcast::Sender<Arc<BrokerMessage>>,
    available: std::sync::atomic::AtomicBool,
}

impl Default for LocalBroker {
    fn default() -> Self {
        Self::new(65_536)
    }
}

impl LocalBroker {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            available: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// Failure injection: an unavailable broker refuses publications.
    pub fn set_available(&self, available: bool) {
        self.available
            .store(available, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Broker for LocalBroker {
    fn backend(&self) -> &'static str {
        "local"
    }

    async fn publish(&self, message: &BrokerMessage) -> Result<(), BrokerError> {
        if !self.available.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(BrokerError("local broker unavailable".into()));
        }
        // No subscriber is not an error: durable events are in the database.
        let _ = self.tx.send(Arc::new(message.clone()));
        Ok(())
    }

    fn subscribe(&self) -> mpsc::Receiver<BrokerDelivery> {
        let mut rx = self.tx.subscribe();
        let (out, recv) = mpsc::channel(4096);
        tokio::spawn(async move {
            loop {
                let delivery = match rx.recv().await {
                    Ok(m) => BrokerDelivery::Message(m),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        crate::metrics::inc("messaging_realtime_broker_lag_total", &[]);
                        tracing::warn!(missed = n, "realtime broker subscriber lagged");
                        BrokerDelivery::Resync
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if out.send(delivery).await.is_err() {
                    return;
                }
            }
        });
        recv
    }
}

/// Self-hosted Redis pub/sub.
pub struct RedisBroker {
    client: redis::Client,
    publisher: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
}

impl RedisBroker {
    /// Validates the URL (never echoed: it may carry a password).
    pub fn new(url: &str) -> Result<Self, String> {
        let client = redis::Client::open(url).map_err(|_| "invalid Redis URL".to_string())?;
        Ok(Self {
            client,
            publisher: tokio::sync::Mutex::new(None),
        })
    }

    async fn connection(&self) -> Result<redis::aio::MultiplexedConnection, BrokerError> {
        let mut guard = self.publisher.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let c = tokio::time::timeout(
            Duration::from_secs(3),
            self.client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| BrokerError("connect timeout".into()))?
        .map_err(|e| BrokerError(format!("{:?}", e.kind())))?;
        *guard = Some(c.clone());
        Ok(c)
    }
}

#[async_trait::async_trait]
impl Broker for RedisBroker {
    fn backend(&self) -> &'static str {
        "redis"
    }

    async fn publish(&self, message: &BrokerMessage) -> Result<(), BrokerError> {
        let payload = serde_json::to_string(message).expect("serializes");
        let mut c = self.connection().await?;
        let r: Result<i64, _> = tokio::time::timeout(
            Duration::from_secs(3),
            redis::cmd("PUBLISH")
                .arg(CHANNEL)
                .arg(payload)
                .query_async(&mut c),
        )
        .await
        .map_err(|_| BrokerError("publish timeout".into()))?;
        match r {
            Ok(_) => Ok(()),
            Err(e) => {
                // Drop the connection so the next publication reconnects.
                *self.publisher.lock().await = None;
                Err(BrokerError(format!("{:?}", e.kind())))
            }
        }
    }

    fn subscribe(&self) -> mpsc::Receiver<BrokerDelivery> {
        let client = self.client.clone();
        let (out, recv) = mpsc::channel(4096);
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(200);
            let mut first = true;
            loop {
                match client.get_async_pubsub().await {
                    Ok(mut pubsub) => {
                        if pubsub.subscribe(CHANNEL).await.is_ok() {
                            backoff = Duration::from_millis(200);
                            // After any (re)connection, events may have been missed.
                            if !first && out.send(BrokerDelivery::Resync).await.is_err() {
                                return;
                            }
                            first = false;
                            use futures_util::StreamExt;
                            let mut stream = pubsub.on_message();
                            while let Some(msg) = stream.next().await {
                                let Ok(text) = msg.get_payload::<String>() else {
                                    continue;
                                };
                                match serde_json::from_str::<BrokerMessage>(&text) {
                                    Ok(m) => {
                                        if out
                                            .send(BrokerDelivery::Message(Arc::new(m)))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    Err(_) => crate::metrics::inc(
                                        "messaging_realtime_broker_malformed_total",
                                        &[],
                                    ),
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error_kind = ?e.kind(), "realtime broker subscription failed")
                    }
                }
                crate::metrics::inc("messaging_realtime_broker_reconnects_total", &[]);
                if out.is_closed() {
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
                // Anything published meanwhile is recovered from the database.
                if !first && out.send(BrokerDelivery::Resync).await.is_err() {
                    return;
                }
            }
        });
        recv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_messages_carry_pointers_and_signals_only() {
        let m = BrokerMessage::Durable {
            conversation_id: Uuid::nil(),
            event_sequence: 7,
            event_type: "messaging.message.created".into(),
            subject_principal_id: None,
        };
        let text = serde_json::to_string(&m).unwrap();
        assert!(text.starts_with(r#"{"k":"durable""#));
        assert_eq!(serde_json::from_str::<BrokerMessage>(&text).unwrap(), m);
        for forbidden in ["ciphertext", "text", "content", "body"] {
            assert!(!text.contains(forbidden));
        }
    }

    #[tokio::test]
    async fn the_local_broker_fans_out_and_refuses_when_unavailable() {
        let b = LocalBroker::new(8);
        let mut r1 = b.subscribe();
        let mut r2 = b.subscribe();
        tokio::task::yield_now().await;
        let m = BrokerMessage::DeviceRevoked {
            device_id: Uuid::nil(),
        };
        b.publish(&m).await.unwrap();
        for r in [&mut r1, &mut r2] {
            match r.recv().await.unwrap() {
                BrokerDelivery::Message(got) => assert_eq!(*got, m),
                BrokerDelivery::Resync => panic!(),
            }
        }
        b.set_available(false);
        assert!(b.publish(&m).await.is_err());
    }
}

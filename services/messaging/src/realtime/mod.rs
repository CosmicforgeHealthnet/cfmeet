//! The Messaging realtime gateway (Part V Part E, §58–§68).
//!
//! An independent WebSocket gateway inside the Messaging service — no LiveKit room,
//! Meeting Service membership, voice / video session or hosted messaging provider.
//!
//! ```text
//! device ──CF-Client-Token──▶ POST /v1/messaging/realtime/tickets ──▶ single-use ticket
//! device ──WebSocket (Sec-WebSocket-Protocol: cfmeet.messaging.v1, cfmeet.ticket.<t>)──▶ gateway
//!            │ session / device / principal re-validated at connect and periodically
//!            ├─ subscriptions: current ACTIVE membership only (NOT_FOUND otherwise)
//!            ├─ durable events: DB commit → outbox → broker (pointer) → gap-filled from DB
//!            ├─ ephemeral: typing / presence through the broker only (TTL, never stored)
//!            └─ bounded queues, frame limits, rate limits, heartbeats, draining
//! ```
//!
//! * [`token`] — short-lived Messaging Client Access Tokens.
//! * [`sessions`] — token issuance, session revocation, tickets, (re-)validation.
//! * [`broker`] / [`ephemeral`] — local or self-hosted Redis fan-out and presence state.
//! * [`hub`] — this instance's connections and routing; [`connection`] — one connection.
//! * [`fanout`] — the outbox transport (committed events → broker; push jobs).
//! * [`presence`] — privacy-respecting presence; [`push`] — the notification boundary.
//!
//! No durable state lives only in a gateway process: subscriptions are re-established
//! by clients after reconnect from their acknowledged cursors.

pub mod broker;
pub mod connection;
pub mod ephemeral;
pub mod fanout;
pub mod http;
pub mod hub;
pub mod presence;
pub mod push;
pub mod push_adapters;
pub mod sessions;
pub mod token;

use std::{sync::Arc, time::Duration};

use self::{
    broker::{Broker, LocalBroker},
    ephemeral::{MemoryPresence, PresenceStore},
    hub::Hub,
    push::{PushProvider, UnconfiguredPush},
    token::ClientTokenKeys,
};

/// Protocol version spoken on the WebSocket (the subprotocol `cfmeet.messaging.v1`).
pub const PROTOCOL_VERSION: u32 = 1;
pub const SUBPROTOCOL: &str = "cfmeet.messaging.v1";
pub const TICKET_PROTOCOL_PREFIX: &str = "cfmeet.ticket.";

/// Gateway limits and timings (validated configuration; see `config`).
#[derive(Debug, Clone)]
pub struct RealtimeSettings {
    pub token_ttl: chrono::Duration,
    pub ticket_ttl: chrono::Duration,
    /// Browser `Origin`s allowed to open a connection (exact match). Native clients send
    /// no Origin. Empty: browser connections are refused.
    pub allowed_origins: Vec<String>,
    /// Refuse handshakes that did not arrive over TLS (`X-Forwarded-Proto: https` from the
    /// TLS-terminating proxy). Required outside local / test.
    pub require_tls: bool,
    pub heartbeat_interval: Duration,
    pub idle_timeout: Duration,
    pub revalidate_interval: Duration,
    pub max_frame_bytes: usize,
    pub send_queue: usize,
    pub send_timeout: Duration,
    pub max_subscriptions: usize,
    pub max_connections: usize,
    pub per_tenant_connections: usize,
    pub per_principal_connections: usize,
    pub per_device_connections: usize,
    pub inbound_frames_per_second: u32,
    pub max_replay: i64,
    pub typing_ttl: Duration,
    pub drain: Duration,
    pub publisher_interval: Duration,
}

impl Default for RealtimeSettings {
    fn default() -> Self {
        Self {
            token_ttl: chrono::Duration::minutes(15),
            ticket_ttl: chrono::Duration::seconds(30),
            allowed_origins: Vec::new(),
            require_tls: false,
            heartbeat_interval: Duration::from_secs(20),
            idle_timeout: Duration::from_secs(60),
            revalidate_interval: Duration::from_secs(30),
            max_frame_bytes: 64 * 1024,
            send_queue: 512,
            send_timeout: Duration::from_secs(10),
            max_subscriptions: 200,
            max_connections: 50_000,
            per_tenant_connections: 10_000,
            per_principal_connections: 20,
            per_device_connections: 5,
            inbound_frames_per_second: 20,
            max_replay: 500,
            typing_ttl: Duration::from_secs(8),
            drain: Duration::from_secs(10),
            publisher_interval: Duration::from_millis(250),
        }
    }
}

/// The realtime runtime shared by every request and connection of this instance.
pub struct Realtime {
    pub settings: RealtimeSettings,
    /// `None`: client tokens cannot be issued (realtime disabled; fail closed).
    pub keys: Option<ClientTokenKeys>,
    pub broker: Arc<dyn Broker>,
    pub presence: Arc<dyn PresenceStore>,
    pub hub: Arc<Hub>,
    pub push: Arc<dyn PushProvider>,
    /// Wakes the outbox publisher right after a commit (low latency; the periodic pass
    /// remains the safety net).
    pub outbox_wakeup: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for Realtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Realtime")
            .field("broker", &self.broker.backend())
            .field("presence", &self.presence.backend())
            .field("client_tokens", &self.keys.is_some())
            .finish_non_exhaustive()
    }
}

impl Realtime {
    pub fn new(
        settings: RealtimeSettings,
        keys: Option<ClientTokenKeys>,
        broker: Arc<dyn Broker>,
        presence: Arc<dyn PresenceStore>,
        push: Arc<dyn PushProvider>,
    ) -> Self {
        let hub = Arc::new(Hub::new(settings.clone()));
        Self {
            settings,
            keys,
            broker,
            presence,
            hub,
            push,
            outbox_wakeup: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Single-instance defaults: no client-token key (disabled), local broker, memory
    /// presence, no push provider.
    pub fn disabled() -> Self {
        Self::new(
            RealtimeSettings::default(),
            None,
            Arc::new(LocalBroker::default()),
            Arc::new(MemoryPresence::default()),
            Arc::new(UnconfiguredPush),
        )
    }

    /// Starts this instance's broker listener (routes broker deliveries to connections).
    pub fn start(self: &Arc<Self>) {
        let rx = self.broker.subscribe();
        tokio::spawn(hub::run(self.hub.clone(), rx));
    }
}

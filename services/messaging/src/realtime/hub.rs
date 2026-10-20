//! This gateway instance's connections and routing (§58, §60, §65, §66).
//!
//! * Limits are enforced BEFORE the WebSocket upgrade: instance, tenant, principal and
//!   device connection counts (a tenant cannot exhaust capacity belonging to others).
//! * Routing is by conversation (subscriptions), device and session. Each connection has
//!   ONE bounded queue: when it is full the connection is marked lagging and stops
//!   receiving — memory never grows with a slow reader; the connection closes with a
//!   resumable error and the client replays durable events from its cursor.
//! * Close notices (revocation, draining) bypass the queue.
//! * Nothing here is durable: subscriptions are re-created by clients after reconnect.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use tokio::sync::{mpsc, watch, Notify};
use uuid::Uuid;

use super::{
    broker::{BrokerDelivery, BrokerMessage},
    RealtimeSettings,
};

/// What the hub hands to one connection (small; the connection hydrates from the DB).
#[derive(Debug, Clone)]
pub enum HubEvent {
    Durable {
        conversation_id: Uuid,
        event_sequence: i64,
        event_type: Arc<str>,
        subject_principal_id: Option<Uuid>,
    },
    Device {
        event_type: Arc<str>,
        reference_id: Uuid,
        conversation_id: Option<Uuid>,
    },
    Typing {
        conversation_id: Uuid,
        principal_id: Uuid,
        started: bool,
        ttl_ms: u64,
    },
    Presence {
        conversation_id: Uuid,
        principal_id: Uuid,
        state: Arc<str>,
        last_seen_at: Option<Arc<str>>,
    },
    /// Messages may have been lost: re-check every subscription's head.
    Resync,
}

/// A close notice delivered out of band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseNotice {
    pub code: u16,
    pub reason: &'static str,
}

pub struct ConnectionHandle {
    pub id: u64,
    pub rx: mpsc::Receiver<HubEvent>,
    pub lagging: Arc<AtomicBool>,
    pub close: Arc<Notify>,
    pub close_notice: Arc<Mutex<Option<CloseNotice>>>,
    pub draining: watch::Receiver<bool>,
}

struct Entry {
    tx: mpsc::Sender<HubEvent>,
    lagging: Arc<AtomicBool>,
    close: Arc<Notify>,
    close_notice: Arc<Mutex<Option<CloseNotice>>>,
    tenant_id: Uuid,
    principal_key: (Uuid, Uuid),
    device_id: Uuid,
    session_id: Uuid,
    subscriptions: HashSet<Uuid>,
}

#[derive(Default)]
struct State {
    conns: HashMap<u64, Entry>,
    by_conversation: HashMap<Uuid, HashSet<u64>>,
    by_device: HashMap<Uuid, HashSet<u64>>,
    by_session: HashMap<Uuid, HashSet<u64>>,
    per_tenant: HashMap<Uuid, usize>,
    per_principal: HashMap<(Uuid, Uuid), usize>,
    reserved: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitExceeded {
    Instance,
    Tenant,
    Principal,
    Device,
    Draining,
}

impl LimitExceeded {
    pub fn reason(&self) -> &'static str {
        match self {
            LimitExceeded::Instance => "INSTANCE_CONNECTION_LIMIT",
            LimitExceeded::Tenant => "TENANT_CONNECTION_LIMIT",
            LimitExceeded::Principal => "PRINCIPAL_CONNECTION_LIMIT",
            LimitExceeded::Device => "DEVICE_CONNECTION_LIMIT",
            LimitExceeded::Draining => "SERVER_DRAINING",
        }
    }
}

/// Identity of a connection being admitted.
#[derive(Debug, Clone, Copy)]
pub struct Admission {
    pub tenant_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
    pub device_id: Uuid,
    pub session_id: Uuid,
}

pub struct Hub {
    settings: RealtimeSettings,
    state: RwLock<State>,
    next: AtomicU64,
    draining: watch::Sender<bool>,
    pub delivered: AtomicU64,
    pub dropped_lagging: AtomicU64,
}

impl Hub {
    pub fn new(settings: RealtimeSettings) -> Self {
        let (draining, _) = watch::channel(false);
        Self {
            settings,
            state: RwLock::new(State::default()),
            next: AtomicU64::new(1),
            draining,
            delivered: AtomicU64::new(0),
            dropped_lagging: AtomicU64::new(0),
        }
    }

    pub fn is_draining(&self) -> bool {
        *self.draining.borrow()
    }

    pub fn connections(&self) -> usize {
        self.state.read().unwrap().conns.len()
    }

    /// Admits and registers a connection (limits checked atomically).
    pub fn register(&self, a: Admission) -> Result<ConnectionHandle, LimitExceeded> {
        if self.is_draining() {
            return Err(LimitExceeded::Draining);
        }
        let mut s = self.state.write().unwrap();
        let pk = (a.environment_id, a.principal_id);
        if s.conns.len() + s.reserved >= self.settings.max_connections {
            return Err(LimitExceeded::Instance);
        }
        if s.per_tenant.get(&a.tenant_id).copied().unwrap_or(0)
            >= self.settings.per_tenant_connections
        {
            return Err(LimitExceeded::Tenant);
        }
        if s.per_principal.get(&pk).copied().unwrap_or(0) >= self.settings.per_principal_connections
        {
            return Err(LimitExceeded::Principal);
        }
        if s.by_device.get(&a.device_id).map_or(0, HashSet::len)
            >= self.settings.per_device_connections
        {
            return Err(LimitExceeded::Device);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(self.settings.send_queue.max(1));
        let lagging = Arc::new(AtomicBool::new(false));
        let close = Arc::new(Notify::new());
        let close_notice = Arc::new(Mutex::new(None));
        s.conns.insert(
            id,
            Entry {
                tx,
                lagging: lagging.clone(),
                close: close.clone(),
                close_notice: close_notice.clone(),
                tenant_id: a.tenant_id,
                principal_key: pk,
                device_id: a.device_id,
                session_id: a.session_id,
                subscriptions: HashSet::new(),
            },
        );
        *s.per_tenant.entry(a.tenant_id).or_default() += 1;
        *s.per_principal.entry(pk).or_default() += 1;
        s.by_device.entry(a.device_id).or_default().insert(id);
        s.by_session.entry(a.session_id).or_default().insert(id);
        crate::metrics::gauge("messaging_realtime_connections", s.conns.len() as i64);
        Ok(ConnectionHandle {
            id,
            rx,
            lagging,
            close,
            close_notice,
            draining: self.draining.subscribe(),
        })
    }

    /// Checks the limits without registering (handshake pre-check, before upgrade).
    pub fn admissible(&self, a: &Admission) -> Result<(), LimitExceeded> {
        if self.is_draining() {
            return Err(LimitExceeded::Draining);
        }
        let s = self.state.read().unwrap();
        let pk = (a.environment_id, a.principal_id);
        if s.conns.len() >= self.settings.max_connections {
            Err(LimitExceeded::Instance)
        } else if s.per_tenant.get(&a.tenant_id).copied().unwrap_or(0)
            >= self.settings.per_tenant_connections
        {
            Err(LimitExceeded::Tenant)
        } else if s.per_principal.get(&pk).copied().unwrap_or(0)
            >= self.settings.per_principal_connections
        {
            Err(LimitExceeded::Principal)
        } else if s.by_device.get(&a.device_id).map_or(0, HashSet::len)
            >= self.settings.per_device_connections
        {
            Err(LimitExceeded::Device)
        } else {
            Ok(())
        }
    }

    pub fn unregister(&self, id: u64) {
        let mut s = self.state.write().unwrap();
        let Some(e) = s.conns.remove(&id) else { return };
        for c in &e.subscriptions {
            if let Some(set) = s.by_conversation.get_mut(c) {
                set.remove(&id);
                if set.is_empty() {
                    s.by_conversation.remove(c);
                }
            }
        }
        let st = &mut *s;
        for (map, key) in [
            (&mut st.by_device, e.device_id),
            (&mut st.by_session, e.session_id),
        ] {
            if let Some(set) = map.get_mut(&key) {
                set.remove(&id);
                if set.is_empty() {
                    map.remove(&key);
                }
            }
        }
        if let Some(n) = s.per_tenant.get_mut(&e.tenant_id) {
            *n -= 1;
            if *n == 0 {
                s.per_tenant.remove(&e.tenant_id);
            }
        }
        if let Some(n) = s.per_principal.get_mut(&e.principal_key) {
            *n -= 1;
            if *n == 0 {
                s.per_principal.remove(&e.principal_key);
            }
        }
        crate::metrics::gauge("messaging_realtime_connections", s.conns.len() as i64);
    }

    /// Routes a conversation's events to `id` (false: subscription limit reached).
    pub fn subscribe(&self, id: u64, conversation: Uuid) -> bool {
        let mut s = self.state.write().unwrap();
        let Some(e) = s.conns.get_mut(&id) else {
            return false;
        };
        if !e.subscriptions.contains(&conversation)
            && e.subscriptions.len() >= self.settings.max_subscriptions
        {
            return false;
        }
        e.subscriptions.insert(conversation);
        s.by_conversation
            .entry(conversation)
            .or_default()
            .insert(id);
        true
    }

    pub fn unsubscribe(&self, id: u64, conversation: Uuid) {
        let mut s = self.state.write().unwrap();
        if let Some(e) = s.conns.get_mut(&id) {
            e.subscriptions.remove(&conversation);
        }
        if let Some(set) = s.by_conversation.get_mut(&conversation) {
            set.remove(&id);
            if set.is_empty() {
                s.by_conversation.remove(&conversation);
            }
        }
    }

    fn offer(&self, e: &Entry, event: HubEvent) {
        if e.lagging.load(Ordering::Relaxed) {
            self.dropped_lagging.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match e.tx.try_send(event) {
            Ok(()) => {
                self.delivered.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Slow consumer: stop buffering; the connection closes resumably.
                e.lagging.store(true, Ordering::SeqCst);
                e.close.notify_one();
                self.dropped_lagging.fetch_add(1, Ordering::Relaxed);
                crate::metrics::inc("messaging_realtime_slow_consumers_total", &[]);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    fn close_where(&self, ids: Option<&HashSet<u64>>, notice: CloseNotice) {
        let s = self.state.read().unwrap();
        for id in ids.into_iter().flatten() {
            if let Some(e) = s.conns.get(id) {
                *e.close_notice.lock().unwrap() = Some(notice);
                e.close.notify_one();
            }
        }
    }

    /// Routes one broker delivery to this instance's connections.
    pub fn route(&self, delivery: BrokerDelivery) {
        let message = match delivery {
            BrokerDelivery::Resync => {
                let s = self.state.read().unwrap();
                for e in s.conns.values() {
                    self.offer(e, HubEvent::Resync);
                }
                return;
            }
            BrokerDelivery::Message(m) => m,
        };
        match &*message {
            BrokerMessage::Durable {
                conversation_id,
                event_sequence,
                event_type,
                subject_principal_id,
            } => {
                let s = self.state.read().unwrap();
                let event_type: Arc<str> = Arc::from(event_type.as_str());
                for id in s.by_conversation.get(conversation_id).into_iter().flatten() {
                    if let Some(e) = s.conns.get(id) {
                        self.offer(
                            e,
                            HubEvent::Durable {
                                conversation_id: *conversation_id,
                                event_sequence: *event_sequence,
                                event_type: event_type.clone(),
                                subject_principal_id: *subject_principal_id,
                            },
                        );
                    }
                }
            }
            BrokerMessage::Device {
                device_id,
                event_type,
                reference_id,
                conversation_id,
            } => {
                let s = self.state.read().unwrap();
                for id in s.by_device.get(device_id).into_iter().flatten() {
                    if let Some(e) = s.conns.get(id) {
                        self.offer(
                            e,
                            HubEvent::Device {
                                event_type: Arc::from(event_type.as_str()),
                                reference_id: *reference_id,
                                conversation_id: *conversation_id,
                            },
                        );
                    }
                }
            }
            BrokerMessage::Typing {
                conversation_id,
                principal_id,
                origin_device_id,
                started,
                ttl_ms,
            } => {
                let s = self.state.read().unwrap();
                for id in s.by_conversation.get(conversation_id).into_iter().flatten() {
                    if let Some(e) = s.conns.get(id) {
                        if e.device_id == *origin_device_id {
                            continue;
                        }
                        self.offer(
                            e,
                            HubEvent::Typing {
                                conversation_id: *conversation_id,
                                principal_id: *principal_id,
                                started: *started,
                                ttl_ms: *ttl_ms,
                            },
                        );
                    }
                }
            }
            BrokerMessage::Presence {
                principal_id,
                conversation_ids,
                state,
                last_seen_at,
            } => {
                let s = self.state.read().unwrap();
                let state: Arc<str> = Arc::from(state.as_str());
                let last: Option<Arc<str>> = last_seen_at.as_deref().map(Arc::from);
                for c in conversation_ids {
                    for id in s.by_conversation.get(c).into_iter().flatten() {
                        if let Some(e) = s.conns.get(id) {
                            if e.principal_key.1 == *principal_id {
                                continue;
                            }
                            self.offer(
                                e,
                                HubEvent::Presence {
                                    conversation_id: *c,
                                    principal_id: *principal_id,
                                    state: state.clone(),
                                    last_seen_at: last.clone(),
                                },
                            );
                        }
                    }
                }
            }
            BrokerMessage::DeviceRevoked { device_id } => {
                let ids = self.state.read().unwrap().by_device.get(device_id).cloned();
                self.close_where(
                    ids.as_ref(),
                    CloseNotice {
                        code: 4401,
                        reason: "DEVICE_REVOKED",
                    },
                );
            }
            BrokerMessage::SessionRevoked { session_id } => {
                let ids = self
                    .state
                    .read()
                    .unwrap()
                    .by_session
                    .get(session_id)
                    .cloned();
                self.close_where(
                    ids.as_ref(),
                    CloseNotice {
                        code: 4401,
                        reason: "SESSION_REVOKED",
                    },
                );
            }
        }
    }

    /// Stops admitting connections and tells every connection to drain.
    pub fn drain(&self) {
        let _ = self.draining.send(true);
    }
}

/// Routes broker deliveries until the broker stream ends.
pub async fn run(hub: Arc<Hub>, mut rx: mpsc::Receiver<BrokerDelivery>) {
    while let Some(d) = rx.recv().await {
        hub.route(d);
    }
    tracing::warn!("realtime broker stream ended");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adm(tenant: Uuid, principal: Uuid, device: Uuid) -> Admission {
        Admission {
            tenant_id: tenant,
            environment_id: tenant,
            principal_id: principal,
            device_id: device,
            session_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn limits_are_enforced_per_device_principal_tenant_and_instance() {
        let hub = Hub::new(RealtimeSettings {
            per_device_connections: 1,
            per_principal_connections: 2,
            per_tenant_connections: 3,
            max_connections: 4,
            ..Default::default()
        });
        let (t, p, d) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let a = hub.register(adm(t, p, d)).unwrap();
        assert_eq!(
            hub.register(adm(t, p, d)).err(),
            Some(LimitExceeded::Device)
        );
        hub.register(adm(t, p, Uuid::new_v4())).unwrap();
        assert_eq!(
            hub.register(adm(t, p, Uuid::new_v4())).err(),
            Some(LimitExceeded::Principal)
        );
        hub.register(adm(t, Uuid::new_v4(), Uuid::new_v4()))
            .unwrap();
        assert_eq!(
            hub.register(adm(t, Uuid::new_v4(), Uuid::new_v4())).err(),
            Some(LimitExceeded::Tenant)
        );
        hub.register(adm(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()))
            .unwrap();
        assert_eq!(
            hub.register(adm(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()))
                .err(),
            Some(LimitExceeded::Instance)
        );
        hub.unregister(a.id);
        assert!(hub.register(adm(t, p, d)).is_ok(), "released on unregister");
        hub.drain();
        assert_eq!(
            hub.register(adm(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()))
                .err(),
            Some(LimitExceeded::Draining)
        );
    }

    #[test]
    fn a_slow_consumer_is_marked_lagging_instead_of_buffering() {
        let hub = Hub::new(RealtimeSettings {
            send_queue: 2,
            ..Default::default()
        });
        let c = Uuid::new_v4();
        let h = hub
            .register(adm(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()))
            .unwrap();
        assert!(hub.subscribe(h.id, c));
        for i in 1..=10 {
            hub.route(BrokerDelivery::Message(Arc::new(BrokerMessage::Durable {
                conversation_id: c,
                event_sequence: i,
                event_type: "messaging.message.created".into(),
                subject_principal_id: None,
            })));
        }
        assert!(h.lagging.load(Ordering::SeqCst));
        assert_eq!(h.rx.len(), 2, "never more than the bounded queue");
        assert_eq!(hub.dropped_lagging.load(Ordering::Relaxed), 8);
        // Other conversations are not routed to it.
        let other = hub
            .register(adm(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()))
            .unwrap();
        hub.route(BrokerDelivery::Message(Arc::new(BrokerMessage::Durable {
            conversation_id: c,
            event_sequence: 11,
            event_type: "x".into(),
            subject_principal_id: None,
        })));
        assert_eq!(other.rx.len(), 0);
    }
}

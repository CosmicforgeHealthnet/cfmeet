//! The Telephony service core (Part E §39–§40).
//!
//! Telephony EXECUTES public-network actions; it never decides whether a business user
//! may call. Every paid action carries authority proof from Calling / Privacy Relay
//! ([`crate::commands::originate`]), is persisted as a durable command BEFORE any provider
//! action, and is executed by lease-holding workers. All provider / SIP specifics sit
//! behind [`crate::providers::TelecomInterconnectProvider`] and
//! [`crate::sip::SipMediaEdge`]; replacing an aggregator, a SIP carrier or a direct trunk
//! changes neither Calling's public API, the canonical Call nor the Privacy Relay model.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;

use crate::{
    clock::SharedClock,
    config::TelecomFlags,
    ports::{CallingPort, PrivacyRelayPort, Unconfigured},
    providers::ProviderRegistry,
    sip::{NoEdge, SipMediaEdge},
};

/// Execution timing (bounded; tests shorten them).
#[derive(Debug, Clone)]
pub struct ExecutionSettings {
    /// How long an originated leg may ring before it is ended as NO_ANSWER.
    pub ring_timeout: Duration,
    /// Provider request deadline (beyond it the outcome is UNKNOWN, never failure).
    pub provider_request_timeout: Duration,
    /// Command lease (a worker that holds it longer is presumed dead).
    pub command_lease: Duration,
    /// First reconciliation delay of an UNKNOWN outcome, and the backoff cap.
    pub reconcile_delay: Duration,
    pub reconcile_backoff_cap: Duration,
    /// Read-back attempts before an unresolved UNKNOWN is escalated (it stays UNKNOWN).
    pub reconcile_escalate_after: i32,
    /// A leg ringing longer than this is a drift case for the reconciler.
    pub ringing_drift_after: Duration,
    /// Retention of restricted provider-event detail.
    pub provider_event_retention: Duration,
    /// Callback authentication failures per minute per provider before an incident.
    pub callback_auth_failure_incident_threshold: i64,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            ring_timeout: Duration::from_secs(60),
            provider_request_timeout: Duration::from_secs(10),
            command_lease: Duration::from_secs(30),
            reconcile_delay: Duration::from_secs(5),
            reconcile_backoff_cap: Duration::from_secs(300),
            reconcile_escalate_after: 5,
            ringing_drift_after: Duration::from_secs(120),
            provider_event_retention: Duration::from_secs(30 * 24 * 3600),
            callback_auth_failure_incident_threshold: 20,
        }
    }
}

#[derive(Clone)]
pub struct TelephonyService {
    pub(crate) db: DatabaseConnection,
    pub(crate) clock: SharedClock,
    pub flags: TelecomFlags,
    pub(crate) providers: ProviderRegistry,
    pub(crate) edge: Arc<dyn SipMediaEdge>,
    pub(crate) privacy: Arc<dyn PrivacyRelayPort>,
    pub(crate) calling: Arc<dyn CallingPort>,
    pub(crate) worker_id: String,
    pub settings: ExecutionSettings,
    /// When this instance last read back the interconnects' call inventory.
    pub(crate) last_inventory: Arc<std::sync::Mutex<Option<DateTime<Utc>>>>,
    /// When this instance last ran the abuse-signal scan.
    pub(crate) last_abuse_scan: Arc<std::sync::Mutex<Option<DateTime<Utc>>>>,
}

impl TelephonyService {
    pub fn new(db: DatabaseConnection, clock: SharedClock, flags: TelecomFlags) -> Self {
        Self {
            db,
            clock,
            flags,
            providers: ProviderRegistry::new(),
            edge: NoEdge::shared(),
            privacy: Arc::new(Unconfigured),
            calling: Arc::new(Unconfigured),
            worker_id: format!("w-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            settings: ExecutionSettings::default(),
            last_inventory: Arc::default(),
            last_abuse_scan: Arc::default(),
        }
    }

    pub fn with_providers(mut self, providers: ProviderRegistry) -> Self {
        self.providers = providers;
        self
    }

    pub fn with_edge(mut self, edge: Arc<dyn SipMediaEdge>) -> Self {
        self.edge = edge;
        self
    }

    pub fn with_privacy_relay(mut self, port: Arc<dyn PrivacyRelayPort>) -> Self {
        self.privacy = port;
        self
    }

    pub fn with_calling(mut self, port: Arc<dyn CallingPort>) -> Self {
        self.calling = port;
        self
    }

    pub fn with_settings(mut self, settings: ExecutionSettings) -> Self {
        self.settings = settings;
        self
    }

    /// A distinct worker identity (simulating another instance in tests).
    pub fn as_instance(mut self, worker_id: &str) -> Self {
        self.worker_id = worker_id.to_string();
        self
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn edge(&self) -> &Arc<dyn SipMediaEdge> {
        &self.edge
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }
}

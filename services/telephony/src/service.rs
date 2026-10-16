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
    config::{invalid, ConfigError, TelecomFlags},
    destinations::{DestinationClass, UnclassifiedPolicy},
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
    /// Invalid callbacks (malformed / oversized / unknown provider / replay burst) per
    /// minute per subject before an aggregated incident (Part H §59–§60).
    pub callback_invalid_incident_threshold: i64,
    /// Destination classes refused before any provider action (Part H §66).
    pub blocked_destination_classes: Vec<DestinationClass>,
    /// What happens to a destination no class rule matches.
    pub unclassified_destination_policy: UnclassifiedPolicy,
    /// Retention of RESOLVED incidents (Part H §80).
    pub incident_retention: Duration,
}

/// Settings read from the environment (every value has a documented default):
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `TELEPHONY_CALLBACK_AUTH_FAILURE_INCIDENT_THRESHOLD` | 20 | auth failures / minute / provider → `CALLBACK_AUTH_FAILURES` |
/// | `TELEPHONY_CALLBACK_INVALID_INCIDENT_THRESHOLD` | 20 | invalid callbacks / minute / subject → `CALLBACK_*` incidents |
/// | `TELEPHONY_BLOCKED_DESTINATION_CLASSES` | `PREMIUM,SHARED_COST,SATELLITE,HIGH_RISK` | classes refused pre-toll |
/// | `TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY` | `ALLOW` | `ALLOW` or `DENY` destinations no rule classifies |
/// | `TELEPHONY_PROVIDER_EVENT_RETENTION_DAYS` | 30 (min 1) | provider event evidence retention |
/// | `TELEPHONY_INCIDENT_RETENTION_DAYS` | 365 (min 1) | RESOLVED incident retention |
impl ExecutionSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|k: &str| std::env::var(k).ok())
    }

    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let mut settings = Self::default();
        let threshold = |var: &'static str, current: i64| -> Result<i64, ConfigError> {
            match value(var) {
                None => Ok(current),
                Some(v) => v
                    .parse::<i64>()
                    .ok()
                    .filter(|n| (1..=100_000).contains(n))
                    .ok_or_else(|| invalid(var, "must be an integer between 1 and 100000")),
            }
        };
        let days = |var: &'static str, current: Duration| -> Result<Duration, ConfigError> {
            match value(var) {
                None => Ok(current),
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|d| (1..=3_650).contains(d))
                    .map(|d| Duration::from_secs(d * 24 * 3600))
                    .ok_or_else(|| {
                        invalid(var, "must be an integer number of days between 1 and 3650")
                    }),
            }
        };
        settings.callback_auth_failure_incident_threshold = threshold(
            "TELEPHONY_CALLBACK_AUTH_FAILURE_INCIDENT_THRESHOLD",
            settings.callback_auth_failure_incident_threshold,
        )?;
        settings.callback_invalid_incident_threshold = threshold(
            "TELEPHONY_CALLBACK_INVALID_INCIDENT_THRESHOLD",
            settings.callback_invalid_incident_threshold,
        )?;
        if let Some(v) = value("TELEPHONY_BLOCKED_DESTINATION_CLASSES") {
            settings.blocked_destination_classes =
                crate::destinations::parse_blocked(&v).ok_or_else(|| {
                    invalid(
                        "TELEPHONY_BLOCKED_DESTINATION_CLASSES",
                        "must be a comma-separated list of STANDARD, PREMIUM, SHARED_COST, SATELLITE, HIGH_RISK",
                    )
                })?;
        }
        if let Some(v) = value("TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY") {
            settings.unclassified_destination_policy =
                UnclassifiedPolicy::parse(&v).ok_or_else(|| {
                    invalid(
                        "TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY",
                        "must be ALLOW or DENY",
                    )
                })?;
        }
        settings.provider_event_retention = days(
            "TELEPHONY_PROVIDER_EVENT_RETENTION_DAYS",
            settings.provider_event_retention,
        )?;
        settings.incident_retention = days(
            "TELEPHONY_INCIDENT_RETENTION_DAYS",
            settings.incident_retention,
        )?;
        Ok(settings)
    }
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
            callback_invalid_incident_threshold: 20,
            blocked_destination_classes: DestinationClass::DEFAULT_BLOCKED.to_vec(),
            unclassified_destination_policy: UnclassifiedPolicy::Allow,
            incident_retention: Duration::from_secs(365 * 24 * 3600),
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
    /// Dependency health this instance observed (incidents / gauges).
    pub(crate) dependencies: Arc<crate::incidents::DependencyState>,
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
            dependencies: Arc::default(),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn execution_settings_have_documented_defaults_and_bounds() {
        let s = ExecutionSettings::from_lookup(&lookup(&[])).unwrap();
        assert_eq!(s.callback_invalid_incident_threshold, 20);
        assert_eq!(
            s.provider_event_retention,
            Duration::from_secs(30 * 24 * 3600)
        );
        assert_eq!(s.incident_retention, Duration::from_secs(365 * 24 * 3600));
        assert_eq!(s.unclassified_destination_policy, UnclassifiedPolicy::Allow);
        assert_eq!(
            s.blocked_destination_classes,
            DestinationClass::DEFAULT_BLOCKED.to_vec()
        );
        let s = ExecutionSettings::from_lookup(&lookup(&[
            ("TELEPHONY_PROVIDER_EVENT_RETENTION_DAYS", "7"),
            ("TELEPHONY_INCIDENT_RETENTION_DAYS", "90"),
            ("TELEPHONY_CALLBACK_INVALID_INCIDENT_THRESHOLD", "5"),
            ("TELEPHONY_BLOCKED_DESTINATION_CLASSES", "PREMIUM"),
            ("TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY", "DENY"),
        ]))
        .unwrap();
        assert_eq!(
            s.provider_event_retention,
            Duration::from_secs(7 * 24 * 3600)
        );
        assert_eq!(s.incident_retention, Duration::from_secs(90 * 24 * 3600));
        assert_eq!(s.callback_invalid_incident_threshold, 5);
        assert_eq!(
            s.blocked_destination_classes,
            vec![DestinationClass::Premium]
        );
        assert_eq!(s.unclassified_destination_policy, UnclassifiedPolicy::Deny);
        for (var, bad) in [
            ("TELEPHONY_PROVIDER_EVENT_RETENTION_DAYS", "0"),
            ("TELEPHONY_INCIDENT_RETENTION_DAYS", "forever"),
            ("TELEPHONY_CALLBACK_INVALID_INCIDENT_THRESHOLD", "0"),
            ("TELEPHONY_BLOCKED_DESTINATION_CLASSES", "EXPENSIVE"),
            ("TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY", "maybe"),
        ] {
            assert!(
                ExecutionSettings::from_lookup(&lookup(&[(var, bad)])).is_err(),
                "{var}={bad}"
            );
        }
    }
}

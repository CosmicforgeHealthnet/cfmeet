//! Data retention and minimization (Part H §80), Privacy Relay's part.
//!
//! Every class of data this service keeps has ONE typed policy: what it is
//! ([`DataClass`]), what happens when it ages out ([`RetentionAction`]), after how long,
//! and which environment variable configures that bound. The bounds are platform
//! conventions with documented defaults, never a legal claim; an operator sets them per
//! deployment. Retention never shortens what evidence an ongoing authority needs: only
//! TERMINAL rows (disabled contacts, ended contexts, consumed grants, published outbox
//! rows) age out.
//!
//! * [`worker`] runs the bounded, `SKIP LOCKED`, multi-instance-safe sweeps;
//! * [`holds`] keeps the operator holds the sweeps respect (`privacy-relay-admin
//!   retention hold|release|list`);
//! * [`audit`] appends the append-only retention evidence (`privacy_relay_retention_audit`).
//!
//! The database refuses every DELETE on evidence tables unless the transaction carries
//! `privacy_relay.retention_worker = 'on'` (set with `SET LOCAL` by the worker alone);
//! UPDATE and TRUNCATE stay refused for everyone.

pub mod audit;
pub mod holds;
pub mod worker;

use std::time::Duration;

use crate::config::ConfigError;

/// A class of stored data with its own retention policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataClass {
    /// The encrypted telephone value of a protected contact.
    ProtectedContactValue,
    /// Verification history of contacts.
    ContactVerificationEvents,
    /// Contact Resolution Grants (secret hashes, bindings).
    ContactResolutionGrants,
    /// The Contact Vault access audit (opaque references only).
    ContactAccessAudit,
    /// Communication contexts / sessions and their evidence (decisions, audit events,
    /// enforcement jobs, inbound resolutions, relay assignments, commands).
    RelayEvidence,
    /// Published outbox envelopes.
    RelayOutbox,
    /// The relay-number inventory ledger (statuses and generations; no number).
    RelayNumberEvents,
}

impl DataClass {
    pub const ALL: [DataClass; 7] = [
        DataClass::ProtectedContactValue,
        DataClass::ContactVerificationEvents,
        DataClass::ContactResolutionGrants,
        DataClass::ContactAccessAudit,
        DataClass::RelayEvidence,
        DataClass::RelayOutbox,
        DataClass::RelayNumberEvents,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            DataClass::ProtectedContactValue => "PROTECTED_CONTACT_VALUE",
            DataClass::ContactVerificationEvents => "CONTACT_VERIFICATION_EVENTS",
            DataClass::ContactResolutionGrants => "CONTACT_RESOLUTION_GRANTS",
            DataClass::ContactAccessAudit => "CONTACT_ACCESS_AUDIT",
            DataClass::RelayEvidence => "RELAY_EVIDENCE",
            DataClass::RelayOutbox => "RELAY_OUTBOX",
            DataClass::RelayNumberEvents => "RELAY_NUMBER_EVENTS",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == value)
    }
}

/// What happens to a row that aged out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionAction {
    /// The row is deleted.
    Delete,
    /// The row stays as an opaque reference; its sensitive value is erased.
    Minimize,
}

impl RetentionAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            RetentionAction::Delete => "DELETE",
            RetentionAction::Minimize => "MINIMIZE",
        }
    }
}

/// One retention policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub class: DataClass,
    pub action: RetentionAction,
    /// How long a terminal row stays before the action applies.
    pub retention: Duration,
    /// The environment variable that configures `retention` (days).
    pub configurable_by: &'static str,
}

impl RetentionPolicy {
    pub fn days(&self) -> u64 {
        self.retention.as_secs() / 86_400
    }

    /// The bounded policy label recorded in the retention audit (`VAR=days`).
    pub fn label(&self) -> String {
        format!("{}={}", self.configurable_by, self.days())
    }
}

const SECONDS_PER_DAY: u64 = 86_400;

/// Default bounds (platform conventions, days).
pub const DEFAULT_CONTACT_ERASE_DISABLED_AFTER_DAYS: u64 = 30;
pub const DEFAULT_VERIFICATION_EVENT_RETENTION_DAYS: u64 = 365;
pub const DEFAULT_GRANT_RETENTION_DAYS: u64 = 90;
pub const DEFAULT_ACCESS_AUDIT_RETENTION_DAYS: u64 = 730;
pub const DEFAULT_EVIDENCE_RETENTION_DAYS: u64 = 730;
pub const DEFAULT_OUTBOX_RETENTION_DAYS: u64 = 30;
pub const DEFAULT_NUMBER_EVENT_RETENTION_DAYS: u64 = 365;
pub const DEFAULT_RETENTION_INTERVAL_SECS: u64 = 60;
/// Rows per class per tick (and per transaction).
pub const DEFAULT_RETENTION_BATCH: u64 = 500;

/// Retention configuration (`PRIVACY_RELAY_RETENTION_*`, `CONTACT_VAULT_*_DAYS`).
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS` | 30 | a DISABLED contact's value is erased after this |
/// | `CONTACT_VAULT_VERIFICATION_EVENT_RETENTION_DAYS` | 365 | verification events are deleted after this |
/// | `CONTACT_VAULT_GRANT_RETENTION_DAYS` | 90 | terminal grants are deleted after this |
/// | `CONTACT_VAULT_ACCESS_AUDIT_RETENTION_DAYS` | 730 | access-audit rows are deleted after this (never below the erasure delay) |
/// | `PRIVACY_RELAY_EVIDENCE_RETENTION_DAYS` | 730 | terminal contexts / sessions and their evidence are deleted after this |
/// | `PRIVACY_RELAY_OUTBOX_RETENTION_DAYS` | 30 | PUBLISHED outbox rows are deleted after this |
/// | `PRIVACY_RELAY_NUMBER_EVENT_RETENTION_DAYS` | 365 | relay-number ledger rows are deleted after this |
/// | `PRIVACY_RELAY_RETENTION_INTERVAL_SECS` | 60 | worker tick interval |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSettings {
    pub contact_erase_after: Duration,
    pub verification_events: Duration,
    pub grants: Duration,
    pub access_audit: Duration,
    pub relay_evidence: Duration,
    pub outbox: Duration,
    pub number_events: Duration,
    pub interval: Duration,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            contact_erase_after: days(DEFAULT_CONTACT_ERASE_DISABLED_AFTER_DAYS),
            verification_events: days(DEFAULT_VERIFICATION_EVENT_RETENTION_DAYS),
            grants: days(DEFAULT_GRANT_RETENTION_DAYS),
            access_audit: days(DEFAULT_ACCESS_AUDIT_RETENTION_DAYS),
            relay_evidence: days(DEFAULT_EVIDENCE_RETENTION_DAYS),
            outbox: days(DEFAULT_OUTBOX_RETENTION_DAYS),
            number_events: days(DEFAULT_NUMBER_EVENT_RETENTION_DAYS),
            interval: Duration::from_secs(DEFAULT_RETENTION_INTERVAL_SECS),
        }
    }
}

fn days(n: u64) -> Duration {
    Duration::from_secs(n * SECONDS_PER_DAY)
}

/// Accepted range of every day bound.
const MIN_DAYS: u64 = 1;
const MAX_DAYS: u64 = 3650;

impl RetentionSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|k: &str| std::env::var(k).ok())
    }

    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let day_bound = |var: &'static str, default: u64| -> Result<Duration, ConfigError> {
            match value(var) {
                None => Ok(days(default)),
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|d| (MIN_DAYS..=MAX_DAYS).contains(d))
                    .map(days)
                    .ok_or_else(|| ConfigError::Invalid {
                        var,
                        reason: format!(
                            "must be an integer number of days between {MIN_DAYS} and {MAX_DAYS}"
                        ),
                    }),
            }
        };
        let settings = Self {
            contact_erase_after: day_bound(
                "CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS",
                DEFAULT_CONTACT_ERASE_DISABLED_AFTER_DAYS,
            )?,
            verification_events: day_bound(
                "CONTACT_VAULT_VERIFICATION_EVENT_RETENTION_DAYS",
                DEFAULT_VERIFICATION_EVENT_RETENTION_DAYS,
            )?,
            grants: day_bound(
                "CONTACT_VAULT_GRANT_RETENTION_DAYS",
                DEFAULT_GRANT_RETENTION_DAYS,
            )?,
            access_audit: day_bound(
                "CONTACT_VAULT_ACCESS_AUDIT_RETENTION_DAYS",
                DEFAULT_ACCESS_AUDIT_RETENTION_DAYS,
            )?,
            relay_evidence: day_bound(
                "PRIVACY_RELAY_EVIDENCE_RETENTION_DAYS",
                DEFAULT_EVIDENCE_RETENTION_DAYS,
            )?,
            outbox: day_bound(
                "PRIVACY_RELAY_OUTBOX_RETENTION_DAYS",
                DEFAULT_OUTBOX_RETENTION_DAYS,
            )?,
            number_events: day_bound(
                "PRIVACY_RELAY_NUMBER_EVENT_RETENTION_DAYS",
                DEFAULT_NUMBER_EVENT_RETENTION_DAYS,
            )?,
            interval: match value("PRIVACY_RELAY_RETENTION_INTERVAL_SECS") {
                None => Duration::from_secs(DEFAULT_RETENTION_INTERVAL_SECS),
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|s| (5..=86_400).contains(s))
                    .map(Duration::from_secs)
                    .ok_or_else(|| ConfigError::Invalid {
                        var: "PRIVACY_RELAY_RETENTION_INTERVAL_SECS",
                        reason: "must be an integer between 5 and 86400".into(),
                    })?,
            },
        };
        // The access audit is the evidence of an erasure: it must outlive the erasure delay.
        if settings.access_audit < settings.contact_erase_after {
            return Err(ConfigError::Invalid {
                var: "CONTACT_VAULT_ACCESS_AUDIT_RETENTION_DAYS",
                reason: "must not be shorter than CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS".into(),
            });
        }
        Ok(settings)
    }

    /// Every policy, in sweep order.
    pub fn policies(&self) -> Vec<RetentionPolicy> {
        vec![
            RetentionPolicy {
                class: DataClass::ProtectedContactValue,
                action: RetentionAction::Minimize,
                retention: self.contact_erase_after,
                configurable_by: "CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS",
            },
            RetentionPolicy {
                class: DataClass::ContactVerificationEvents,
                action: RetentionAction::Delete,
                retention: self.verification_events,
                configurable_by: "CONTACT_VAULT_VERIFICATION_EVENT_RETENTION_DAYS",
            },
            RetentionPolicy {
                class: DataClass::ContactResolutionGrants,
                action: RetentionAction::Delete,
                retention: self.grants,
                configurable_by: "CONTACT_VAULT_GRANT_RETENTION_DAYS",
            },
            RetentionPolicy {
                class: DataClass::ContactAccessAudit,
                action: RetentionAction::Delete,
                retention: self.access_audit,
                configurable_by: "CONTACT_VAULT_ACCESS_AUDIT_RETENTION_DAYS",
            },
            RetentionPolicy {
                class: DataClass::RelayEvidence,
                action: RetentionAction::Delete,
                retention: self.relay_evidence,
                configurable_by: "PRIVACY_RELAY_EVIDENCE_RETENTION_DAYS",
            },
            RetentionPolicy {
                class: DataClass::RelayOutbox,
                action: RetentionAction::Delete,
                retention: self.outbox,
                configurable_by: "PRIVACY_RELAY_OUTBOX_RETENTION_DAYS",
            },
            RetentionPolicy {
                class: DataClass::RelayNumberEvents,
                action: RetentionAction::Delete,
                retention: self.number_events,
                configurable_by: "PRIVACY_RELAY_NUMBER_EVENT_RETENTION_DAYS",
            },
        ]
    }

    pub fn policy(&self, class: DataClass) -> RetentionPolicy {
        self.policies()
            .into_iter()
            .find(|p| p.class == class)
            .expect("every class has a policy")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn defaults_are_the_platform_conventions_and_every_class_has_one_policy() {
        let s = RetentionSettings::from_lookup(&lookup(&[])).unwrap();
        assert_eq!(s, RetentionSettings::default());
        let policies = s.policies();
        assert_eq!(policies.len(), DataClass::ALL.len());
        for class in DataClass::ALL {
            let p = s.policy(class);
            assert_eq!(p.class, class);
            assert_eq!(DataClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(s.policy(DataClass::ProtectedContactValue).days(), 30);
        assert_eq!(
            s.policy(DataClass::ProtectedContactValue).action,
            RetentionAction::Minimize
        );
        assert_eq!(s.policy(DataClass::ContactAccessAudit).days(), 730);
        assert_eq!(
            s.policy(DataClass::RelayOutbox).label(),
            "PRIVACY_RELAY_OUTBOX_RETENTION_DAYS=30"
        );
        assert_eq!(s.interval, Duration::from_secs(60));
    }

    #[test]
    fn bounds_are_validated_and_the_audit_outlives_the_erasure_delay() {
        let ok = RetentionSettings::from_lookup(&lookup(&[
            ("CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS", "7"),
            ("CONTACT_VAULT_ACCESS_AUDIT_RETENTION_DAYS", "7"),
            ("PRIVACY_RELAY_RETENTION_INTERVAL_SECS", "5"),
        ]))
        .unwrap();
        assert_eq!(ok.contact_erase_after, days(7));
        assert_eq!(ok.interval, Duration::from_secs(5));
        for (k, v) in [
            ("CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS", "0"),
            ("CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS", "9999"),
            ("CONTACT_VAULT_GRANT_RETENTION_DAYS", "soon"),
            ("PRIVACY_RELAY_EVIDENCE_RETENTION_DAYS", "-1"),
            ("PRIVACY_RELAY_RETENTION_INTERVAL_SECS", "1"),
        ] {
            let err = RetentionSettings::from_lookup(&lookup(&[(k, v)])).unwrap_err();
            assert!(err.to_string().contains(k), "{k}={v}: {err}");
        }
        let err = RetentionSettings::from_lookup(&lookup(&[
            ("CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS", "60"),
            ("CONTACT_VAULT_ACCESS_AUDIT_RETENTION_DAYS", "59"),
        ]))
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("CONTACT_VAULT_ACCESS_AUDIT_RETENTION_DAYS"));
    }
}

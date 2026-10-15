//! Data retention, deletion and minimization (Part H §80, ADR 0030): Calling's part.
//!
//! Every class of data Calling keeps has ONE typed policy ([`RetentionPolicy`]): what it
//! is ([`DataClass`]), what happens once its bound passes ([`RetentionAction`]), after how
//! long and which environment variable configures that bound. The bounds are platform
//! conventions with documented defaults, never a legal claim; an operator sets them per
//! deployment. The vocabulary is the one Privacy Relay (`services/privacy-relay/src/retention`)
//! and Telephony (`services/telephony/src/retention`) use.
//!
//! | Class | Action | Bound (env, default) |
//! |---|---|---|
//! | `CALL_RECORD` | Minimize display metadata of terminal Calls | `CALLING_RETENTION_CALL_DISPLAY_DAYS` 365 |
//! | `CALL_EVENT` | Retain (outbox source, transition evidence) | — (`CALLING_RETENTION_CALL_EVENTS_DAYS` must stay unset) |
//! | `CALL_USAGE` | Retain (immutable usage facts) | — |
//! | `SPEND_EVIDENCE` | Retain | — |
//! | `WEBHOOK_EVENT_HISTORY` | Delete events without a delivery | `CALLING_RETENTION_WEBHOOK_HISTORY_DAYS` 90 |
//! | `WEBHOOK_DELIVERY_HISTORY` | Delete terminal deliveries + attempts | `CALLING_RETENTION_WEBHOOK_HISTORY_DAYS` 90 |
//! | `CALLING_INCIDENT` | Delete RESOLVED incidents | `CALLING_RETENTION_INCIDENTS_DAYS` 365 |
//! | `MEDIA_PROVIDER_EVIDENCE` | Delete provider events of terminal / unknown Calls | `CALLING_RETENTION_MEDIA_PROVIDER_EVENTS_DAYS` 30 |
//! | `OPERATOR_AUDIT` | Delete after its own bound | `CALLING_RETENTION_OPERATOR_AUDIT_DAYS` 2555 |
//! | `ADMIN_AUDIT` / `RECORDING_ACCESS_AUDIT` | Retain | — |
//! | `RECORDING_ARTIFACT` / `VOICEMAIL_ARTIFACT` | Delete per recording policy / mailbox (Part F workers; honour holds) | per policy |
//! | `PRINCIPAL` | Minimize on operator tombstone | — |
//!
//! * [`worker`] runs the bounded, `FOR UPDATE SKIP LOCKED`, multi-instance-safe sweeps;
//! * [`holds`] keeps the operator holds every retention action respects
//!   (`platform_admin retention hold|release|list|policies`, operator-audited);
//! * [`audit`] appends `calling_retention_audit` (never content);
//! * [`tombstone`] minimizes a principal (`platform_admin principal tombstone`).
//!
//! The database refuses every DELETE on the purgeable evidence tables unless the
//! transaction carries `calling.retention_worker = 'on'` (set with `SET LOCAL` by this
//! module alone); UPDATE and TRUNCATE stay refused for everyone, and Calls, events, usage
//! and spend keep unconditional no-delete guards.

pub mod audit;
pub mod holds;
pub mod tombstone;
pub mod worker;

use std::time::Duration;

use serde::Serialize;

use crate::config::ConfigError;

text_enum!(
    /// A class of stored data with its own retention policy.
    DataClass {
        CallRecord = "CALL_RECORD",
        CallEvent = "CALL_EVENT",
        CallUsage = "CALL_USAGE",
        SpendEvidence = "SPEND_EVIDENCE",
        WebhookEventHistory = "WEBHOOK_EVENT_HISTORY",
        WebhookDeliveryHistory = "WEBHOOK_DELIVERY_HISTORY",
        CallingIncident = "CALLING_INCIDENT",
        MediaProviderEvidence = "MEDIA_PROVIDER_EVIDENCE",
        OperatorAudit = "OPERATOR_AUDIT",
        AdminAudit = "ADMIN_AUDIT",
        RecordingAccessAudit = "RECORDING_ACCESS_AUDIT",
        RecordingArtifact = "RECORDING_ARTIFACT",
        VoicemailArtifact = "VOICEMAIL_ARTIFACT",
        Principal = "PRINCIPAL",
    }
);

text_enum!(
    /// What happens once a class's bound passes: DELETE (bounded batches, worker flag),
    /// MINIMIZE (the rows stay as opaque evidence; identifying / display content is
    /// erased) or RETAIN (kept: evidence another record needs; status only).
    RetentionAction {
        Delete = "DELETE",
        Minimize = "MINIMIZE",
        Retain = "RETAIN",
    }
);

text_enum!(
    /// What one retention audit row records.
    RetentionOutcome {
        Deleted = "DELETED",
        Minimized = "MINIMIZED",
        SkippedHold = "SKIPPED_HOLD",
        Failed = "FAILED",
    }
);

/// One retention policy (configuration, never a legal claim).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicy {
    #[serde(rename = "dataClass")]
    pub class: DataClass,
    pub action: RetentionAction,
    /// The bound after which `action` applies (`None`: retained, or a per-resource policy).
    #[serde(skip)]
    pub retention: Option<Duration>,
    /// The environment variable that configures `retention`, or a bounded marker
    /// (`RETAINED`, `PER_POLICY`, `OPERATOR_TOMBSTONE`).
    pub configured_by: &'static str,
    pub description: &'static str,
}

impl RetentionPolicy {
    /// The bound in days (`None` when there is no configured bound).
    pub fn days(&self) -> Option<u64> {
        self.retention.map(|d| d.as_secs() / SECONDS_PER_DAY)
    }

    /// The bounded policy label written to the retention audit (`VAR=days`, or the marker).
    pub fn label(&self) -> String {
        match self.days() {
            Some(days) => format!("{}={days}", self.configured_by),
            None => self.configured_by.to_string(),
        }
    }
}

const SECONDS_PER_DAY: u64 = 86_400;

/// Default bounds (platform conventions, days).
pub const DEFAULT_CALL_DISPLAY_DAYS: u64 = 365;
pub const DEFAULT_WEBHOOK_HISTORY_DAYS: u64 = 90;
pub const DEFAULT_INCIDENTS_DAYS: u64 = 365;
pub const DEFAULT_MEDIA_PROVIDER_EVENTS_DAYS: u64 = 30;
pub const DEFAULT_OPERATOR_AUDIT_DAYS: u64 = 2555;
pub const DEFAULT_RETENTION_INTERVAL_SECS: u64 = 60;
/// Rows per class per tick (and per transaction): the hard upper bound of a batch.
pub const MAX_RETENTION_BATCH: u64 = 500;

/// The variable that would bound call events: refused when set (they are retained).
pub const CALL_EVENTS_DAYS_VAR: &str = "CALLING_RETENTION_CALL_EVENTS_DAYS";

/// Retention configuration (`CALLING_RETENTION_*`).
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `CALLING_RETENTION_CALL_DISPLAY_DAYS` | 365 | display metadata of a terminal Call is minimized this long after it ended |
/// | `CALLING_RETENTION_CALL_EVENTS_DAYS` | unset | must stay unset: call events are retained (startup refuses a value) |
/// | `CALLING_RETENTION_WEBHOOK_HISTORY_DAYS` | 90 | terminal webhook deliveries (+ attempts) and delivery-less events are deleted after this |
/// | `CALLING_RETENTION_INCIDENTS_DAYS` | 365 | RESOLVED Calling incidents are deleted this long after resolution |
/// | `CALLING_RETENTION_MEDIA_PROVIDER_EVENTS_DAYS` | 30 | media / telephony provider events of terminal or unknown Calls are deleted after this |
/// | `CALLING_RETENTION_OPERATOR_AUDIT_DAYS` | 2555 | operator audit rows are deleted after this (own bound) |
/// | `CALLING_RETENTION_INTERVAL_SECS` | 60 | worker tick interval (5–86400) |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSettings {
    pub call_display: Duration,
    pub webhook_history: Duration,
    pub incidents: Duration,
    pub media_provider_events: Duration,
    pub operator_audit: Duration,
    pub interval: Duration,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            call_display: days(DEFAULT_CALL_DISPLAY_DAYS),
            webhook_history: days(DEFAULT_WEBHOOK_HISTORY_DAYS),
            incidents: days(DEFAULT_INCIDENTS_DAYS),
            media_provider_events: days(DEFAULT_MEDIA_PROVIDER_EVENTS_DAYS),
            operator_audit: days(DEFAULT_OPERATOR_AUDIT_DAYS),
            interval: std::time::Duration::from_secs(DEFAULT_RETENTION_INTERVAL_SECS),
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

    /// Invalid values refuse startup (never a silent default).
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
                    .ok_or_else(|| {
                        ConfigError::invalid(
                            var,
                            format!(
                                "must be an integer number of days between {MIN_DAYS} and {MAX_DAYS}"
                            ),
                        )
                    }),
            }
        };
        if value(CALL_EVENTS_DAYS_VAR).is_some() {
            return Err(ConfigError::invalid(
                CALL_EVENTS_DAYS_VAR,
                "call events are retained (they are the outbox source and the evidence of every \
                 state transition); leave it unset"
                    .to_string(),
            ));
        }
        Ok(Self {
            call_display: day_bound(
                "CALLING_RETENTION_CALL_DISPLAY_DAYS",
                DEFAULT_CALL_DISPLAY_DAYS,
            )?,
            webhook_history: day_bound(
                "CALLING_RETENTION_WEBHOOK_HISTORY_DAYS",
                DEFAULT_WEBHOOK_HISTORY_DAYS,
            )?,
            incidents: day_bound("CALLING_RETENTION_INCIDENTS_DAYS", DEFAULT_INCIDENTS_DAYS)?,
            media_provider_events: day_bound(
                "CALLING_RETENTION_MEDIA_PROVIDER_EVENTS_DAYS",
                DEFAULT_MEDIA_PROVIDER_EVENTS_DAYS,
            )?,
            operator_audit: day_bound(
                "CALLING_RETENTION_OPERATOR_AUDIT_DAYS",
                DEFAULT_OPERATOR_AUDIT_DAYS,
            )?,
            interval: match value("CALLING_RETENTION_INTERVAL_SECS") {
                None => Duration::from_secs(DEFAULT_RETENTION_INTERVAL_SECS),
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|s| (5..=86_400).contains(s))
                    .map(Duration::from_secs)
                    .ok_or_else(|| {
                        ConfigError::invalid(
                            "CALLING_RETENTION_INTERVAL_SECS",
                            "must be an integer between 5 and 86400".to_string(),
                        )
                    })?,
            },
        })
    }

    /// The policy of `class`.
    pub fn policy(&self, class: DataClass) -> RetentionPolicy {
        policies(self)
            .into_iter()
            .find(|p| p.class == class)
            .expect("every class has a policy")
    }
}

/// Every Calling retention policy, one per [`DataClass`], in sweep order.
pub fn policies(settings: &RetentionSettings) -> Vec<RetentionPolicy> {
    use DataClass as C;
    use RetentionAction as A;
    let policy = |class, action, retention, configured_by, description| RetentionPolicy {
        class,
        action,
        retention,
        configured_by,
        description,
    };
    vec![
        policy(
            C::CallRecord,
            A::Minimize,
            Some(settings.call_display),
            "CALLING_RETENTION_CALL_DISPLAY_DAYS",
            "terminal Calls: participant display names and the enterprise entry label are erased; \
             ids, state, outcome, timestamps, principal / ContactReference relations and evidence links stay",
        ),
        policy(
            C::WebhookDeliveryHistory,
            A::Delete,
            Some(settings.webhook_history),
            "CALLING_RETENTION_WEBHOOK_HISTORY_DAYS",
            "SUCCEEDED / FAILED / CANCELLED deliveries and their attempts; PENDING, DELIVERING and \
             RETRY_SCHEDULED deliveries are never deleted",
        ),
        policy(
            C::WebhookEventHistory,
            A::Delete,
            Some(settings.webhook_history),
            "CALLING_RETENTION_WEBHOOK_HISTORY_DAYS",
            "normalized webhook events once no delivery of them remains",
        ),
        policy(
            C::CallingIncident,
            A::Delete,
            Some(settings.incidents),
            "CALLING_RETENTION_INCIDENTS_DAYS",
            "RESOLVED incidents, counted from resolution; OPEN incidents are never deleted",
        ),
        policy(
            C::MediaProviderEvidence,
            A::Delete,
            Some(settings.media_provider_events),
            "CALLING_RETENTION_MEDIA_PROVIDER_EVENTS_DAYS",
            "media-provider and telephony provider events of terminal (or unknown) Calls",
        ),
        policy(
            C::OperatorAudit,
            A::Delete,
            Some(settings.operator_audit),
            "CALLING_RETENTION_OPERATOR_AUDIT_DAYS",
            "operator audit rows, on their own bound (independent of the audited resource)",
        ),
        policy(
            C::CallEvent,
            A::Retain,
            None,
            "RETAINED",
            "call events: the outbox source (RESTRICT) and the evidence of every state transition",
        ),
        policy(
            C::CallUsage,
            A::Retain,
            None,
            "RETAINED",
            "immutable usage facts (never rewritten, never deleted)",
        ),
        policy(
            C::SpendEvidence,
            A::Retain,
            None,
            "RETAINED",
            "spend reservations and telecom cost evidence",
        ),
        policy(
            C::AdminAudit,
            A::Retain,
            None,
            "RETAINED",
            "enterprise administration audit (append-only)",
        ),
        policy(
            C::RecordingAccessAudit,
            A::Retain,
            None,
            "RETAINED",
            "recording access audit (append-only; outlives every recording)",
        ),
        policy(
            C::RecordingArtifact,
            A::Delete,
            None,
            "PER_POLICY",
            "recording objects: the recording policy's retentionDays (Part F worker; honours holds)",
        ),
        policy(
            C::VoicemailArtifact,
            A::Delete,
            None,
            "PER_POLICY",
            "voicemail objects: the mailbox's retentionDays (Part F worker; honours holds)",
        ),
        policy(
            C::Principal,
            A::Minimize,
            None,
            "OPERATOR_TOMBSTONE",
            "on operator tombstone: DEACTIVATED, reference replaced by a non-identifying value, \
             display names of its participations erased; ids and relations stay",
        ),
    ]
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
        let all = policies(&s);
        assert_eq!(all.len(), DataClass::ALL.len());
        for class in DataClass::ALL {
            assert_eq!(all.iter().filter(|p| p.class == *class).count(), 1);
            assert_eq!(DataClass::parse(class.as_str()), Some(*class));
        }
        assert_eq!(s.policy(DataClass::CallRecord).days(), Some(365));
        assert_eq!(
            s.policy(DataClass::CallRecord).action,
            RetentionAction::Minimize
        );
        assert_eq!(
            s.policy(DataClass::OperatorAudit).label(),
            "CALLING_RETENTION_OPERATOR_AUDIT_DAYS=2555"
        );
        assert_eq!(
            s.policy(DataClass::CallUsage).action,
            RetentionAction::Retain
        );
        assert_eq!(s.policy(DataClass::CallEvent).label(), "RETAINED");
        assert_eq!(s.interval, Duration::from_secs(60));
    }

    #[test]
    fn bounds_are_validated_and_call_events_cannot_be_given_a_bound() {
        let ok = RetentionSettings::from_lookup(&lookup(&[
            ("CALLING_RETENTION_WEBHOOK_HISTORY_DAYS", "7"),
            ("CALLING_RETENTION_INTERVAL_SECS", "5"),
        ]))
        .unwrap();
        assert_eq!(ok.webhook_history, days(7));
        assert_eq!(ok.interval, Duration::from_secs(5));
        for (k, v) in [
            ("CALLING_RETENTION_CALL_DISPLAY_DAYS", "0"),
            ("CALLING_RETENTION_INCIDENTS_DAYS", "9999"),
            ("CALLING_RETENTION_MEDIA_PROVIDER_EVENTS_DAYS", "soon"),
            ("CALLING_RETENTION_OPERATOR_AUDIT_DAYS", "-1"),
            ("CALLING_RETENTION_INTERVAL_SECS", "1"),
            ("CALLING_RETENTION_CALL_EVENTS_DAYS", "30"),
        ] {
            let err = RetentionSettings::from_lookup(&lookup(&[(k, v)])).unwrap_err();
            assert!(err.to_string().contains(k), "{k}={v}: {err}");
        }
    }
}

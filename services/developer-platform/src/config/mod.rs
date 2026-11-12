//! Developer Platform configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | the Developer Platform's OWN PostgreSQL database |
//! | `DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS` | no | pool size (1–200, default 10) |
//! | `DEVELOPER_PLATFORM_BIND_ADDRESS` | no | default `127.0.0.1:8098` |
//! | `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED` | no | feature flag (default `false`): serve requests the API Gateway forwards; off, they are 404 |
//! | `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED` | no | feature flag (default `false`): customer production-access submit / cancel / activate; off, they are 503 `CAPABILITY_UNAVAILABLE` (reads and operator decisions stay) |
//! | `DEVELOPER_PLATFORM_WORKER_INTERVAL_MS` | no | worker loop interval (100–600000, default 5000) |
//! | `DEVELOPER_PLATFORM_WORKER_INSTANCE_ID` | no | worker instance id (default `worker-<pid>`) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//! | `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` | no | feature flag (default `false`): central customer webhooks (Section 53); off, every webhook route is 404 |
//! | `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` | no | `shadow` (default: ingest and record, never send) / `active` |
//! | `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY` (`_ID`, `_PREVIOUS`, `_PREVIOUS_ID`, `_FILE` variants) | no | signing-secret sealing key ring; unset = webhook subscription routes 503 (fail closed) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` | no | http / private endpoints, `local` runtime only |
//! | `DEVELOPER_PLATFORM_WEBHOOK_REQUEST_TIMEOUT_MS`, `_MAX_ATTEMPTS`, `_AUTO_DISABLE_AFTER` | no | delivery timeout, attempts before dead letter, consecutive failures before automatic disablement |
//! | `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN` | no | Section 53 migration freeze of customer / console webhook mutations (default `false`) |
//! | `DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY` (`_FILE`, `_ID`) | no | the webhook migration transfer key (Section 53 §9.1); unset = secret import / rollback export refused |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED` | no | PLT-008 credential lifecycle webhooks (`credential.expiring_soon` / `credential.expired`; default `true`; acts only with webhooks enabled and Platform Access configured) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS` | no | lead times in days, comma-separated, 1–8 distinct values of 1–365 (default `14,7,1`) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS` | no | how long after expiry `credential.expired` is still sent (0–90, default 7) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS` | no | minimum time between two sweeps across all worker instances (60–86400, default 3600) |
//! | `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_MAX_ENVIRONMENTS` | no | subscribed environments examined per sweep (1–10000, default 500) |
//! | `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS` | no | DATA-017 report-only reference reconciliation logged by the worker (0 = off, else 300–604800; default 86400) |
//! | `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT` | no | ids of each kind verified at Platform Access per report (0 = local checks only, max 100000; default 1000) |
//!
//! The webhook variables are detailed in [`webhooks`]. Service authentication (`CF_*`) is
//! read by [`service_auth`]. Error messages never echo a value.

pub mod service_auth;
pub mod webhooks;

use std::{fmt, net::SocketAddr, time::Duration};

use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    /// The message never echoes the offending value (it may be a secret).
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

impl ConfigError {
    pub fn invalid(var: &'static str, reason: impl Into<String>) -> Self {
        ConfigError::Invalid {
            var,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

/// Disabled-by-default feature flags (Part VI rollout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeatureFlags {
    /// Serve customer requests forwarded by the API Gateway (`/internal/v1/gateway/**`).
    pub gateway_api: bool,
    /// Section 55: software-only production-access requests may be approved by the
    /// AUTOMATIC policy (`DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL`, default off).
    /// Regulated telecom / numbering / SMS capabilities always need an operator.
    pub production_auto_approval: bool,
    /// Part VI §21.2 `PRODUCTION_ACCESS_WORKFLOW_ENABLED`: the customer self-service
    /// production-access flow (submit / cancel / activate). Off: those answer 503
    /// `CAPABILITY_UNAVAILABLE`; reads and operator decisions (review, suspension) stay
    /// available so an operator can always act on existing requests.
    pub production_access_workflow: bool,
}

pub struct DeveloperPlatformConfig {
    /// Final completion (production-access workflow): the named operators allowed to
    /// review, approve or reject production-access requests
    /// (`DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS`, comma separated operator names). Empty:
    /// any operator holding `developer-platform.administer` (previous behaviour). Customer
    /// submissions (`…_PRODUCTION_ACCESS_WORKFLOW_ENABLED`) require at least one reviewer.
    pub production_reviewers: Vec<String>,
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub flags: FeatureFlags,
    pub worker_interval: Duration,
    pub worker_instance_id: String,
    pub log_format: LogFormat,
    /// Central customer webhooks (Section 53).
    pub webhooks: crate::webhooks::WebhookSettings,
    /// Section 15 (PLT-008): credential lifecycle webhooks.
    pub credential_lifecycle: crate::control::credential_lifecycle::CredentialLifecycleSettings,
    /// DATA-017: period of the worker's report-only reference reconciliation (None: off).
    pub reference_reconciliation_interval: Option<Duration>,
    /// DATA-017: ids of each kind verified at Platform Access per report.
    pub reference_reconciliation_remote_limit: u64,
}

impl fmt::Debug for DeveloperPlatformConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeveloperPlatformConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("flags", &self.flags)
            .field("worker_interval", &self.worker_interval)
            .field("worker_instance_id", &self.worker_instance_id)
            .field("webhooks", &self.webhooks)
            .field("credential_lifecycle", &self.credential_lifecycle)
            .field(
                "reference_reconciliation_interval",
                &self.reference_reconciliation_interval,
            )
            .finish_non_exhaustive()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8098";

pub(crate) fn bounded(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, ConfigError> {
    match get(var) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| {
                ConfigError::invalid(var, format!("must be an integer between {min} and {max}"))
            }),
    }
}

/// A boolean flag: `true` / `false` (unset: `false`, fail closed).
pub(crate) fn flag(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<bool, ConfigError> {
    match get(var).as_deref() {
        None | Some("false") | Some("0") => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some(_) => Err(ConfigError::invalid(var, "must be `true` or `false`")),
    }
}

/// A worker instance id: `[a-z0-9][a-z0-9._-]{0,63}` (the persisted format).
pub fn valid_instance_id(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// The PLT-008 credential lifecycle settings (defaults: on, 14/7/1 days, 7-day lookback,
/// hourly, 500 environments per sweep).
fn credential_lifecycle(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<crate::control::credential_lifecycle::CredentialLifecycleSettings, ConfigError> {
    use crate::control::credential_lifecycle::{parse_warning_days, CredentialLifecycleSettings};
    let defaults = CredentialLifecycleSettings::default();
    let enabled = match get("DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED").as_deref() {
        None | Some("true") | Some("1") => true,
        Some("false") | Some("0") => false,
        Some(_) => {
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED",
                "must be `true` or `false`",
            ))
        }
    };
    let warning_days = match get("DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS") {
        None => defaults.warning_days,
        Some(v) => parse_warning_days(&v).ok_or_else(|| {
            ConfigError::invalid(
                "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS",
                "must be 1-8 distinct comma-separated days between 1 and 365",
            )
        })?,
    };
    Ok(CredentialLifecycleSettings {
        enabled,
        warning_days,
        expired_lookback: chrono::Duration::days(bounded(
            get,
            "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS",
            7,
            0,
            90,
        )?),
        interval: Duration::from_secs(bounded(
            get,
            "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS",
            3_600,
            60,
            86_400,
        )? as u64),
        max_environments: bounded(
            get,
            "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_MAX_ENVIRONMENTS",
            500,
            1,
            10_000,
        )? as u64,
    })
}

impl DeveloperPlatformConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Builds the configuration from an arbitrary key lookup (environment, tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let database_url =
            get("DATABASE_URL")
                .map(SecretString::from)
                .ok_or(ConfigError::Missing {
                    var: "DATABASE_URL",
                })?;
        let bind_address = match get("DEVELOPER_PLATFORM_BIND_ADDRESS") {
            None => DEFAULT_BIND_ADDRESS.parse().expect("valid default"),
            Some(v) => v.parse::<SocketAddr>().map_err(|_| {
                ConfigError::invalid(
                    "DEVELOPER_PLATFORM_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8098",
                )
            })?,
        };
        let database_max_connections = bounded(
            &get,
            "DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS",
            10,
            1,
            200,
        )? as u32;
        let flags = FeatureFlags {
            gateway_api: flag(&get, "DEVELOPER_PLATFORM_GATEWAY_API_ENABLED")?,
            production_auto_approval: flag(&get, "DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL")?,
            production_access_workflow: flag(
                &get,
                "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
            )?,
        };
        let production_reviewers: Vec<String> = get("DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS")
            .unwrap_or_default()
            .split(',')
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        if production_reviewers
            .iter()
            .any(|r| !crate::control::operator::valid_operator(r))
        {
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS",
                "must be comma-separated operator names",
            ));
        }
        if flags.production_access_workflow && production_reviewers.is_empty() {
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS",
                "customer production-access submissions need at least one named reviewer",
            ));
        }
        if flags.production_auto_approval {
            // Owner decision (final completion): manual approval only.
            return Err(ConfigError::invalid(
                "DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL",
                "automatic production approval is not permitted; reviewers decide manually",
            ));
        }
        let worker_interval = Duration::from_millis(bounded(
            &get,
            "DEVELOPER_PLATFORM_WORKER_INTERVAL_MS",
            5_000,
            100,
            600_000,
        )? as u64);
        let worker_instance_id = match get("DEVELOPER_PLATFORM_WORKER_INSTANCE_ID") {
            None => format!("worker-{}", std::process::id()),
            Some(v) if valid_instance_id(&v) => v,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "DEVELOPER_PLATFORM_WORKER_INSTANCE_ID",
                    "must be 1-64 characters of [a-z0-9._-], starting alphanumeric",
                ))
            }
        };
        let log_format = match get("LOG_FORMAT").as_deref() {
            None | Some("pretty") => LogFormat::Pretty,
            Some("json") => LogFormat::Json,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "LOG_FORMAT",
                    "must be `pretty` or `json`",
                ))
            }
        };
        let webhooks = webhooks::from_lookup(&get)?;
        let credential_lifecycle = credential_lifecycle(&get)?;
        let reference_reconciliation_interval = match bounded(
            &get,
            "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
            86_400,
            0,
            604_800,
        )? {
            0 => None,
            n if n < 300 => {
                return Err(ConfigError::invalid(
                    "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
                    "must be 0 (off) or between 300 and 604800",
                ))
            }
            n => Some(Duration::from_secs(n as u64)),
        };
        let reference_reconciliation_remote_limit = bounded(
            &get,
            "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT",
            1_000,
            0,
            100_000,
        )? as u64;
        Ok(Self {
            production_reviewers,
            database_url,
            database_max_connections,
            bind_address,
            flags,
            worker_interval,
            worker_instance_id,
            log_format,
            webhooks,
            credential_lifecycle,
            reference_reconciliation_interval,
            reference_reconciliation_remote_limit,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
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

    const BASE: &[(&str, &str)] = &[("DATABASE_URL", "postgres://devplat:pw-secret-9@db/dp")];

    #[test]
    fn requires_its_own_database_and_flags_default_off() {
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(BASE)).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert_eq!(cfg.flags, FeatureFlags::default());
        assert!(!cfg.flags.gateway_api, "disabled by default");
        assert!(
            !cfg.flags.production_access_workflow,
            "the production-access workflow is disabled by default"
        );
        assert!(!format!("{cfg:?}").contains("pw-secret-9"));
        assert!(DeveloperPlatformConfig::from_lookup(lookup(&[])).is_err());
    }

    #[test]
    fn production_access_needs_named_reviewers_and_never_auto_approves() {
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut v: Vec<(&str, &str)> = BASE.to_vec();
            v.extend_from_slice(extra);
            DeveloperPlatformConfig::from_lookup(lookup(&v))
        };
        assert!(with(&[(
            "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
            "true"
        )])
        .is_err());
        let cfg = with(&[
            (
                "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
                "true",
            ),
            (
                "DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS",
                "github-alice, github-bob",
            ),
        ])
        .unwrap();
        assert_eq!(cfg.production_reviewers, ["github-alice", "github-bob"]);
        assert!(with(&[("DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS", "bad name")]).is_err());
        assert!(with(&[("DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL", "true")]).is_err());
        assert!(with(&[]).unwrap().production_reviewers.is_empty());
    }

    #[test]
    fn credential_lifecycle_and_reconciliation_have_sane_defaults() {
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(BASE)).unwrap();
        let c = &cfg.credential_lifecycle;
        assert!(c.enabled);
        assert_eq!(c.warning_days, vec![14, 7, 1]);
        assert_eq!(c.expired_lookback, chrono::Duration::days(7));
        assert_eq!(c.interval, Duration::from_secs(3600));
        assert_eq!(c.max_environments, 500);
        assert_eq!(
            cfg.reference_reconciliation_interval,
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(cfg.reference_reconciliation_remote_limit, 1_000);
        let mut pairs = BASE.to_vec();
        pairs.extend([
            ("DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED", "false"),
            ("DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS", "3, 30"),
            ("DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS", "0"),
            (
                "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
                "0",
            ),
        ]);
        let cfg = DeveloperPlatformConfig::from_lookup(lookup(&pairs)).unwrap();
        assert!(!cfg.credential_lifecycle.enabled);
        assert_eq!(cfg.credential_lifecycle.warning_days, vec![30, 3]);
        assert_eq!(cfg.reference_reconciliation_interval, None);
    }

    #[test]
    fn invalid_values_are_refused_without_echo() {
        for (var, value) in [
            ("DEVELOPER_PLATFORM_BIND_ADDRESS", "not-an-address-xyz"),
            ("DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS", "999"),
            ("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "maybe-later"),
            ("DEVELOPER_PLATFORM_WORKER_INTERVAL_MS", "5"),
            ("DEVELOPER_PLATFORM_WORKER_INSTANCE_ID", "Has Space"),
            ("LOG_FORMAT", "xml-format"),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED",
                "perhaps-yes",
            ),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS",
                "14,14,9999",
            ),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS",
                "9191",
            ),
            ("DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS", "5"),
            (
                "DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_MAX_ENVIRONMENTS",
                "-3",
            ),
            (
                "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS",
                "59",
            ),
            (
                "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT",
                "-5",
            ),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            let err = DeveloperPlatformConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
        let mut on = BASE.to_vec();
        on.push(("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "true"));
        assert!(
            DeveloperPlatformConfig::from_lookup(lookup(&on))
                .unwrap()
                .flags
                .gateway_api
        );
    }
}

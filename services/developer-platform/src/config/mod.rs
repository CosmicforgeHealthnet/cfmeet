//! Developer Platform configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | the Developer Platform's OWN PostgreSQL database |
//! | `DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS` | no | pool size (1–200, default 10) |
//! | `DEVELOPER_PLATFORM_BIND_ADDRESS` | no | default `127.0.0.1:8098` |
//! | `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED` | no | feature flag (default `false`): serve requests the API Gateway forwards; off, they are 404 |
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
}

pub struct DeveloperPlatformConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub flags: FeatureFlags,
    pub worker_interval: Duration,
    pub worker_instance_id: String,
    pub log_format: LogFormat,
    /// Central customer webhooks (Section 53).
    pub webhooks: crate::webhooks::WebhookSettings,
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
        };
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
        Ok(Self {
            database_url,
            database_max_connections,
            bind_address,
            flags,
            worker_interval,
            worker_instance_id,
            log_format,
            webhooks,
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
        assert!(!format!("{cfg:?}").contains("pw-secret-9"));
        assert!(DeveloperPlatformConfig::from_lookup(lookup(&[])).is_err());
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

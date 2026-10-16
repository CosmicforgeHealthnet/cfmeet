//! Numbering configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | Numbering's OWN PostgreSQL database |
//! | `NUMBERING_DATABASE_MAX_CONNECTIONS` | no | pool size (default 10) |
//! | `NUMBERING_BIND_ADDRESS` | no | default `127.0.0.1:8095` |
//! | `NUMBERING_METRICS_ADDRESS` | no | separate `GET /metrics` listener |
//! | `NUMBERING_WORKER_INTERVAL_MS` | no | worker poll interval (100–60000, default 1000) |
//! | `NUMBERING_WORKER_LEASE_SECONDS` | no | lease of a claimed outbox event / job (5–3600, default 60) |
//! | `NUMBERING_OUTBOX_MAX_ATTEMPTS` | no | publication attempts before DEAD_LETTER (1–100, default 12) |
//! | `NUMBERING_RETRY_BASE_MS` / `NUMBERING_RETRY_MAX_MS` | no | exponential backoff bounds (default 1000 / 300000) |
//! | `NUMBERING_IDEMPOTENCY_TTL_HOURS` | no | how long idempotency keys are kept (1–720, default 24) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//!
//! Acquisition limits (`NUMBERING_SEARCH_*`, `NUMBERING_RESERVATION_TTL_SECONDS`,
//! `NUMBERING_PRE_ELIGIBILITY_HOLDS`, …) are read by [`acquisition`].
//!
//! Routing grant signing (`NUMBERING_GRANT_*`, a dedicated key) is read by [`grants`].
//!
//! Service authentication (`CF_*`, including `CF_PLATFORM_ACCESS_BASE_URL`) is read by
//! [`service_auth`]. The database URL is held in a redacted container. Numbering holds
//! no other secret beyond its service-auth and grant keys (files): customer credentials are
//! verified by Platform Access.

pub mod acquisition;
pub mod grants;
pub mod kyc;
pub mod providers;
pub mod service_auth;

use std::{fmt, net::SocketAddr, time::Duration};

use secrecy::{ExposeSecret, SecretString};

use crate::infra::RetryPolicy;

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

/// Worker and reliability tunables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSettings {
    pub interval: Duration,
    pub lease: chrono::Duration,
    pub outbox_max_attempts: i32,
    pub retry: RetryPolicy,
    pub idempotency_ttl: chrono::Duration,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(1_000),
            lease: chrono::Duration::seconds(60),
            outbox_max_attempts: 12,
            retry: RetryPolicy::default(),
            idempotency_ttl: chrono::Duration::hours(24),
        }
    }
}

pub struct NumberingConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub metrics_address: Option<SocketAddr>,
    pub workers: WorkerSettings,
    pub log_format: LogFormat,
}

impl fmt::Debug for NumberingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberingConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("metrics_address", &self.metrics_address)
            .field("workers", &self.workers)
            .field("log_format", &self.log_format)
            .finish()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8095";

fn bounded(
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

fn address(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Option<SocketAddr>, ConfigError> {
    get(var)
        .map(|v| {
            v.parse::<SocketAddr>().map_err(|_| {
                ConfigError::invalid(var, "must be a socket address such as 127.0.0.1:8095")
            })
        })
        .transpose()
}

impl NumberingConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Builds the configuration from an arbitrary key lookup (environment, tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |key: &str| get(key).filter(|v| !v.trim().is_empty());
        let database_url =
            get("DATABASE_URL")
                .map(SecretString::from)
                .ok_or(ConfigError::Missing {
                    var: "DATABASE_URL",
                })?;
        let bind_address = address(&get, "NUMBERING_BIND_ADDRESS")?
            .unwrap_or_else(|| DEFAULT_BIND_ADDRESS.parse().expect("valid default"));
        let metrics_address = address(&get, "NUMBERING_METRICS_ADDRESS")?;
        if metrics_address == Some(bind_address) {
            return Err(ConfigError::invalid(
                "NUMBERING_METRICS_ADDRESS",
                "must differ from NUMBERING_BIND_ADDRESS",
            ));
        }
        let database_max_connections =
            bounded(&get, "NUMBERING_DATABASE_MAX_CONNECTIONS", 10, 1, 500)? as u32;
        let defaults = WorkerSettings::default();
        let base_ms = bounded(&get, "NUMBERING_RETRY_BASE_MS", 1_000, 10, 600_000)?;
        let max_ms = bounded(&get, "NUMBERING_RETRY_MAX_MS", 300_000, 10, 86_400_000)?;
        if max_ms < base_ms {
            return Err(ConfigError::invalid(
                "NUMBERING_RETRY_MAX_MS",
                "must not be below NUMBERING_RETRY_BASE_MS",
            ));
        }
        let workers = WorkerSettings {
            interval: Duration::from_millis(bounded(
                &get,
                "NUMBERING_WORKER_INTERVAL_MS",
                1_000,
                100,
                60_000,
            )? as u64),
            lease: chrono::Duration::seconds(bounded(
                &get,
                "NUMBERING_WORKER_LEASE_SECONDS",
                defaults.lease.num_seconds(),
                5,
                3_600,
            )?),
            outbox_max_attempts: bounded(
                &get,
                "NUMBERING_OUTBOX_MAX_ATTEMPTS",
                defaults.outbox_max_attempts.into(),
                1,
                100,
            )? as i32,
            retry: RetryPolicy {
                base: chrono::Duration::milliseconds(base_ms),
                max: chrono::Duration::milliseconds(max_ms),
            },
            idempotency_ttl: chrono::Duration::hours(bounded(
                &get,
                "NUMBERING_IDEMPOTENCY_TTL_HOURS",
                24,
                1,
                720,
            )?),
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
        Ok(Self {
            database_url,
            database_max_connections,
            bind_address,
            metrics_address,
            workers,
            log_format,
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

    #[test]
    fn requires_its_own_database_and_applies_defaults() {
        assert!(matches!(
            NumberingConfig::from_lookup(lookup(&[])),
            Err(ConfigError::Missing {
                var: "DATABASE_URL"
            })
        ));
        let cfg = NumberingConfig::from_lookup(lookup(&[(
            "DATABASE_URL",
            "postgres://numbering:pw-123@db/n",
        )]))
        .unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert_eq!(cfg.workers, WorkerSettings::default());
        assert!(!format!("{cfg:?}").contains("pw-123"));
    }

    #[test]
    fn tunables_are_bounded_and_never_echo_values() {
        let base = [("DATABASE_URL", "postgres://x/y")];
        for (var, value) in [
            ("NUMBERING_WORKER_INTERVAL_MS", "5"),
            ("NUMBERING_WORKER_LEASE_SECONDS", "1"),
            ("NUMBERING_OUTBOX_MAX_ATTEMPTS", "0"),
            ("NUMBERING_OUTBOX_MAX_ATTEMPTS", "101"),
            ("NUMBERING_RETRY_BASE_MS", "soon"),
            ("NUMBERING_IDEMPOTENCY_TTL_HOURS", "9999"),
            ("NUMBERING_BIND_ADDRESS", "everywhere"),
            ("LOG_FORMAT", "xml"),
        ] {
            let mut pairs = base.to_vec();
            pairs.push((var, value));
            let err = NumberingConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value) || value.len() < 4, "{var}");
        }
        let mut inverted = base.to_vec();
        inverted.push(("NUMBERING_RETRY_BASE_MS", "5000"));
        inverted.push(("NUMBERING_RETRY_MAX_MS", "1000"));
        assert!(NumberingConfig::from_lookup(lookup(&inverted)).is_err());
        let mut same = base.to_vec();
        same.push(("NUMBERING_BIND_ADDRESS", "127.0.0.1:9000"));
        same.push(("NUMBERING_METRICS_ADDRESS", "127.0.0.1:9000"));
        assert!(NumberingConfig::from_lookup(lookup(&same)).is_err());
    }
}

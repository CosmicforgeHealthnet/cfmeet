//! Calling service configuration, read from the environment.
//!
//! Secrets (database URL credentials, signing keys) are held in redacted containers and
//! may be mounted as files (`<NAME>_FILE`, see [`secret`]). Calling holds NO credential
//! verifier material: customer credentials are verified by Platform Access (ADR 0032),
//! reached through `CF_PLATFORM_ACCESS_BASE_URL` (internal service authentication).

pub mod secret;

use std::{fmt, net::SocketAddr};

use secrecy::{ExposeSecret, SecretString};

use crate::platform::limits::LimitDefaults;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    /// The message never echoes the offending value (it may be a secret).
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

impl ConfigError {
    pub fn invalid(var: &'static str, reason: String) -> Self {
        ConfigError::Invalid { var, reason }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

pub struct CallingConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub limit_defaults: LimitDefaults,
    /// How often the identity projection reads the Platform Access change feed
    /// (`CALLING_IDENTITY_SYNC_INTERVAL_MS`, 100–60000, default 1000).
    pub identity_sync_interval: std::time::Duration,
    pub log_format: LogFormat,
}

impl fmt::Debug for CallingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallingConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("identity_sync_interval", &self.identity_sync_interval)
            .field("limit_defaults", &self.limit_defaults)
            .field("log_format", &self.log_format)
            .finish()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8090";

impl CallingConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Builds the configuration from an arbitrary key lookup (environment, tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let non_empty = |key: &str| get(key).filter(|v| !v.trim().is_empty());

        let database_url =
            non_empty("DATABASE_URL")
                .map(SecretString::from)
                .ok_or(ConfigError::Missing {
                    var: "DATABASE_URL",
                })?;

        let bind_address = non_empty("CALLING_BIND_ADDRESS")
            .unwrap_or_else(|| DEFAULT_BIND_ADDRESS.to_string())
            .parse::<SocketAddr>()
            .map_err(|_| {
                ConfigError::invalid(
                    "CALLING_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8090".to_string(),
                )
            })?;

        let identity_sync_interval = std::time::Duration::from_millis(
            u64::try_from(parse_number(
                &non_empty,
                "CALLING_IDENTITY_SYNC_INTERVAL_MS",
                1000,
                100,
            )?)
            .unwrap_or(1000)
            .min(60_000),
        );

        let platform_defaults = LimitDefaults::default();
        let limit_defaults = LimitDefaults {
            requests_per_minute: positive(
                &non_empty,
                "PLATFORM_DEFAULT_REQUESTS_PER_MINUTE",
                platform_defaults.requests_per_minute,
            )?,
            max_concurrent_communications: non_negative(
                &non_empty,
                "PLATFORM_DEFAULT_MAX_CONCURRENT_COMMUNICATIONS",
                platform_defaults.max_concurrent_communications,
            )?,
        };

        let log_format = match non_empty("LOG_FORMAT").as_deref() {
            None | Some("pretty") => LogFormat::Pretty,
            Some("json") => LogFormat::Json,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "LOG_FORMAT",
                    "must be `pretty` or `json`".to_string(),
                ))
            }
        };

        let database_max_connections =
            positive(&non_empty, "CALLING_DATABASE_MAX_CONNECTIONS", 10)?;

        Ok(Self {
            database_url,
            database_max_connections: u32::try_from(database_max_connections).map_err(|_| {
                ConfigError::invalid(
                    "CALLING_DATABASE_MAX_CONNECTIONS",
                    "is too large".to_string(),
                )
            })?,
            bind_address,
            limit_defaults,
            identity_sync_interval,
            log_format,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }
}

fn parse_number(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
    min: i64,
) -> Result<i64, ConfigError> {
    match get(var) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= min && *n <= i64::from(i32::MAX))
            .ok_or_else(|| {
                ConfigError::invalid(
                    var,
                    format!("must be an integer between {min} and {}", i32::MAX),
                )
            }),
    }
}

fn positive(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
) -> Result<i64, ConfigError> {
    parse_number(get, var, default, 1)
}

fn non_negative(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: i64,
) -> Result<i64, ConfigError> {
    parse_number(get, var, default, 0)
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
    fn requires_database_url_and_nothing_credential_related() {
        let err = CallingConfig::from_lookup(lookup(&[])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "DATABASE_URL"
            }
        ));
        // No verifier secret is needed (or read) by Calling any more.
        let cfg = CallingConfig::from_lookup(lookup(&[("DATABASE_URL", "postgres://x/y")])).unwrap();
        assert_eq!(cfg.identity_sync_interval, std::time::Duration::from_secs(1));
    }

    #[test]
    fn defaults_and_redacted_debug() {
        let cfg = CallingConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://user:dbpassword@host/db"),
            ("CALLING_IDENTITY_SYNC_INTERVAL_MS", "250"),
        ]))
        .unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert_eq!(cfg.log_format, LogFormat::Pretty);
        assert_eq!(cfg.identity_sync_interval, std::time::Duration::from_millis(250));
        assert!(!format!("{cfg:?}").contains("dbpassword"));
        assert!(CallingConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("CALLING_IDENTITY_SYNC_INTERVAL_MS", "5"),
        ]))
        .is_err());
    }
}

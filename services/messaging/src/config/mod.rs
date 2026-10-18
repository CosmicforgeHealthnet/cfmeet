//! Messaging configuration, read from the environment. Validation fails closed and error
//! messages never echo a value.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | Messaging's OWN PostgreSQL database (runtime role) |
//! | `MESSAGING_DATABASE_MAX_CONNECTIONS` | no | pool size (1–500, default 10) |
//! | `MESSAGING_BIND_ADDRESS` | no | default `127.0.0.1:8097` |
//! | `MESSAGING_METRICS_ADDRESS` | no | separate `GET /metrics` listener (e.g. `127.0.0.1:9197`) |
//! | `MESSAGING_E2EE_DEVICES_ENABLED` | no | `true` / `false`: device registration and KeyPackage publication. Default `true` only in `local` / `test` runtimes; staging / production require an explicit `true` (owner approval of the cryptographic decisions, ADR 0045) |
//! | `MESSAGING_DEVICE_CHALLENGE_TTL_SECONDS` | no | registration challenge validity (60–3600, default 300) |
//! | `MESSAGING_DEVICE_CHALLENGES_PER_HOUR` | no | challenges per principal per hour (1–1000, default 20) |
//! | `MESSAGING_MAX_ACTIVE_DEVICES` | no | active devices per principal (1–100, default 10) |
//! | `MESSAGING_MAX_KEY_PACKAGES_PER_DEVICE` | no | AVAILABLE KeyPackages per device (1–1000, default 100) |
//! | `MESSAGING_KEY_PACKAGE_MAX_LIFETIME_DAYS` | no | longest accepted KeyPackage lifetime (1–90, default 90) |
//! | `MESSAGING_DIRECTORY_LOOKUPS_PER_MINUTE` | no | device-directory / proof lookups per credential per minute (1–100000, default 600) |
//! | `MESSAGING_AUTH_CACHE_TTL_SECONDS` | no | positive authentication cache for READ routes (0–60, default 10; 0 disables). Mutations always re-authenticate |
//! | `MESSAGING_IDENTITY_FEED_INTERVAL_MS` | no | Platform Access change-feed poll (250–60000, default 2000); any change flushes the cache, a failed poll disables it |
//! | `MESSAGING_CONVERSATIONS_PER_MINUTE` | no | conversation creations per acting principal per minute (1–100000, default 120) |
//! | `MESSAGING_EXPIRY_INTERVAL_MS` | no | conversation expiry worker interval (250–600000, default 5000) |
//! | `MESSAGING_WORKER_INTERVAL_MS` / `MESSAGING_WORKER_LEASE_SECONDS` / `MESSAGING_OUTBOX_MAX_ATTEMPTS` | no | outbox publisher (defaults 1000 / 60 / 12) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//!
//! Service authentication (`CF_*`) is read by [`service_auth`].

pub mod service_auth;

use std::{fmt, net::SocketAddr, time::Duration};

use cf_service_auth::RuntimeEnvironment;
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

/// Device / key-directory policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSettings {
    pub enabled: bool,
    pub challenge_ttl: chrono::Duration,
    pub challenges_per_hour: i64,
    pub max_active_devices: i64,
    pub max_key_packages_per_device: i64,
    pub key_package_max_lifetime_days: u64,
    pub directory_lookups_per_minute: i64,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            challenge_ttl: chrono::Duration::seconds(300),
            challenges_per_hour: 20,
            max_active_devices: 10,
            max_key_packages_per_device: 100,
            key_package_max_lifetime_days: 90,
            directory_lookups_per_minute: 600,
        }
    }
}

impl DeviceSettings {
    /// The KeyPackage lifetime policy derived from these settings.
    pub fn lifetime_policy(&self) -> cf_messaging_crypto::key_package::LifetimePolicy {
        cf_messaging_crypto::key_package::LifetimePolicy {
            max_total_seconds: self.key_package_max_lifetime_days * 86_400 + 3_600,
            ..Default::default()
        }
    }
}

/// Authorization caching (Part V B.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthCacheSettings {
    pub ttl: Duration,
    pub feed_interval: Duration,
}

impl Default for AuthCacheSettings {
    fn default() -> Self {
        Self {
            ttl: Duration::ZERO,
            feed_interval: Duration::from_millis(2_000),
        }
    }
}

/// Conversation-domain limits and workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationSettings {
    pub creations_per_minute: i64,
    pub expiry_interval: Duration,
}

impl Default for ConversationSettings {
    fn default() -> Self {
        Self {
            creations_per_minute: 120,
            expiry_interval: Duration::from_millis(5_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSettings {
    pub interval: Duration,
    pub lease: chrono::Duration,
    pub outbox_max_attempts: i32,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(1_000),
            lease: chrono::Duration::seconds(60),
            outbox_max_attempts: 12,
        }
    }
}

pub struct MessagingConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub metrics_address: Option<SocketAddr>,
    pub devices: DeviceSettings,
    pub auth_cache: AuthCacheSettings,
    pub conversations: ConversationSettings,
    pub workers: WorkerSettings,
    pub log_format: LogFormat,
}

impl fmt::Debug for MessagingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessagingConfig")
            .field("database_url", &"[REDACTED]")
            .field("bind_address", &self.bind_address)
            .field("metrics_address", &self.metrics_address)
            .field("devices", &self.devices)
            .field("auth_cache", &self.auth_cache)
            .field("workers", &self.workers)
            .finish_non_exhaustive()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8097";

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
                ConfigError::invalid(var, "must be a socket address such as 127.0.0.1:8097")
            })
        })
        .transpose()
}

impl MessagingConfig {
    pub fn from_env(runtime: RuntimeEnvironment) -> Result<Self, ConfigError> {
        Self::from_lookup(runtime, |key| std::env::var(key).ok())
    }

    /// Builds the configuration for `runtime` from an arbitrary key lookup.
    pub fn from_lookup(
        runtime: RuntimeEnvironment,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
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
        let bind_address = address(&get, "MESSAGING_BIND_ADDRESS")?
            .unwrap_or_else(|| DEFAULT_BIND_ADDRESS.parse().expect("valid default"));
        let metrics_address = address(&get, "MESSAGING_METRICS_ADDRESS")?;
        if metrics_address == Some(bind_address) {
            return Err(ConfigError::invalid(
                "MESSAGING_METRICS_ADDRESS",
                "must differ from MESSAGING_BIND_ADDRESS",
            ));
        }
        let enabled = match get("MESSAGING_E2EE_DEVICES_ENABLED").as_deref() {
            None => runtime.allows_insecure_transport(),
            Some("true") => true,
            Some("false") => false,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "MESSAGING_E2EE_DEVICES_ENABLED",
                    "must be `true` or `false`",
                ))
            }
        };
        let devices = DeviceSettings {
            enabled,
            challenge_ttl: chrono::Duration::seconds(bounded(
                &get,
                "MESSAGING_DEVICE_CHALLENGE_TTL_SECONDS",
                300,
                60,
                3_600,
            )?),
            challenges_per_hour: bounded(
                &get,
                "MESSAGING_DEVICE_CHALLENGES_PER_HOUR",
                20,
                1,
                1_000,
            )?,
            max_active_devices: bounded(&get, "MESSAGING_MAX_ACTIVE_DEVICES", 10, 1, 100)?,
            max_key_packages_per_device: bounded(
                &get,
                "MESSAGING_MAX_KEY_PACKAGES_PER_DEVICE",
                100,
                1,
                1_000,
            )?,
            key_package_max_lifetime_days: bounded(
                &get,
                "MESSAGING_KEY_PACKAGE_MAX_LIFETIME_DAYS",
                90,
                1,
                90,
            )? as u64,
            directory_lookups_per_minute: bounded(
                &get,
                "MESSAGING_DIRECTORY_LOOKUPS_PER_MINUTE",
                600,
                1,
                100_000,
            )?,
        };
        let auth_cache = AuthCacheSettings {
            ttl: Duration::from_secs(
                bounded(&get, "MESSAGING_AUTH_CACHE_TTL_SECONDS", 10, 0, 60)? as u64,
            ),
            feed_interval: Duration::from_millis(bounded(
                &get,
                "MESSAGING_IDENTITY_FEED_INTERVAL_MS",
                2_000,
                250,
                60_000,
            )? as u64),
        };
        let conversations = ConversationSettings {
            creations_per_minute: bounded(
                &get,
                "MESSAGING_CONVERSATIONS_PER_MINUTE",
                120,
                1,
                100_000,
            )?,
            expiry_interval: Duration::from_millis(bounded(
                &get,
                "MESSAGING_EXPIRY_INTERVAL_MS",
                5_000,
                250,
                600_000,
            )? as u64),
        };
        let workers = WorkerSettings {
            interval: Duration::from_millis(bounded(
                &get,
                "MESSAGING_WORKER_INTERVAL_MS",
                1_000,
                100,
                60_000,
            )? as u64),
            lease: chrono::Duration::seconds(bounded(
                &get,
                "MESSAGING_WORKER_LEASE_SECONDS",
                60,
                5,
                3_600,
            )?),
            outbox_max_attempts: bounded(&get, "MESSAGING_OUTBOX_MAX_ATTEMPTS", 12, 1, 100)? as i32,
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
            database_max_connections: bounded(
                &get,
                "MESSAGING_DATABASE_MAX_CONNECTIONS",
                10,
                1,
                500,
            )? as u32,
            bind_address,
            metrics_address,
            devices,
            auth_cache,
            conversations,
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

    const BASE: &[(&str, &str)] = &[("DATABASE_URL", "postgres://messaging:pw-xyz-123@db/m")];

    #[test]
    fn requires_its_own_database_and_applies_safe_defaults() {
        let cfg = MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(BASE)).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert!(cfg.devices.enabled);
        assert!(!format!("{cfg:?}").contains("pw-xyz-123"));
        assert!(MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(&[])).is_err());
    }

    #[test]
    fn e2ee_device_operations_need_an_explicit_decision_in_staging_and_production() {
        for env in [RuntimeEnvironment::Staging, RuntimeEnvironment::Production] {
            let cfg = MessagingConfig::from_lookup(env, lookup(BASE)).unwrap();
            assert!(!cfg.devices.enabled, "{env}");
            let mut on = BASE.to_vec();
            on.push(("MESSAGING_E2EE_DEVICES_ENABLED", "true"));
            assert!(
                MessagingConfig::from_lookup(env, lookup(&on))
                    .unwrap()
                    .devices
                    .enabled
            );
        }
    }

    #[test]
    fn bad_values_are_refused_without_echo() {
        for (var, value) in [
            ("MESSAGING_E2EE_DEVICES_ENABLED", "yes-please"),
            ("MESSAGING_MAX_ACTIVE_DEVICES", "-7777"),
            ("MESSAGING_KEY_PACKAGE_MAX_LIFETIME_DAYS", "365"),
            ("MESSAGING_AUTH_CACHE_TTL_SECONDS", "3600"),
            ("MESSAGING_BIND_ADDRESS", "everywhere-port"),
            ("LOG_FORMAT", "xml-format"),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            let err =
                MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
        let mut same = BASE.to_vec();
        same.push(("MESSAGING_METRICS_ADDRESS", DEFAULT_BIND_ADDRESS));
        assert!(MessagingConfig::from_lookup(RuntimeEnvironment::Test, lookup(&same)).is_err());
    }
}

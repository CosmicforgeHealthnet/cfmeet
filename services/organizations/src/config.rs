//! Organizations Service configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | the Organizations Service's OWN PostgreSQL database |
//! | `ORGANIZATIONS_DATABASE_MAX_CONNECTIONS` | no | pool size (1–200, default 10) |
//! | `ORGANIZATIONS_BIND_ADDRESS` | no | default `127.0.0.1:8101` |
//! | `ORGANIZATIONS_WORKER_INTERVAL_MS` | no | maintenance loop interval (1000–600000, default 30000) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.organizations` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | the receiver-authoritative trust bundle of `cfmeet.organizations` |
//!
//! Organizations calls no other service yet (its outbox has no delivery target until
//! Sections 7–14), so it holds no signing key; inline keys are refused regardless.
//! Error messages never echo a value.

use std::{fmt, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    services, RuntimeEnvironment, ServiceId, ServiceTokenVerifier, SharedClock, TrustPolicy,
};
use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    /// The message never echoes the offending value (it may be a secret).
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var,
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8101";

pub fn organizations_service_id() -> ServiceId {
    ServiceId::parse(services::ORGANIZATIONS).expect("canonical identity")
}

pub struct OrganizationsConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub worker_interval: Duration,
    pub log_format: LogFormat,
    pub runtime_environment: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
}

impl fmt::Debug for OrganizationsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrganizationsConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("worker_interval", &self.worker_interval)
            .field("runtime_environment", &self.runtime_environment)
            .finish_non_exhaustive()
    }
}

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
            .ok_or_else(|| invalid(var, format!("must be an integer between {min} and {max}"))),
    }
}

impl OrganizationsConfig {
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
        let bind_address = match get("ORGANIZATIONS_BIND_ADDRESS") {
            None => DEFAULT_BIND_ADDRESS.parse().expect("valid default"),
            Some(v) => v.parse::<SocketAddr>().map_err(|_| {
                invalid(
                    "ORGANIZATIONS_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8101",
                )
            })?,
        };
        let database_max_connections =
            bounded(&get, "ORGANIZATIONS_DATABASE_MAX_CONNECTIONS", 10, 1, 200)? as u32;
        let worker_interval = Duration::from_millis(bounded(
            &get,
            "ORGANIZATIONS_WORKER_INTERVAL_MS",
            30_000,
            1_000,
            600_000,
        )? as u64);
        let log_format = match get("LOG_FORMAT").as_deref() {
            None | Some("pretty") => LogFormat::Pretty,
            Some("json") => LogFormat::Json,
            Some(_) => return Err(invalid("LOG_FORMAT", "must be `pretty` or `json`")),
        };
        if get("CF_SERVICE_AUTH_PRIVATE_KEY").is_some() {
            return Err(invalid(
                "CF_SERVICE_AUTH_PRIVATE_KEY",
                "inline private keys are not accepted",
            ));
        }
        if let Some(id) = get("CF_SERVICE_ID") {
            if id != services::ORGANIZATIONS {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.organizations"));
            }
        }
        let runtime_environment = RuntimeEnvironment::parse(&get("CF_RUNTIME_ENVIRONMENT").ok_or(
            ConfigError::Missing {
                var: "CF_RUNTIME_ENVIRONMENT",
            },
        )?)
        .map_err(|_| {
            invalid(
                "CF_RUNTIME_ENVIRONMENT",
                "must be local, test, staging or production",
            )
        })?;
        let trust_bundle_file = PathBuf::from(get("CF_SERVICE_TRUST_BUNDLE_FILE").ok_or(
            ConfigError::Missing {
                var: "CF_SERVICE_TRUST_BUNDLE_FILE",
            },
        )?);
        Ok(Self {
            database_url,
            database_max_connections,
            bind_address,
            worker_interval,
            log_format,
            runtime_environment,
            trust_bundle_file,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }

    /// Loads the trust bundle and builds the receiver-authoritative verifier; fails closed
    /// on anything malformed (wrong receiver, grants the registry forbids, …).
    pub fn verifier(&self, clock: SharedClock) -> Result<Arc<ServiceTokenVerifier>, ConfigError> {
        let policy = TrustPolicy::from_file(
            &self.trust_bundle_file,
            &organizations_service_id(),
            self.runtime_environment,
        )
        .map_err(|e| invalid("CF_SERVICE_TRUST_BUNDLE_FILE", e.to_string()))?;
        Ok(Arc::new(ServiceTokenVerifier::new(policy, clock)))
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

    const BASE: &[(&str, &str)] = &[
        (
            "DATABASE_URL",
            "postgres://org:pw-secret-9@db/organizations",
        ),
        ("CF_RUNTIME_ENVIRONMENT", "staging"),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", "/etc/cfmeet/trust.json"),
    ];

    #[test]
    fn requires_its_own_database_and_trust_bundle() {
        let cfg = OrganizationsConfig::from_lookup(lookup(BASE)).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert!(!format!("{cfg:?}").contains("pw-secret-9"));
        for missing in BASE.iter().map(|(k, _)| *k) {
            let pairs: Vec<_> = BASE
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            assert!(
                OrganizationsConfig::from_lookup(lookup(&pairs)).is_err(),
                "{missing}"
            );
        }
    }

    #[test]
    fn invalid_values_are_refused_without_echo() {
        for (var, value) in [
            ("ORGANIZATIONS_BIND_ADDRESS", "not-an-address-xyz"),
            ("ORGANIZATIONS_DATABASE_MAX_CONNECTIONS", "999"),
            ("ORGANIZATIONS_WORKER_INTERVAL_MS", "5"),
            ("LOG_FORMAT", "xml-format"),
            ("CF_SERVICE_ID", "cfmeet.calling-x"),
            ("CF_SERVICE_AUTH_PRIVATE_KEY", "inline-key-material"),
            ("CF_RUNTIME_ENVIRONMENT", "prod-ish"),
        ] {
            let mut pairs: Vec<_> = BASE.iter().filter(|(k, _)| *k != var).copied().collect();
            pairs.push((var, value));
            let err = OrganizationsConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
    }
}

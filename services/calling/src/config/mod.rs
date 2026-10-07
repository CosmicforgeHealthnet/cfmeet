//! Calling service configuration, read from the environment.
//!
//! Secrets (database URL credentials, the credential pepper) are held in redacted
//! containers. There is no fallback secret in any environment: a missing or weak
//! `API_CREDENTIAL_PEPPER` stops the service and the admin CLI from starting.

use std::{fmt, net::SocketAddr};

use chrono::Duration;
use secrecy::{ExposeSecret, SecretString};

use crate::platform::{
    limits::LimitDefaults,
    verifier::{CredentialPepper, VerifierKeyring},
};

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

/// Credential lifetime / rotation policy applied by issuance and rotation.
#[derive(Debug, Clone, Copy)]
pub struct CredentialPolicy {
    pub default_lifetime: Duration,
    pub max_lifetime: Duration,
    pub default_rotation_overlap: Duration,
    pub max_rotation_overlap: Duration,
}

impl Default for CredentialPolicy {
    fn default() -> Self {
        Self {
            default_lifetime: Duration::days(365),
            max_lifetime: Duration::days(730),
            default_rotation_overlap: Duration::hours(24),
            max_rotation_overlap: Duration::days(30),
        }
    }
}

pub struct CallingConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub keyring: VerifierKeyring,
    pub credential_policy: CredentialPolicy,
    pub limit_defaults: LimitDefaults,
    pub log_format: LogFormat,
}

impl fmt::Debug for CallingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallingConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("keyring", &self.keyring)
            .field("credential_policy", &self.credential_policy)
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

        let keyring = keyring_from(&non_empty)?;

        let defaults = CredentialPolicy::default();
        let credential_policy = CredentialPolicy {
            default_lifetime: Duration::days(positive(
                &non_empty,
                "CREDENTIAL_DEFAULT_LIFETIME_DAYS",
                defaults.default_lifetime.num_days(),
            )?),
            max_lifetime: Duration::days(positive(
                &non_empty,
                "CREDENTIAL_MAX_LIFETIME_DAYS",
                defaults.max_lifetime.num_days(),
            )?),
            default_rotation_overlap: Duration::minutes(non_negative(
                &non_empty,
                "CREDENTIAL_ROTATION_OVERLAP_MINUTES",
                defaults.default_rotation_overlap.num_minutes(),
            )?),
            max_rotation_overlap: Duration::minutes(non_negative(
                &non_empty,
                "CREDENTIAL_MAX_ROTATION_OVERLAP_MINUTES",
                defaults.max_rotation_overlap.num_minutes(),
            )?),
        };
        if credential_policy.default_lifetime > credential_policy.max_lifetime {
            return Err(ConfigError::invalid(
                "CREDENTIAL_DEFAULT_LIFETIME_DAYS",
                "must not exceed CREDENTIAL_MAX_LIFETIME_DAYS".to_string(),
            ));
        }
        if credential_policy.default_rotation_overlap > credential_policy.max_rotation_overlap {
            return Err(ConfigError::invalid(
                "CREDENTIAL_ROTATION_OVERLAP_MINUTES",
                "must not exceed CREDENTIAL_MAX_ROTATION_OVERLAP_MINUTES".to_string(),
            ));
        }

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
            keyring,
            credential_policy,
            limit_defaults,
            log_format,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }
}

/// Builds only the verifier keyring (admin CLI and server share this).
pub fn keyring_from(get: &impl Fn(&str) -> Option<String>) -> Result<VerifierKeyring, ConfigError> {
    let pepper = get("API_CREDENTIAL_PEPPER").ok_or(ConfigError::Missing {
        var: "API_CREDENTIAL_PEPPER",
    })?;
    let current = CredentialPepper::parse("API_CREDENTIAL_PEPPER", &pepper)?;
    let current_version = version(get, "API_CREDENTIAL_PEPPER_VERSION")?.unwrap_or(1);
    let previous = match get("API_CREDENTIAL_PREVIOUS_PEPPER") {
        None => None,
        Some(raw) => {
            let pepper = CredentialPepper::parse("API_CREDENTIAL_PREVIOUS_PEPPER", &raw)?;
            let version = version(get, "API_CREDENTIAL_PREVIOUS_PEPPER_VERSION")?.ok_or(
                ConfigError::Missing {
                    var: "API_CREDENTIAL_PREVIOUS_PEPPER_VERSION",
                },
            )?;
            Some((version, pepper))
        }
    };
    VerifierKeyring::new(current_version, current, previous)
}

fn version(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Option<i32>, ConfigError> {
    get(var)
        .map(|v| {
            v.trim()
                .parse::<i32>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| ConfigError::invalid(var, "must be a positive integer".to_string()))
        })
        .transpose()
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

    const PEPPER: &str = "config-test-pepper-0123456789abcdefghijklmnopqrstuvwxyz";

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn requires_database_url_and_pepper_without_fallback() {
        let err =
            CallingConfig::from_lookup(lookup(&[("API_CREDENTIAL_PEPPER", PEPPER)])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "DATABASE_URL"
            }
        ));
        let err =
            CallingConfig::from_lookup(lookup(&[("DATABASE_URL", "postgres://x/y")])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "API_CREDENTIAL_PEPPER"
            }
        ));
    }

    #[test]
    fn rejects_placeholder_pepper() {
        for weak in [
            "changeme",
            "secret123",
            "development-secret",
            "your-secret-key-change-this-in-production",
        ] {
            let err = CallingConfig::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x/y"),
                ("API_CREDENTIAL_PEPPER", weak),
            ]))
            .unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { .. }), "{weak}");
            assert!(!err.to_string().contains(weak), "error echoes the value");
        }
    }

    #[test]
    fn defaults_and_redacted_debug() {
        let cfg = CallingConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://user:dbpassword@host/db"),
            ("API_CREDENTIAL_PEPPER", PEPPER),
        ]))
        .unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert_eq!(cfg.keyring.current_version(), 1);
        assert_eq!(cfg.log_format, LogFormat::Pretty);
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("dbpassword"));
        assert!(!dbg.contains("config-test-pepper"));
    }

    #[test]
    fn previous_pepper_requires_its_version() {
        let err = CallingConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("API_CREDENTIAL_PEPPER", PEPPER),
            ("API_CREDENTIAL_PEPPER_VERSION", "2"),
            (
                "API_CREDENTIAL_PREVIOUS_PEPPER",
                "previous-pepper-zyxwvutsrqponmlkjihgfedcba9876543210",
            ),
        ]))
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "API_CREDENTIAL_PREVIOUS_PEPPER_VERSION"
            }
        ));
    }

    #[test]
    fn rejects_inconsistent_credential_policy() {
        let err = CallingConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("API_CREDENTIAL_PEPPER", PEPPER),
            ("CREDENTIAL_DEFAULT_LIFETIME_DAYS", "900"),
        ]))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }
}

//! Platform Access configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | Platform Access's OWN PostgreSQL database |
//! | `PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS` | no | pool size (default 10) |
//! | `PLATFORM_ACCESS_BIND_ADDRESS` | no | default `127.0.0.1:8094` (internal API only) |
//! | `PLATFORM_ACCESS_METRICS_ADDRESS` | no | separate `GET /metrics` listener |
//! | `API_CREDENTIAL_PEPPER` / `_FILE` (+ `_VERSION`, `API_CREDENTIAL_PREVIOUS_PEPPER*`) | yes | the credential verifier keyring (the same pepper Calling held in Part III — moved, not duplicated) |
//! | `CREDENTIAL_*` | no | credential lifetime / rotation policy |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//!
//! Service authentication (`CF_*`) is read by [`service_auth`].
//!
//! Secrets (database URL credentials, the credential pepper) are held in redacted
//! containers. There is no fallback secret in any environment: a missing or weak
//! `API_CREDENTIAL_PEPPER` stops the service and the admin CLI from starting. Every
//! secret may be mounted as a file instead (`<NAME>_FILE`, see [`secret`]).

pub mod secret;
pub mod service_auth;

use std::{fmt, net::SocketAddr};

use chrono::Duration;
use secrecy::{ExposeSecret, SecretString};

use crate::identity::verifier::{CredentialPepper, VerifierKeyring};

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

pub struct PlatformAccessConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub metrics_address: Option<SocketAddr>,
    pub keyring: VerifierKeyring,
    pub credential_policy: CredentialPolicy,
    pub log_format: LogFormat,
}

impl fmt::Debug for PlatformAccessConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlatformAccessConfig")
            .field("database_url", &"[REDACTED]")
            .field("database_max_connections", &self.database_max_connections)
            .field("bind_address", &self.bind_address)
            .field("metrics_address", &self.metrics_address)
            .field("keyring", &self.keyring)
            .field("credential_policy", &self.credential_policy)
            .field("log_format", &self.log_format)
            .finish()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8094";

impl PlatformAccessConfig {
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

        let bind_address = non_empty("PLATFORM_ACCESS_BIND_ADDRESS")
            .unwrap_or_else(|| DEFAULT_BIND_ADDRESS.to_string())
            .parse::<SocketAddr>()
            .map_err(|_| {
                ConfigError::invalid(
                    "PLATFORM_ACCESS_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8094".to_string(),
                )
            })?;
        let metrics_address = non_empty("PLATFORM_ACCESS_METRICS_ADDRESS")
            .map(|v| v.parse::<SocketAddr>())
            .transpose()
            .map_err(|_| {
                ConfigError::invalid(
                    "PLATFORM_ACCESS_METRICS_ADDRESS",
                    "must be a socket address".to_string(),
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
            positive(&non_empty, "PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", 10)?;

        Ok(Self {
            database_url,
            database_max_connections: u32::try_from(database_max_connections).map_err(|_| {
                ConfigError::invalid(
                    "PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS",
                    "is too large".to_string(),
                )
            })?,
            bind_address,
            metrics_address,
            keyring,
            credential_policy,
            log_format,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }
}

/// Builds only the verifier keyring (admin CLI and server share this).
pub fn keyring_from(get: &impl Fn(&str) -> Option<String>) -> Result<VerifierKeyring, ConfigError> {
    let pepper =
        secret::required_secret(get, "API_CREDENTIAL_PEPPER", "API_CREDENTIAL_PEPPER_FILE")?;
    let current = CredentialPepper::parse("API_CREDENTIAL_PEPPER", pepper.expose_secret())?;
    let current_version = version(get, "API_CREDENTIAL_PEPPER_VERSION")?.unwrap_or(1);
    let previous = match secret::read_secret(
        get,
        "API_CREDENTIAL_PREVIOUS_PEPPER",
        "API_CREDENTIAL_PREVIOUS_PEPPER_FILE",
    )? {
        None => None,
        Some(raw) => {
            let pepper =
                CredentialPepper::parse("API_CREDENTIAL_PREVIOUS_PEPPER", raw.expose_secret())?;
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
            PlatformAccessConfig::from_lookup(lookup(&[("API_CREDENTIAL_PEPPER", PEPPER)])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "DATABASE_URL"
            }
        ));
        let err =
            PlatformAccessConfig::from_lookup(lookup(&[("DATABASE_URL", "postgres://x/y")])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "API_CREDENTIAL_PEPPER"
            }
        ));
    }

    /// No fallback pepper exists in any runtime environment (§74.11): a production
    /// deployment without `API_CREDENTIAL_PEPPER` / `_FILE` refuses to start.
    #[test]
    fn production_without_a_pepper_refuses_to_start() {
        for runtime in ["production", "staging", "local", "test"] {
            let err = PlatformAccessConfig::from_lookup(lookup(&[
                ("DATABASE_URL", "postgres://x/y"),
                ("CF_RUNTIME_ENVIRONMENT", runtime),
            ]))
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    ConfigError::Missing {
                        var: "API_CREDENTIAL_PEPPER"
                    }
                ),
                "{runtime}: {err}"
            );
        }
        // The CLI / server share keyring_from: same refusal.
        assert!(keyring_from(&lookup(&[("CF_RUNTIME_ENVIRONMENT", "production")])).is_err());
    }

    #[test]
    fn the_pepper_can_be_a_secret_file_and_is_never_ambiguous() {
        use super::secret::tests::TempSecret;
        let file = TempSecret::new(&format!("{PEPPER}\n"), 0o600);
        let path = file.path();
        let cfg = PlatformAccessConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("CF_RUNTIME_ENVIRONMENT", "production"),
            ("API_CREDENTIAL_PEPPER_FILE", path.as_str()),
        ]))
        .unwrap();
        assert_eq!(cfg.keyring.current_version(), 1);
        // Both forms set: refused (no silent preference).
        let err = PlatformAccessConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("API_CREDENTIAL_PEPPER", PEPPER),
            ("API_CREDENTIAL_PEPPER_FILE", path.as_str()),
        ]))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
        // A missing file is an error, never the inline value or a default.
        let err = PlatformAccessConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("API_CREDENTIAL_PEPPER_FILE", "/nonexistent/platform-access/pepper"),
        ]))
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "API_CREDENTIAL_PEPPER_FILE",
                ..
            }
        ));
        // A placeholder in a file is as weak as inline.
        let weak = TempSecret::new("changeme", 0o600);
        let weak_path = weak.path();
        assert!(PlatformAccessConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("API_CREDENTIAL_PEPPER_FILE", weak_path.as_str()),
        ]))
        .is_err());
        // The previous pepper accepts the file form too.
        let previous = TempSecret::new(
            "previous-pepper-zyxwvutsrqponmlkjihgfedcba9876543210",
            0o600,
        );
        let previous_path = previous.path();
        let cfg = PlatformAccessConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("API_CREDENTIAL_PEPPER", PEPPER),
            ("API_CREDENTIAL_PEPPER_VERSION", "2"),
            (
                "API_CREDENTIAL_PREVIOUS_PEPPER_FILE",
                previous_path.as_str(),
            ),
            ("API_CREDENTIAL_PREVIOUS_PEPPER_VERSION", "1"),
        ]))
        .unwrap();
        assert_eq!(cfg.keyring.current_version(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_pepper_file_is_refused_in_production() {
        use super::secret::tests::TempSecret;
        let file = TempSecret::new(PEPPER, 0o644);
        let path = file.path();
        let err = PlatformAccessConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("CF_RUNTIME_ENVIRONMENT", "production"),
            ("API_CREDENTIAL_PEPPER_FILE", path.as_str()),
        ]))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
        assert!(!err.to_string().contains(PEPPER));
    }

    #[test]
    fn rejects_placeholder_pepper() {
        for weak in [
            "changeme",
            "secret123",
            "development-secret",
            "your-secret-key-change-this-in-production",
        ] {
            let err = PlatformAccessConfig::from_lookup(lookup(&[
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
        let cfg = PlatformAccessConfig::from_lookup(lookup(&[
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
        let err = PlatformAccessConfig::from_lookup(lookup(&[
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
        let err = PlatformAccessConfig::from_lookup(lookup(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("API_CREDENTIAL_PEPPER", PEPPER),
            ("CREDENTIAL_DEFAULT_LIFETIME_DAYS", "900"),
        ]))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }
}

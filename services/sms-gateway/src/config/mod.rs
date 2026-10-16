//! SMS Gateway configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | the SMS Gateway's OWN PostgreSQL database |
//! | `SMS_GATEWAY_DATABASE_MAX_CONNECTIONS` | no | pool size (default 10) |
//! | `SMS_GATEWAY_BIND_ADDRESS` | no | default `127.0.0.1:8096` |
//! | `SMS_GATEWAY_METRICS_ADDRESS` | no | separate `GET /metrics` listener (e.g. `127.0.0.1:9196`) |
//! | `SMS_GATEWAY_WORKER_INTERVAL_MS` | no | worker poll interval (100–60000, default 1000) |
//! | `SMS_GATEWAY_WORKER_LEASE_SECONDS` | no | lease of a claimed attempt / outbox event (5–3600, default 60) |
//! | `SMS_GATEWAY_OUTBOX_MAX_ATTEMPTS` | no | publication attempts before DEAD_LETTER (1–100, default 12) |
//! | `SMS_GATEWAY_RETRY_BASE_MS` / `SMS_GATEWAY_RETRY_MAX_MS` | no | backoff bounds (default 1000 / 300000) |
//! | `SMS_GATEWAY_CONTENT_KEY_ID` / `SMS_GATEWAY_CONTENT_KEY_FILE` | yes | AEAD key (base64, 32 bytes) sealing content and numbers at rest |
//! | `SMS_GATEWAY_PREVIOUS_CONTENT_KEY_ID` / `SMS_GATEWAY_PREVIOUS_CONTENT_KEY_FILE` | no | rotation overlap |
//! | `SMS_GATEWAY_FINGERPRINT_KEY_FILE` | yes | HMAC key (base64, ≥ 32 bytes) of remote-number / content fingerprints |
//! | `SMS_GATEWAY_CONTENT_RETENTION_DAYS` | no | sealed content kept (1–3650, default 30) |
//! | `SMS_GATEWAY_QUARANTINE_RETENTION_DAYS` | no | unattributable inbound kept (1–90, default 7) |
//! | `SMS_GATEWAY_PROVIDER` | no | `deterministic` (local / test only); unset: no provider, sends are refused |
//! | `SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE` | with a provider | HMAC secret of the provider's callbacks |
//! | `SMS_GATEWAY_SUBMIT_TIMEOUT_MS` | no | provider submission timeout (100–60000, default 10000) |
//! | `SMS_GATEWAY_MAX_SUBMIT_ATTEMPTS` | no | definitely-unsent attempts before FAILED (1–10, default 3) |
//! | `SMS_GATEWAY_VALIDITY_SECONDS` | no | default message validity (60–86400, default 3600) |
//! | `SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES` | no | comma-separated E.164 prefixes (`+1,+44`); empty: no destination allowed |
//! | `SMS_GATEWAY_RATE_PER_MINUTE` | no | outbound messages per environment per minute (1–100000, default 60) |
//! | `SMS_GATEWAY_DESTINATION_RATE_PER_HOUR` | no | per recipient per hour (1–10000, default 10) |
//! | `SMS_GATEWAY_SEGMENT_COST_MINOR` / `SMS_GATEWAY_COST_CURRENCY` | no | estimate per segment (default 5 USD minor units) |
//! | `SMS_GATEWAY_DAILY_SPEND_LIMIT_MINOR` | no | estimated spend per environment per day (default 10000) |
//! | `SMS_GATEWAY_ABUSE_IDENTICAL_RECIPIENTS` | no | distinct recipients of identical content per hour (2–100000, default 20) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//!
//! Service authentication (`CF_*`) is read by [`service_auth`]. Keys are read from FILES
//! only (no inline key material); error messages never echo a value.

pub mod service_auth;

use std::{fmt, net::SocketAddr, path::PathBuf, time::Duration};

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
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(1_000),
            lease: chrono::Duration::seconds(60),
            outbox_max_attempts: 12,
            retry: RetryPolicy::default(),
        }
    }
}

/// Where the protection keys live (files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionFiles {
    pub content_key_id: String,
    pub content_key_file: PathBuf,
    pub previous: Option<(String, PathBuf)>,
    pub fingerprint_key_file: PathBuf,
}

/// The provider choice of this deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderChoice {
    /// No provider: sending is refused (PROVIDER_UNAVAILABLE); inbound callbacks 404.
    None,
    /// The deterministic test provider (refused in staging / production).
    Deterministic { callback_secret_file: PathBuf },
}

/// Outbound safety controls (reason codes in [`crate::sms::controls`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlSettings {
    pub allowed_prefixes: Vec<String>,
    pub rate_per_minute: i64,
    pub destination_rate_per_hour: i64,
    pub segment_cost_minor: i64,
    pub currency: String,
    pub daily_spend_limit_minor: i64,
    pub abuse_identical_recipients: i64,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            allowed_prefixes: Vec::new(),
            rate_per_minute: 60,
            destination_rate_per_hour: 10,
            segment_cost_minor: 5,
            currency: "USD".into(),
            daily_spend_limit_minor: 10_000,
            abuse_identical_recipients: 20,
        }
    }
}

/// Transport tunables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportSettings {
    pub submit_timeout: Duration,
    pub max_submit_attempts: i16,
    pub default_validity: chrono::Duration,
    pub content_retention: chrono::Duration,
    pub quarantine_retention: chrono::Duration,
}

impl Default for TransportSettings {
    fn default() -> Self {
        Self {
            submit_timeout: Duration::from_millis(10_000),
            max_submit_attempts: 3,
            default_validity: chrono::Duration::hours(1),
            content_retention: chrono::Duration::days(30),
            quarantine_retention: chrono::Duration::days(7),
        }
    }
}

pub struct SmsGatewayConfig {
    pub database_url: SecretString,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub metrics_address: Option<SocketAddr>,
    pub workers: WorkerSettings,
    pub protection: ProtectionFiles,
    pub provider: ProviderChoice,
    pub transport: TransportSettings,
    pub controls: ControlSettings,
    pub log_format: LogFormat,
}

impl fmt::Debug for SmsGatewayConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmsGatewayConfig")
            .field("database_url", &"[REDACTED]")
            .field("bind_address", &self.bind_address)
            .field("metrics_address", &self.metrics_address)
            .field("workers", &self.workers)
            .field("provider", &self.provider)
            .field("transport", &self.transport)
            .field("controls", &self.controls)
            .finish_non_exhaustive()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8096";

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
                ConfigError::invalid(var, "must be a socket address such as 127.0.0.1:8096")
            })
        })
        .transpose()
}

/// A key id: `[A-Za-z0-9][A-Za-z0-9_-]{0,31}` (the sealing format's key id).
fn key_id(value: &str) -> bool {
    cf_webhooks::secret_box::valid_key_id(value)
}

/// An E.164 prefix: `+` and 1–6 digits, no leading zero.
fn prefix(value: &str) -> bool {
    value.strip_prefix('+').is_some_and(|d| {
        (1..=6).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit()) && !d.starts_with('0')
    })
}

impl SmsGatewayConfig {
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
        for inline in [
            "SMS_GATEWAY_CONTENT_KEY",
            "SMS_GATEWAY_FINGERPRINT_KEY",
            "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET",
        ] {
            if get(inline).is_some() {
                return Err(ConfigError::invalid(
                    "SMS_GATEWAY_*_KEY",
                    "inline key material is not accepted; mount it as a secret file (…_FILE)",
                ));
            }
        }
        let database_url =
            get("DATABASE_URL")
                .map(SecretString::from)
                .ok_or(ConfigError::Missing {
                    var: "DATABASE_URL",
                })?;
        let bind_address = address(&get, "SMS_GATEWAY_BIND_ADDRESS")?
            .unwrap_or_else(|| DEFAULT_BIND_ADDRESS.parse().expect("valid default"));
        let metrics_address = address(&get, "SMS_GATEWAY_METRICS_ADDRESS")?;
        if metrics_address == Some(bind_address) {
            return Err(ConfigError::invalid(
                "SMS_GATEWAY_METRICS_ADDRESS",
                "must differ from SMS_GATEWAY_BIND_ADDRESS",
            ));
        }
        let database_max_connections =
            bounded(&get, "SMS_GATEWAY_DATABASE_MAX_CONNECTIONS", 10, 1, 500)? as u32;
        let base_ms = bounded(&get, "SMS_GATEWAY_RETRY_BASE_MS", 1_000, 10, 600_000)?;
        let max_ms = bounded(&get, "SMS_GATEWAY_RETRY_MAX_MS", 300_000, 10, 86_400_000)?;
        if max_ms < base_ms {
            return Err(ConfigError::invalid(
                "SMS_GATEWAY_RETRY_MAX_MS",
                "must not be below SMS_GATEWAY_RETRY_BASE_MS",
            ));
        }
        let workers = WorkerSettings {
            interval: Duration::from_millis(bounded(
                &get,
                "SMS_GATEWAY_WORKER_INTERVAL_MS",
                1_000,
                100,
                60_000,
            )? as u64),
            lease: chrono::Duration::seconds(bounded(
                &get,
                "SMS_GATEWAY_WORKER_LEASE_SECONDS",
                60,
                5,
                3_600,
            )?),
            outbox_max_attempts: bounded(&get, "SMS_GATEWAY_OUTBOX_MAX_ATTEMPTS", 12, 1, 100)?
                as i32,
            retry: RetryPolicy {
                base: chrono::Duration::milliseconds(base_ms),
                max: chrono::Duration::milliseconds(max_ms),
            },
        };
        let content_key_id = get("SMS_GATEWAY_CONTENT_KEY_ID").ok_or(ConfigError::Missing {
            var: "SMS_GATEWAY_CONTENT_KEY_ID",
        })?;
        if !key_id(&content_key_id) {
            return Err(ConfigError::invalid(
                "SMS_GATEWAY_CONTENT_KEY_ID",
                "must be 1-32 characters of [A-Za-z0-9_-], starting alphanumeric",
            ));
        }
        let previous = match (
            get("SMS_GATEWAY_PREVIOUS_CONTENT_KEY_ID"),
            get("SMS_GATEWAY_PREVIOUS_CONTENT_KEY_FILE"),
        ) {
            (None, None) => None,
            (Some(id), Some(file)) if key_id(&id) && id != content_key_id => {
                Some((id, PathBuf::from(file)))
            }
            _ => return Err(ConfigError::invalid(
                "SMS_GATEWAY_PREVIOUS_CONTENT_KEY_ID",
                "the previous key needs both its id (distinct from the current one) and its file",
            )),
        };
        let protection = ProtectionFiles {
            content_key_id,
            content_key_file: get("SMS_GATEWAY_CONTENT_KEY_FILE")
                .map(PathBuf::from)
                .ok_or(ConfigError::Missing {
                    var: "SMS_GATEWAY_CONTENT_KEY_FILE",
                })?,
            previous,
            fingerprint_key_file: get("SMS_GATEWAY_FINGERPRINT_KEY_FILE")
                .map(PathBuf::from)
                .ok_or(ConfigError::Missing {
                    var: "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
                })?,
        };
        let provider = match get("SMS_GATEWAY_PROVIDER").as_deref() {
            None => ProviderChoice::None,
            Some("deterministic") => ProviderChoice::Deterministic {
                callback_secret_file: get("SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE")
                    .map(PathBuf::from)
                    .ok_or(ConfigError::Missing {
                        var: "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
                    })?,
            },
            Some(_) => {
                return Err(ConfigError::invalid(
                    "SMS_GATEWAY_PROVIDER",
                    "the only adapter in this build is `deterministic` (local / test)",
                ))
            }
        };
        let transport = TransportSettings {
            submit_timeout: Duration::from_millis(bounded(
                &get,
                "SMS_GATEWAY_SUBMIT_TIMEOUT_MS",
                10_000,
                100,
                60_000,
            )? as u64),
            max_submit_attempts: bounded(&get, "SMS_GATEWAY_MAX_SUBMIT_ATTEMPTS", 3, 1, 10)? as i16,
            default_validity: chrono::Duration::seconds(bounded(
                &get,
                "SMS_GATEWAY_VALIDITY_SECONDS",
                3_600,
                60,
                86_400,
            )?),
            content_retention: chrono::Duration::days(bounded(
                &get,
                "SMS_GATEWAY_CONTENT_RETENTION_DAYS",
                30,
                1,
                3_650,
            )?),
            quarantine_retention: chrono::Duration::days(bounded(
                &get,
                "SMS_GATEWAY_QUARANTINE_RETENTION_DAYS",
                7,
                1,
                90,
            )?),
        };
        let allowed_prefixes: Vec<String> = get("SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES")
            .map(|v| {
                v.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if allowed_prefixes.iter().any(|p| !prefix(p)) || allowed_prefixes.len() > 200 {
            return Err(ConfigError::invalid(
                "SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES",
                "must be comma-separated E.164 prefixes such as +1,+44",
            ));
        }
        let currency = get("SMS_GATEWAY_COST_CURRENCY").unwrap_or_else(|| "USD".into());
        if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(ConfigError::invalid(
                "SMS_GATEWAY_COST_CURRENCY",
                "must be an ISO 4217 code",
            ));
        }
        let controls = ControlSettings {
            allowed_prefixes,
            rate_per_minute: bounded(&get, "SMS_GATEWAY_RATE_PER_MINUTE", 60, 1, 100_000)?,
            destination_rate_per_hour: bounded(
                &get,
                "SMS_GATEWAY_DESTINATION_RATE_PER_HOUR",
                10,
                1,
                10_000,
            )?,
            segment_cost_minor: bounded(&get, "SMS_GATEWAY_SEGMENT_COST_MINOR", 5, 0, 1_000_000)?,
            currency,
            daily_spend_limit_minor: bounded(
                &get,
                "SMS_GATEWAY_DAILY_SPEND_LIMIT_MINOR",
                10_000,
                0,
                i64::MAX / 4,
            )?,
            abuse_identical_recipients: bounded(
                &get,
                "SMS_GATEWAY_ABUSE_IDENTICAL_RECIPIENTS",
                20,
                2,
                100_000,
            )?,
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
            protection,
            provider,
            transport,
            controls,
            log_format,
        })
    }

    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }
}

/// Reads a base64 key file (whitespace trimmed); the error never contains the content.
pub fn read_key_file(var: &'static str, path: &std::path::Path) -> Result<Vec<u8>, ConfigError> {
    use base64::Engine;
    let raw =
        std::fs::read_to_string(path).map_err(|_| ConfigError::invalid(var, "cannot be read"))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw.trim()))
        .map_err(|_| ConfigError::invalid(var, "must contain base64 key material"))?;
    Ok(decoded)
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
        ("DATABASE_URL", "postgres://sms:pw-123@db/s"),
        ("SMS_GATEWAY_CONTENT_KEY_ID", "content-1"),
        (
            "SMS_GATEWAY_CONTENT_KEY_FILE",
            "/run/secrets/sms-content.key",
        ),
        (
            "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
            "/run/secrets/sms-fingerprint.key",
        ),
    ];

    #[test]
    fn requires_its_own_database_and_key_files_and_applies_defaults() {
        let cfg = SmsGatewayConfig::from_lookup(lookup(BASE)).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert_eq!(cfg.provider, ProviderChoice::None);
        assert!(
            cfg.controls.allowed_prefixes.is_empty(),
            "fail closed: no destination"
        );
        assert!(!format!("{cfg:?}").contains("pw-123"));
        for missing in BASE.iter().map(|(k, _)| *k) {
            let pairs: Vec<_> = BASE
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            assert!(
                SmsGatewayConfig::from_lookup(lookup(&pairs)).is_err(),
                "{missing}"
            );
        }
    }

    #[test]
    fn inline_keys_unknown_providers_and_bad_tunables_are_refused_without_echo() {
        for (var, value) in [
            ("SMS_GATEWAY_CONTENT_KEY", "c2VjcmV0LWtleS1tYXRlcmlhbA=="),
            ("SMS_GATEWAY_PROVIDER", "carrier-x"),
            ("SMS_GATEWAY_PROVIDER", "deterministic"),
            ("SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES", "+1,44"),
            ("SMS_GATEWAY_RATE_PER_MINUTE", "0"),
            ("SMS_GATEWAY_MAX_SUBMIT_ATTEMPTS", "11"),
            ("SMS_GATEWAY_COST_CURRENCY", "dollars"),
            ("SMS_GATEWAY_CONTENT_KEY_ID", "has space"),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            let err = SmsGatewayConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(!err.to_string().contains(value) || value.len() < 4, "{var}");
        }
        let mut ok = BASE.to_vec();
        ok.push(("SMS_GATEWAY_PROVIDER", "deterministic"));
        ok.push((
            "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
            "/run/secrets/cb",
        ));
        ok.push(("SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES", "+1, +44"));
        let cfg = SmsGatewayConfig::from_lookup(lookup(&ok)).unwrap();
        assert!(matches!(cfg.provider, ProviderChoice::Deterministic { .. }));
        assert_eq!(cfg.controls.allowed_prefixes, vec!["+1", "+44"]);
    }
}

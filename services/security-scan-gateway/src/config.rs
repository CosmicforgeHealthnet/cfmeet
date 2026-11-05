//! Runtime configuration (environment only; no secret is ever inline).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.security-scan` |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | receiver-authoritative trust bundle (callers' public keys and `security-scan.submit` grants) |
//! | `SCAN_GATEWAY_CLIENTS_FILE` | yes | client registry: product, size, concurrency and rate limits of every caller |
//! | `SCAN_GATEWAY_VERDICT_KEY_FILE` | yes | PKCS#8 PEM Ed25519 key that signs definitive verdicts |
//! | `SCAN_GATEWAY_VERDICT_KEY_ID` | yes | `kid` of that key |
//! | `SCAN_GATEWAY_BACKEND_ADDRESS` | yes | `host:port` of the EXISTING clamd (private network only) |
//! | `SCAN_GATEWAY_BACKEND_MAX_CONCURRENCY` | no | scans in flight at the engine, all callers together (1–8, default 2) |
//! | `SCAN_GATEWAY_BACKEND_STREAM_MAX_BYTES` | yes | the engine's own `StreamMaxLength`; no client limit may exceed it |
//! | `SCAN_GATEWAY_SCAN_TIMEOUT_MS` | no | whole-scan budget once admitted (1000–300000, default 60000) |
//! | `SCAN_GATEWAY_QUEUE_TIMEOUT_MS` | no | how long a request may wait for a slot (0–30000, default 2000) |
//! | `SCAN_GATEWAY_MAX_SIGNATURE_AGE_HOURS` | no | refuse to scan with older signatures (1–168, default 72) |
//! | `SCAN_GATEWAY_BIND_ADDRESS` | no | default `0.0.0.0:8099` |
//! | `SCAN_GATEWAY_METRICS_ADDRESS` | no | private Prometheus listener; unset: disabled |

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use cf_service_auth::{services, KeyId, RuntimeEnvironment};

#[derive(Debug, Clone)]
pub struct BackendConfig {
    pub address: String,
    pub max_concurrency: usize,
    pub stream_max_bytes: u64,
    pub connect_timeout: Duration,
    pub max_signature_age: Duration,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub environment: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
    pub clients_file: PathBuf,
    pub verdict_key_file: PathBuf,
    pub verdict_key_id: KeyId,
    pub backend: BackendConfig,
    pub scan_timeout: Duration,
    pub queue_timeout: Duration,
    pub bind: SocketAddr,
    pub metrics_bind: Option<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{var}: {reason}")]
pub struct ConfigError {
    pub var: &'static str,
    pub reason: String,
}

fn err(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError {
        var,
        reason: reason.into(),
    }
}

fn number(
    get: &dyn Fn(&str) -> Option<String>,
    var: &'static str,
    default: Option<u64>,
    min: u64,
    max: u64,
) -> Result<u64, ConfigError> {
    let value = match get(var) {
        Some(v) => v
            .trim()
            .parse::<u64>()
            .map_err(|_| err(var, "must be an integer"))?,
        None => default.ok_or_else(|| err(var, "is required"))?,
    };
    if !(min..=max).contains(&value) {
        return Err(err(var, format!("must be within {min}–{max}")));
    }
    Ok(value)
}

fn required(
    get: &dyn Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<String, ConfigError> {
    get(var)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| err(var, "is required"))
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        if let Some(id) = get("CF_SERVICE_ID") {
            if id != services::SECURITY_SCAN {
                return Err(err("CF_SERVICE_ID", "must be cfmeet.security-scan"));
            }
        }
        let environment = RuntimeEnvironment::parse(&required(get, "CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| {
                err(
                    "CF_RUNTIME_ENVIRONMENT",
                    "must be local, test, staging or production",
                )
            })?;
        let address = required(get, "SCAN_GATEWAY_BACKEND_ADDRESS")?;
        let (host, port) = address
            .rsplit_once(':')
            .ok_or_else(|| err("SCAN_GATEWAY_BACKEND_ADDRESS", "must be host:port"))?;
        if host.is_empty() || port.parse::<u16>().map_or(true, |p| p == 0) {
            return Err(err("SCAN_GATEWAY_BACKEND_ADDRESS", "must be host:port"));
        }
        let backend = BackendConfig {
            address,
            max_concurrency: number(get, "SCAN_GATEWAY_BACKEND_MAX_CONCURRENCY", Some(2), 1, 8)?
                as usize,
            stream_max_bytes: number(
                get,
                "SCAN_GATEWAY_BACKEND_STREAM_MAX_BYTES",
                None,
                1024,
                4 * 1024 * 1024 * 1024,
            )?,
            connect_timeout: Duration::from_secs(5),
            max_signature_age: Duration::from_secs(
                3600 * number(
                    get,
                    "SCAN_GATEWAY_MAX_SIGNATURE_AGE_HOURS",
                    Some(72),
                    1,
                    168,
                )?,
            ),
        };
        let bind = get("SCAN_GATEWAY_BIND_ADDRESS")
            .unwrap_or_else(|| "0.0.0.0:8099".into())
            .parse()
            .map_err(|_| err("SCAN_GATEWAY_BIND_ADDRESS", "must be ip:port"))?;
        let metrics_bind = match get("SCAN_GATEWAY_METRICS_ADDRESS") {
            Some(v) => Some(
                v.parse()
                    .map_err(|_| err("SCAN_GATEWAY_METRICS_ADDRESS", "must be ip:port"))?,
            ),
            None => None,
        };
        if metrics_bind == Some(bind) {
            return Err(err(
                "SCAN_GATEWAY_METRICS_ADDRESS",
                "must differ from the scan listener",
            ));
        }
        Ok(Self {
            environment,
            trust_bundle_file: required(get, "CF_SERVICE_TRUST_BUNDLE_FILE")?.into(),
            clients_file: required(get, "SCAN_GATEWAY_CLIENTS_FILE")?.into(),
            verdict_key_file: required(get, "SCAN_GATEWAY_VERDICT_KEY_FILE")?.into(),
            verdict_key_id: KeyId::parse(&required(get, "SCAN_GATEWAY_VERDICT_KEY_ID")?)
                .map_err(|_| err("SCAN_GATEWAY_VERDICT_KEY_ID", "is not a valid key id"))?,
            backend,
            scan_timeout: Duration::from_millis(number(
                get,
                "SCAN_GATEWAY_SCAN_TIMEOUT_MS",
                Some(60_000),
                1_000,
                300_000,
            )?),
            queue_timeout: Duration::from_millis(number(
                get,
                "SCAN_GATEWAY_QUEUE_TIMEOUT_MS",
                Some(2_000),
                0,
                30_000,
            )?),
            bind,
            metrics_bind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn base() -> HashMap<&'static str, String> {
        [
            ("CF_RUNTIME_ENVIRONMENT", "production"),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", "/run/trust.json"),
            ("SCAN_GATEWAY_CLIENTS_FILE", "/run/clients.json"),
            ("SCAN_GATEWAY_VERDICT_KEY_FILE", "/run/verdict.pem"),
            ("SCAN_GATEWAY_VERDICT_KEY_ID", "scan-verdict-1"),
            ("SCAN_GATEWAY_BACKEND_ADDRESS", "mail-clamav:3310"),
            ("SCAN_GATEWAY_BACKEND_STREAM_MAX_BYTES", "26214400"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect()
    }

    fn load(m: &HashMap<&'static str, String>) -> Result<Config, ConfigError> {
        Config::from_lookup(&|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_are_conservative() {
        let c = load(&base()).unwrap();
        assert_eq!(c.backend.max_concurrency, 2);
        assert_eq!(c.backend.max_signature_age, Duration::from_secs(72 * 3600));
        assert_eq!(c.queue_timeout, Duration::from_secs(2));
        assert!(c.metrics_bind.is_none());
    }

    #[test]
    fn missing_or_unsafe_values_are_refused() {
        for var in [
            "CF_RUNTIME_ENVIRONMENT",
            "CF_SERVICE_TRUST_BUNDLE_FILE",
            "SCAN_GATEWAY_CLIENTS_FILE",
            "SCAN_GATEWAY_VERDICT_KEY_FILE",
            "SCAN_GATEWAY_BACKEND_ADDRESS",
            "SCAN_GATEWAY_BACKEND_STREAM_MAX_BYTES",
        ] {
            let mut m = base();
            m.remove(var);
            assert_eq!(load(&m).unwrap_err().var, var);
        }
        for (var, value) in [
            ("SCAN_GATEWAY_BACKEND_MAX_CONCURRENCY", "9"),
            ("SCAN_GATEWAY_BACKEND_MAX_CONCURRENCY", "0"),
            ("SCAN_GATEWAY_MAX_SIGNATURE_AGE_HOURS", "500"),
            ("SCAN_GATEWAY_BACKEND_ADDRESS", "clamd"),
            ("CF_SERVICE_ID", "cfmeet.messaging"),
            ("SCAN_GATEWAY_METRICS_ADDRESS", "0.0.0.0:8099"),
        ] {
            let mut m = base();
            m.insert(var, value.into());
            assert_eq!(load(&m).unwrap_err().var, var, "{var}={value}");
        }
    }
}

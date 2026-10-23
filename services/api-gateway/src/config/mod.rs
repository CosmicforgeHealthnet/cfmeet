//! API Gateway configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `API_GATEWAY_BIND_ADDRESS` | no | default `127.0.0.1:8099` |
//! | `API_GATEWAY_PUBLIC_ROUTING_ENABLED` | no | feature flag (default `false`): serve the route catalogue; off, every customer path is NOT_FOUND |
//! | `API_GATEWAY_WEBSOCKETS_ENABLED` | no | feature flag (default `false`): admit WebSocket routes into the catalogue (forwarding them is Section 51) |
//! | `API_GATEWAY_UPSTREAM_TIMEOUT_MS` | no | per-request upstream deadline (100–60000, default 10000) |
//! | `API_GATEWAY_MAX_BODY_BYTES` | no | request body limit (1024–8388608, default 1048576) |
//! | `API_GATEWAY_REDIS_URL` | no | Redis for future SHARED quotas (`redis://` local / test, `rediss://` otherwise); unset: quotas not enforced |
//! | `API_GATEWAY_REDIS_KEY_PREFIX` | no | key namespace (default `cfgw:`) |
//! | `LOG_FORMAT` | no | `pretty` / `json` |
//!
//! Service authentication (`CF_*`) is read by [`service_auth`]. The gateway has NO
//! database. Error messages never echo a value.

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

/// Disabled-by-default feature flags (Part VI rollout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeatureFlags {
    pub public_routing: bool,
    pub websockets: bool,
}

/// Redis settings of the future shared quota store (Sections 50+). Configuration only:
/// no quota is enforced by this foundation.
#[derive(Clone)]
pub struct QuotaSettings {
    redis_url: Option<SecretString>,
    pub key_prefix: String,
}

impl Default for QuotaSettings {
    fn default() -> Self {
        Self {
            redis_url: None,
            key_prefix: "cfgw:".into(),
        }
    }
}

impl fmt::Debug for QuotaSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuotaSettings")
            .field("redis_url", &self.redis_url.as_ref().map(|_| "[REDACTED]"))
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

impl QuotaSettings {
    pub fn configured(&self) -> bool {
        self.redis_url.is_some()
    }

    pub fn redis_url(&self) -> Option<&str> {
        self.redis_url.as_ref().map(|u| u.expose_secret())
    }

    /// TLS (`rediss://`) is required outside local / test runtime environments.
    pub fn validate_for(&self, runtime: RuntimeEnvironment) -> Result<(), ConfigError> {
        match self.redis_url() {
            Some(url) if url.starts_with("redis://") && !runtime.allows_insecure_transport() => {
                Err(ConfigError::invalid(
                    "API_GATEWAY_REDIS_URL",
                    "must use rediss:// (TLS) in staging and production",
                ))
            }
            _ => Ok(()),
        }
    }
}

pub struct ApiGatewayConfig {
    pub bind_address: SocketAddr,
    pub flags: FeatureFlags,
    pub upstream_timeout: Duration,
    pub max_body_bytes: usize,
    pub quotas: QuotaSettings,
    pub log_format: LogFormat,
}

impl fmt::Debug for ApiGatewayConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiGatewayConfig")
            .field("bind_address", &self.bind_address)
            .field("flags", &self.flags)
            .field("upstream_timeout", &self.upstream_timeout)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("quotas", &self.quotas)
            .finish_non_exhaustive()
    }
}

pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8099";

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

fn flag(get: &impl Fn(&str) -> Option<String>, var: &'static str) -> Result<bool, ConfigError> {
    match get(var).as_deref() {
        None | Some("false") | Some("0") => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some(_) => Err(ConfigError::invalid(var, "must be `true` or `false`")),
    }
}

impl ApiGatewayConfig {
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
        // The gateway owns no data: a database is a misconfiguration.
        if get("DATABASE_URL").is_some() {
            return Err(ConfigError::invalid(
                "DATABASE_URL",
                "the API Gateway has no database; remove the variable",
            ));
        }
        let bind_address = match get("API_GATEWAY_BIND_ADDRESS") {
            None => DEFAULT_BIND_ADDRESS.parse().expect("valid default"),
            Some(v) => v.parse::<SocketAddr>().map_err(|_| {
                ConfigError::invalid(
                    "API_GATEWAY_BIND_ADDRESS",
                    "must be a socket address such as 127.0.0.1:8099",
                )
            })?,
        };
        let flags = FeatureFlags {
            public_routing: flag(&get, "API_GATEWAY_PUBLIC_ROUTING_ENABLED")?,
            websockets: flag(&get, "API_GATEWAY_WEBSOCKETS_ENABLED")?,
        };
        let upstream_timeout = Duration::from_millis(bounded(
            &get,
            "API_GATEWAY_UPSTREAM_TIMEOUT_MS",
            10_000,
            100,
            60_000,
        )? as u64);
        let max_body_bytes = bounded(
            &get,
            "API_GATEWAY_MAX_BODY_BYTES",
            1_048_576,
            1_024,
            8_388_608,
        )? as usize;
        let redis_url = match get("API_GATEWAY_REDIS_URL") {
            None => None,
            Some(v) => {
                let parsed = url::Url::parse(&v).ok().filter(|u| {
                    matches!(u.scheme(), "redis" | "rediss")
                        && u.host_str().is_some_and(|h| !h.is_empty())
                });
                if parsed.is_none() {
                    return Err(ConfigError::invalid(
                        "API_GATEWAY_REDIS_URL",
                        "must be a redis:// or rediss:// URL with a host",
                    ));
                }
                Some(SecretString::from(v))
            }
        };
        let key_prefix = get("API_GATEWAY_REDIS_KEY_PREFIX").unwrap_or_else(|| "cfgw:".into());
        if key_prefix.len() > 32
            || !key_prefix.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b':' | b'-' | b'_')
            })
        {
            return Err(ConfigError::invalid(
                "API_GATEWAY_REDIS_KEY_PREFIX",
                "must be at most 32 characters of [a-z0-9:_-]",
            ));
        }
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
            bind_address,
            flags,
            upstream_timeout,
            max_body_bytes,
            quotas: QuotaSettings {
                redis_url,
                key_prefix,
            },
            log_format,
        })
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
    fn defaults_are_default_deny_without_a_database_or_quotas() {
        let cfg = ApiGatewayConfig::from_lookup(lookup(&[])).unwrap();
        assert_eq!(cfg.bind_address.to_string(), DEFAULT_BIND_ADDRESS);
        assert_eq!(cfg.flags, FeatureFlags::default());
        assert!(!cfg.quotas.configured());
        assert!(
            ApiGatewayConfig::from_lookup(lookup(&[("DATABASE_URL", "postgres://x/y")])).is_err()
        );
    }

    #[test]
    fn redis_is_validated_redacted_and_tls_outside_local() {
        let cfg = ApiGatewayConfig::from_lookup(lookup(&[(
            "API_GATEWAY_REDIS_URL",
            "redis://:pw-redis-9@127.0.0.1:6379/2",
        )]))
        .unwrap();
        assert!(cfg.quotas.configured());
        assert!(!format!("{cfg:?}").contains("pw-redis-9"));
        assert!(cfg.quotas.validate_for(RuntimeEnvironment::Local).is_ok());
        assert!(cfg
            .quotas
            .validate_for(RuntimeEnvironment::Production)
            .is_err());
        let tls = ApiGatewayConfig::from_lookup(lookup(&[(
            "API_GATEWAY_REDIS_URL",
            "rediss://cache.internal:6380",
        )]))
        .unwrap();
        assert!(tls
            .quotas
            .validate_for(RuntimeEnvironment::Production)
            .is_ok());
        for (var, value) in [
            ("API_GATEWAY_REDIS_URL", "http://cache-host-name:6379"),
            ("API_GATEWAY_REDIS_KEY_PREFIX", "Has Spaces Here"),
            ("API_GATEWAY_PUBLIC_ROUTING_ENABLED", "sometimes"),
            ("API_GATEWAY_UPSTREAM_TIMEOUT_MS", "999999"),
            ("API_GATEWAY_MAX_BODY_BYTES", "12"),
        ] {
            let err = ApiGatewayConfig::from_lookup(lookup(&[(var, value)])).unwrap_err();
            assert!(!err.to_string().contains(value), "{var}");
        }
    }
}

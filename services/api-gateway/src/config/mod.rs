//! API Gateway configuration, read from the environment.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `API_GATEWAY_BIND_ADDRESS` | no | default `127.0.0.1:8099` |
//! | `API_GATEWAY_PUBLIC_ROUTING_ENABLED` | no | feature flag (default `false`): serve the route catalogue; off, every customer path is NOT_FOUND |
//! | `API_GATEWAY_WEBSOCKETS_ENABLED` | no | feature flag (default `false`): admit WebSocket routes into the catalogue (forwarding them is Section 51) |
//! | `API_GATEWAY_UPSTREAM_TIMEOUT_MS` | no | per-request upstream deadline (100–60000, default 10000) |
//! | `API_GATEWAY_MAX_BODY_BYTES` | no | request body limit (1024–8388608, default 1048576) |
//! | `API_GATEWAY_REDIS_URL` | no | Redis for SHARED atomic quota counters across instances (`redis://` local / test, `rediss://` otherwise); unset: per-instance counters (quotas still enforced, limits from the Developer Platform) |
//! | `API_GATEWAY_REDIS_PRIVATE_NETWORK` | no | `true`: also accept a plaintext `redis://` store on a private host (single-label container name, private / loopback IP) outside local / test (gap audit P5) |
//! | `API_GATEWAY_REDIS_KEY_PREFIX` | no | key namespace (default `cfgw:`) |
//! | `API_GATEWAY_REDIS_TIMEOUT_MS` | no | bound of one quota-store call and of one connection attempt (20–5000, default 250); past it the request is counted per instance (`quotas: degraded`) |
//! | `API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE` | no | per-token request ceiling of end-user / device traffic (meeting sessions, participant tokens, developer sessions, client tokens, signaling credentials, realtime tickets): `0` (default) off, else 10–100000 requests per token per minute, counted in the same quota store; anonymous requests carry no token and stay with the services and the edge |
//! | `API_GATEWAY_USER_TOKENS_ENABLED` | no | feature flag (default `false`): accept CF Meet user tokens (`Authorization: Bearer cfut_…`) on `USER_TOKEN` routes — signature and expiry pre-checked with Platform Access's published keys (`platform-user-token.verify`, refreshed every 5 minutes or less; no keys: refused), `Authorization` forwarded unchanged for the owning service's live verification |
//! | `API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED` | no | feature flag (default `false`): CORS for partner websites registered per environment in Platform Access (`platform-environment-origins.read` change feed, polled every 30 s or less) on routes accepting a device authentication mode; `Origin` forwarded on WebSocket upgrades |
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
    /// `API_GATEWAY_USER_TOKENS_ENABLED` (ADR 0062): the `USER_TOKEN` mode.
    pub user_tokens: bool,
    /// `API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED` (ADR 0062): partner-website CORS.
    pub environment_origins: bool,
}

/// The quota store shared by every gateway instance. With `API_GATEWAY_REDIS_URL` the
/// counters are shared atomic Redis windows; without it (or while it is unreachable)
/// each instance keeps its own counters. Either way the gateway ENFORCES the platform
/// request quotas of platform-credential traffic (per application environment and, when
/// restricted, per service account / credential; limits from the Developer Platform)
/// and, when `API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE` is set, the per-token ceiling of
/// end-user / device traffic ([`crate::quota`]).
#[derive(Clone)]
pub struct QuotaSettings {
    redis_url: Option<SecretString>,
    pub key_prefix: String,
    /// Bound of one quota-store call / connection attempt.
    pub redis_timeout: Duration,
    /// Gap audit P5: `API_GATEWAY_REDIS_PRIVATE_NETWORK=true` accepts a plaintext
    /// `redis://` store on a private host (no dots, or an RFC 1918 / loopback address),
    /// such as the release's internal `redis` container, outside local / test.
    pub private_network: bool,
}

impl Default for QuotaSettings {
    fn default() -> Self {
        Self {
            redis_url: None,
            private_network: false,
            key_prefix: "cfgw:".into(),
            redis_timeout: Duration::from_millis(250),
        }
    }
}

impl fmt::Debug for QuotaSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuotaSettings")
            .field("redis_url", &self.redis_url.as_ref().map(|_| "[REDACTED]"))
            .field("key_prefix", &self.key_prefix)
            .field("redis_timeout", &self.redis_timeout)
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
            Some(url)
                if url.starts_with("redis://")
                    && !runtime.allows_insecure_transport()
                    && !(self.private_network && private_host(url)) =>
            {
                Err(ConfigError::invalid(
                    "API_GATEWAY_REDIS_URL",
                    "must use rediss:// (TLS) in staging and production, or a private-network \
                     host with API_GATEWAY_REDIS_PRIVATE_NETWORK=true",
                ))
            }
            _ => Ok(()),
        }
    }
}

/// A host only reachable inside the deployment: a single-label name (a container on the
/// private network) or a private / loopback IP address.
fn private_host(url: &str) -> bool {
    let Some(host) = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    else {
        return false;
    };
    match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => !host.contains('.') && !host.is_empty(),
    }
}

pub struct ApiGatewayConfig {
    pub bind_address: SocketAddr,
    pub flags: FeatureFlags,
    /// Routing rollback controls (disabled API families, central webhooks cut-over).
    pub controls: crate::routing::RoutingControls,
    /// Exact allowed browser origins (`scheme://host[:port]`); empty: no CORS.
    pub cors_allowed_origins: Vec<String>,
    pub max_header_bytes: usize,
    pub max_headers: usize,
    pub ws_max_message_bytes: usize,
    pub quota_flush_interval: Duration,
    pub upstream_timeout: Duration,
    pub max_body_bytes: usize,
    pub quotas: QuotaSettings,
    /// Per-token requests per minute of end-user / device traffic (`0`: off).
    pub token_rate_limit_per_minute: u64,
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
            .field(
                "token_rate_limit_per_minute",
                &self.token_rate_limit_per_minute,
            )
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
            user_tokens: flag(&get, "API_GATEWAY_USER_TOKENS_ENABLED")?,
            environment_origins: flag(&get, "API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED")?,
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
        let redis_timeout =
            Duration::from_millis(
                bounded(&get, "API_GATEWAY_REDIS_TIMEOUT_MS", 250, 20, 5_000)? as u64,
            );
        const DOMAINS: &[&str] = &[
            "meetings",
            "calling",
            "privacy-relay",
            "messaging",
            "numbering",
            "sms",
            "developer-platform",
            "gateway",
        ];
        let mut controls = crate::routing::RoutingControls::default();
        if let Some(v) = get("API_GATEWAY_DISABLED_DOMAINS") {
            for d in v.split(',').map(str::trim).filter(|d| !d.is_empty()) {
                if !DOMAINS.contains(&d) {
                    return Err(ConfigError::invalid(
                        "API_GATEWAY_DISABLED_DOMAINS",
                        format!("unknown API family (known: {})", DOMAINS.join(", ")),
                    ));
                }
                controls.disabled_domains.insert(d.to_string());
            }
        }
        controls.central_webhooks = flag(&get, "API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED")?;
        controls.central_usage = flag(&get, "API_GATEWAY_CENTRAL_USAGE_ENABLED")?;
        let mut cors_allowed_origins = Vec::new();
        if let Some(v) = get("API_GATEWAY_CORS_ALLOWED_ORIGINS") {
            for o in v.split(',').map(str::trim).filter(|o| !o.is_empty()) {
                let ok = url::Url::parse(o).ok().is_some_and(|u| {
                    matches!(u.scheme(), "https" | "http")
                        && u.host_str().is_some()
                        && u.path() == "/"
                        && u.query().is_none()
                        && u.username().is_empty()
                        && !o.ends_with('/')
                });
                if !ok || o == "*" {
                    return Err(ConfigError::invalid(
                        "API_GATEWAY_CORS_ALLOWED_ORIGINS",
                        "must be a comma-separated list of exact origins such as https://app.cfmeet.com (no wildcard)",
                    ));
                }
                cors_allowed_origins.push(o.to_string());
            }
        }
        let max_header_bytes =
            bounded(&get, "API_GATEWAY_MAX_HEADER_BYTES", 16_384, 1_024, 65_536)? as usize;
        let max_headers = bounded(&get, "API_GATEWAY_MAX_HEADERS", 64, 8, 200)? as usize;
        let ws_max_message_bytes = bounded(
            &get,
            "API_GATEWAY_WS_MAX_MESSAGE_BYTES",
            1_048_576,
            1_024,
            16_777_216,
        )? as usize;
        let quota_flush_interval = Duration::from_millis(bounded(
            &get,
            "API_GATEWAY_QUOTA_FLUSH_INTERVAL_MS",
            5_000,
            100,
            300_000,
        )? as u64);
        let token_rate_limit_per_minute = match bounded(
            &get,
            "API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE",
            0,
            0,
            100_000,
        )? {
            1..=9 => {
                return Err(ConfigError::invalid(
                    "API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE",
                    "must be 0 (off) or between 10 and 100000",
                ))
            }
            n => n as u64,
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
            bind_address,
            flags,
            controls,
            cors_allowed_origins,
            max_header_bytes,
            max_headers,
            ws_max_message_bytes,
            quota_flush_interval,
            upstream_timeout,
            max_body_bytes,
            quotas: QuotaSettings {
                redis_url,
                key_prefix,
                redis_timeout,
                private_network: flag(&get, "API_GATEWAY_REDIS_PRIVATE_NETWORK")?,
            },
            token_rate_limit_per_minute,
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
        assert_eq!(
            cfg.token_rate_limit_per_minute, 0,
            "the token ceiling is off"
        );
        assert!(
            ApiGatewayConfig::from_lookup(lookup(&[("DATABASE_URL", "postgres://x/y")])).is_err()
        );
    }

    #[test]
    fn platform_build_flags_default_off_and_parse() {
        let cfg = ApiGatewayConfig::from_lookup(lookup(&[])).unwrap();
        assert!(!cfg.flags.user_tokens && !cfg.flags.environment_origins);
        let on = ApiGatewayConfig::from_lookup(lookup(&[
            ("API_GATEWAY_USER_TOKENS_ENABLED", "true"),
            ("API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED", "1"),
        ]))
        .unwrap();
        assert!(on.flags.user_tokens && on.flags.environment_origins);
        for var in [
            "API_GATEWAY_USER_TOKENS_ENABLED",
            "API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED",
        ] {
            assert!(ApiGatewayConfig::from_lookup(lookup(&[(var, "maybe")])).is_err());
        }
    }

    #[test]
    fn the_token_ceiling_is_off_or_bounded() {
        let on = ApiGatewayConfig::from_lookup(lookup(&[(
            "API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE",
            "600",
        )]))
        .unwrap();
        assert_eq!(on.token_rate_limit_per_minute, 600);
        let off = ApiGatewayConfig::from_lookup(lookup(&[(
            "API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE",
            "0",
        )]))
        .unwrap();
        assert_eq!(off.token_rate_limit_per_minute, 0);
        for bad in ["5", "100001", "-1", "lots"] {
            assert!(
                ApiGatewayConfig::from_lookup(lookup(&[(
                    "API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE",
                    bad
                )]))
                .is_err(),
                "{bad}"
            );
        }
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
        // Gap audit P5: plaintext only on a private host, and only when asked for.
        for (url, ok) in [
            ("redis://redis:6379/3", true),
            ("redis://10.0.0.7:6379", true),
            ("redis://cache.example.com:6379", false),
            ("redis://8.8.8.8:6379", false),
        ] {
            let private = ApiGatewayConfig::from_lookup(lookup(&[
                ("API_GATEWAY_REDIS_URL", url),
                ("API_GATEWAY_REDIS_PRIVATE_NETWORK", "true"),
            ]))
            .unwrap();
            assert_eq!(
                private
                    .quotas
                    .validate_for(RuntimeEnvironment::Production)
                    .is_ok(),
                ok,
                "{url}"
            );
        }
        let unasked = ApiGatewayConfig::from_lookup(lookup(&[(
            "API_GATEWAY_REDIS_URL",
            "redis://redis:6379/3",
        )]))
        .unwrap();
        assert!(unasked
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

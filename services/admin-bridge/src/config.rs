//! Admin bridge configuration, read from the environment. Fails closed; error messages
//! never echo a value. Everything is OFF by default.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `ADMIN_BRIDGE_ENABLED` | no | `true` / `false` (default `false`): off, every HQ request is `503 BRIDGE_DISABLED` |
//! | `ADMIN_BRIDGE_WRITES_ENABLED` | no | default `false`: every command is `503 CAPABILITY_DISABLED` |
//! | `ADMIN_BRIDGE_MODULE_<MODULE>` | no | default `false`, one per HQ module (`ACCOUNTS`, `MEETINGS`, …) |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging`, `production` |
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.admin-bridge` |
//! | `DATABASE_URL` | when enabled | the bridge's OWN database (`cfmeet_admin_bridge`), runtime role |
//! | `ADMIN_BRIDGE_DATABASE_MAX_CONNECTIONS` | no | 1–50 (default 5) |
//! | `ADMIN_BRIDGE_HQ_BIND_ADDRESS` | no | private mTLS listener (default `127.0.0.1:8443`) |
//! | `ADMIN_BRIDGE_OPS_BIND_ADDRESS` | no | loopback `/health`, `/ready`, `/metrics` (default `127.0.0.1:8190`; must be loopback) |
//! | `ADMIN_BRIDGE_TLS_CERT_FILE` / `ADMIN_BRIDGE_TLS_KEY_FILE` | when enabled | server certificate chain and key (PEM files) |
//! | `ADMIN_BRIDGE_CLIENT_CA_FILE` | when enabled | CA(s) that issue HQ's client certificate (PEM) |
//! | `ADMIN_BRIDGE_HQ_WORKLOAD_URI` | when enabled | the single URI SAN HQ's certificate must carry (e.g. `cf://staging/product-admin/cfmeet`) |
//! | `ADMIN_BRIDGE_HMAC_KEY_DIR` | when enabled | directory of `<X-HQ-Key-ID>.key` files (≥ 32 bytes each); the database holds only their sha256 |
//! | `ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX` | when enabled | CF Meet policy signer, Ed25519 public key (64 hex) |
//! | `ADMIN_BRIDGE_POLICY_KEY_ID` | when enabled | its key id |
//! | `ADMIN_BRIDGE_POLICY_REVISION` | when enabled | the pinned access-policy revision (≥ 1) |
//! | `ADMIN_BRIDGE_STRONG_ACR` | no | comma list (default `urn:cosmicforge:acr:mfa`); never the workload acr |
//! | `ADMIN_BRIDGE_WORKLOAD_ACR` | no | default `urn:cosmicforge:acr:workload` (only for `cfmeet.source_sync`) |
//! | `ADMIN_BRIDGE_ENROLLED_MARKETS` | when enabled | comma list of markets HQ may act in (e.g. `GLOBAL`) |
//! | `ADMIN_BRIDGE_ENROLLED_TENANTS` | when enabled | comma list of tenants HQ may act in |
//! | `ADMIN_BRIDGE_RATE_LIMIT_PER_MINUTE` | no | per staff member, permission and scope (1–10000, default 120) |
//! | `ADMIN_BRIDGE_HEALTH_<SERVICE>_URL` | no | owner health URL probed by `bridge.health.read` (`MEETING_SERVICE`, `MESSAGING`, …); unset: NOT_CONFIGURED |
//! | `LOG_FORMAT` | no | `pretty` / `json` |

use std::{
    collections::{BTreeMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
};

use cf_service_auth::{services, RuntimeEnvironment};
use ed25519_dalek::VerifyingKey;

use crate::contract::model::{Module, Owner};

pub const DEFAULT_STRONG_ACR: &str = "urn:cosmicforge:acr:mfa";
pub const DEFAULT_WORKLOAD_ACR: &str = "urn:cosmicforge:acr:workload";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{var} is invalid: {reason}")]
    Invalid {
        var: &'static str,
        reason: &'static str,
    },
}

fn invalid(var: &'static str, reason: &'static str) -> ConfigError {
    ConfigError::Invalid { var, reason }
}

/// Values the security chain needs (no file paths, no secrets loaded yet).
#[derive(Debug, Clone)]
pub struct SecurityConfig {
    pub hq_workload_uri: String,
    pub hmac_key_dir: PathBuf,
    pub policy_key: VerifyingKey,
    pub policy_key_id: String,
    pub policy_revision: i64,
    pub strong_acr: HashSet<String>,
    pub workload_acr: String,
    pub enrolled_markets: HashSet<String>,
    pub enrolled_tenants: HashSet<String>,
    pub rate_limit_per_minute: u32,
}

#[derive(Debug, Clone)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub client_ca: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Switches {
    pub enabled: bool,
    pub writes_enabled: bool,
    pub modules: HashSet<Module>,
}

impl Switches {
    pub fn module_on(&self, m: Module) -> bool {
        m.flag().is_none() || self.modules.contains(&m)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_environment: RuntimeEnvironment,
    pub switches: Switches,
    pub database_url: Option<String>,
    pub database_max_connections: u32,
    pub hq_bind: SocketAddr,
    pub ops_bind: SocketAddr,
    /// `None` only while the bridge is disabled.
    pub tls: Option<TlsFiles>,
    pub security: Option<SecurityConfig>,
    pub health_urls: BTreeMap<Owner, String>,
    pub json_logs: bool,
}

pub trait Env {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    }
}

impl<const N: usize> Env for [(&str, &str); N] {
    fn get(&self, key: &str) -> Option<String> {
        self.iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .filter(|v| !v.is_empty())
    }
}

fn flag(env: &dyn Env, var: &'static str) -> Result<bool, ConfigError> {
    match env.get(var).as_deref() {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(invalid(var, "expected true or false")),
    }
}

fn required(env: &dyn Env, var: &'static str) -> Result<String, ConfigError> {
    env.get(var).ok_or(ConfigError::Missing(var))
}

fn list(env: &dyn Env, var: &'static str) -> HashSet<String> {
    env.get(var)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn number<T: std::str::FromStr + PartialOrd>(
    env: &dyn Env,
    var: &'static str,
    default: T,
    min: T,
    max: T,
) -> Result<T, ConfigError> {
    match env.get(var) {
        None => Ok(default),
        Some(v) => match v.parse::<T>() {
            Ok(n) if n >= min && n <= max => Ok(n),
            _ => Err(invalid(var, "out of range")),
        },
    }
}

fn addr(env: &dyn Env, var: &'static str, default: &str) -> Result<SocketAddr, ConfigError> {
    env.get(var)
        .unwrap_or_else(|| default.to_string())
        .parse()
        .map_err(|_| invalid(var, "expected host:port"))
}

fn scope_value_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-'))
}

impl Config {
    pub fn from_env(env: &dyn Env) -> Result<Config, ConfigError> {
        let runtime_environment =
            RuntimeEnvironment::parse(&required(env, "CF_RUNTIME_ENVIRONMENT")?).map_err(|_| {
                invalid(
                    "CF_RUNTIME_ENVIRONMENT",
                    "expected local, test, staging or production",
                )
            })?;
        if let Some(id) = env.get("CF_SERVICE_ID") {
            if id != services::ADMIN_BRIDGE {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.admin-bridge"));
            }
        }
        let mut modules = HashSet::new();
        for m in Module::ALL {
            if let Some(var) = m.flag() {
                let var: &'static str = Box::leak(var.into_boxed_str());
                if flag(env, var)? {
                    modules.insert(*m);
                }
            }
        }
        let switches = Switches {
            enabled: flag(env, "ADMIN_BRIDGE_ENABLED")?,
            writes_enabled: flag(env, "ADMIN_BRIDGE_WRITES_ENABLED")?,
            modules,
        };
        let ops_bind = addr(env, "ADMIN_BRIDGE_OPS_BIND_ADDRESS", "127.0.0.1:8190")?;
        if !ops_bind.ip().is_loopback() {
            return Err(invalid(
                "ADMIN_BRIDGE_OPS_BIND_ADDRESS",
                "must be a loopback address",
            ));
        }
        let hq_bind = addr(env, "ADMIN_BRIDGE_HQ_BIND_ADDRESS", "127.0.0.1:8443")?;
        let mut health_urls = BTreeMap::new();
        for owner in Owner::ALL {
            if *owner == Owner::AdminBridge {
                continue;
            }
            let var: &'static str = Box::leak(
                format!(
                    "ADMIN_BRIDGE_HEALTH_{}_URL",
                    owner.as_str().replace('-', "_").to_ascii_uppercase()
                )
                .into_boxed_str(),
            );
            if let Some(url) = env.get(var) {
                let ok = url.starts_with("https://")
                    || (url.starts_with("http://")
                        && matches!(
                            runtime_environment,
                            RuntimeEnvironment::Local | RuntimeEnvironment::Test
                        ))
                    || url.starts_with("http://") && url.contains(".internal");
                if !ok {
                    return Err(invalid(
                        var,
                        "must be https (http only for private .internal hosts or local/test)",
                    ));
                }
                health_urls.insert(*owner, url);
            }
        }
        let database_max_connections =
            number(env, "ADMIN_BRIDGE_DATABASE_MAX_CONNECTIONS", 5, 1, 50)?;
        let json_logs = env.get("LOG_FORMAT").as_deref() == Some("json");

        // Disabled: nothing else is required (the bridge answers 503 to everything).
        if !switches.enabled {
            return Ok(Config {
                runtime_environment,
                switches,
                database_url: env.get("DATABASE_URL"),
                database_max_connections,
                hq_bind,
                ops_bind,
                tls: None,
                security: None,
                health_urls,
                json_logs,
            });
        }

        let tls = TlsFiles {
            cert: PathBuf::from(required(env, "ADMIN_BRIDGE_TLS_CERT_FILE")?),
            key: PathBuf::from(required(env, "ADMIN_BRIDGE_TLS_KEY_FILE")?),
            client_ca: PathBuf::from(required(env, "ADMIN_BRIDGE_CLIENT_CA_FILE")?),
        };
        let hq_workload_uri = required(env, "ADMIN_BRIDGE_HQ_WORKLOAD_URI")?;
        if !hq_workload_uri.contains("://") || hq_workload_uri.len() > 256 {
            return Err(invalid("ADMIN_BRIDGE_HQ_WORKLOAD_URI", "expected a URI"));
        }
        let policy_key_hex = required(env, "ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX")?;
        let policy_key = hex::decode(&policy_key_hex)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
            .ok_or(invalid(
                "ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX",
                "expected an Ed25519 public key (64 hex)",
            ))?;
        let policy_key_id = required(env, "ADMIN_BRIDGE_POLICY_KEY_ID")?;
        let policy_revision = number(env, "ADMIN_BRIDGE_POLICY_REVISION", 0i64, 1, i64::MAX)?;
        if policy_revision == 0 {
            return Err(ConfigError::Missing("ADMIN_BRIDGE_POLICY_REVISION"));
        }
        let workload_acr = env
            .get("ADMIN_BRIDGE_WORKLOAD_ACR")
            .unwrap_or_else(|| DEFAULT_WORKLOAD_ACR.into());
        let mut strong_acr = list(env, "ADMIN_BRIDGE_STRONG_ACR");
        if strong_acr.is_empty() {
            strong_acr.insert(DEFAULT_STRONG_ACR.into());
        }
        if strong_acr.contains(&workload_acr) {
            return Err(invalid(
                "ADMIN_BRIDGE_STRONG_ACR",
                "the workload acr can never be strong",
            ));
        }
        let enrolled_markets = list(env, "ADMIN_BRIDGE_ENROLLED_MARKETS");
        let enrolled_tenants = list(env, "ADMIN_BRIDGE_ENROLLED_TENANTS");
        if enrolled_markets.is_empty() || !enrolled_markets.iter().all(|v| scope_value_ok(v)) {
            return Err(invalid(
                "ADMIN_BRIDGE_ENROLLED_MARKETS",
                "expected a comma list of scope values",
            ));
        }
        if enrolled_tenants.is_empty() || !enrolled_tenants.iter().all(|v| scope_value_ok(v)) {
            return Err(invalid(
                "ADMIN_BRIDGE_ENROLLED_TENANTS",
                "expected a comma list of scope values",
            ));
        }
        let security = SecurityConfig {
            hq_workload_uri,
            hmac_key_dir: PathBuf::from(required(env, "ADMIN_BRIDGE_HMAC_KEY_DIR")?),
            policy_key,
            policy_key_id,
            policy_revision,
            strong_acr,
            workload_acr,
            enrolled_markets,
            enrolled_tenants,
            rate_limit_per_minute: number(
                env,
                "ADMIN_BRIDGE_RATE_LIMIT_PER_MINUTE",
                120,
                1,
                10_000,
            )?,
        };
        Ok(Config {
            runtime_environment,
            switches,
            database_url: Some(required(env, "DATABASE_URL")?),
            database_max_connections,
            hq_bind,
            ops_bind,
            tls: Some(tls),
            security: Some(security),
            health_urls,
            json_logs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    fn enabled(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![
            ("CF_RUNTIME_ENVIRONMENT", "test"),
            ("ADMIN_BRIDGE_ENABLED", "true"),
            ("DATABASE_URL", "postgres://x/y"),
            ("ADMIN_BRIDGE_TLS_CERT_FILE", "/c"),
            ("ADMIN_BRIDGE_TLS_KEY_FILE", "/k"),
            ("ADMIN_BRIDGE_CLIENT_CA_FILE", "/ca"),
            (
                "ADMIN_BRIDGE_HQ_WORKLOAD_URI",
                "cf://staging/product-admin/cfmeet",
            ),
            ("ADMIN_BRIDGE_HMAC_KEY_DIR", "/keys"),
            ("ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX", KEY),
            ("ADMIN_BRIDGE_POLICY_KEY_ID", "policy-1"),
            ("ADMIN_BRIDGE_POLICY_REVISION", "1"),
            ("ADMIN_BRIDGE_ENROLLED_MARKETS", "GLOBAL"),
            ("ADMIN_BRIDGE_ENROLLED_TENANTS", "cfmeet-first-party"),
        ];
        for (k, val) in extra {
            v.retain(|(kk, _)| kk != k);
            v.push((k, val));
        }
        v
    }

    struct V(Vec<(&'static str, &'static str)>);
    impl Env for V {
        fn get(&self, key: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
                .filter(|v| !v.is_empty())
        }
    }

    #[test]
    fn everything_is_off_by_default_and_nothing_else_is_required() {
        let c = Config::from_env(&[("CF_RUNTIME_ENVIRONMENT", "production")]).unwrap();
        assert!(!c.switches.enabled && !c.switches.writes_enabled && c.switches.modules.is_empty());
        assert!(c.security.is_none() && c.tls.is_none());
        assert!(c.ops_bind.ip().is_loopback());
    }

    #[test]
    fn enabled_needs_every_security_input_and_uses_the_agreed_defaults() {
        let c = Config::from_env(&V(enabled(&[]))).unwrap();
        let s = c.security.unwrap();
        assert!(s.strong_acr.contains(DEFAULT_STRONG_ACR) && s.strong_acr.len() == 1);
        assert_eq!(s.workload_acr, DEFAULT_WORKLOAD_ACR);
        assert_eq!(s.rate_limit_per_minute, 120);
        for var in [
            "DATABASE_URL",
            "ADMIN_BRIDGE_POLICY_REVISION",
            "ADMIN_BRIDGE_ENROLLED_TENANTS",
            "ADMIN_BRIDGE_CLIENT_CA_FILE",
        ] {
            let e = Config::from_env(&V(enabled(&[(var, "")]))).unwrap_err();
            assert!(
                matches!(e, ConfigError::Missing(v) | ConfigError::Invalid { var: v, .. } if v == var),
                "{var}"
            );
        }
    }

    #[test]
    fn the_workload_acr_can_never_be_strong() {
        let e = Config::from_env(&V(enabled(&[(
            "ADMIN_BRIDGE_STRONG_ACR",
            "urn:cosmicforge:acr:mfa,urn:cosmicforge:acr:workload",
        )])))
        .unwrap_err();
        assert_eq!(
            e,
            invalid(
                "ADMIN_BRIDGE_STRONG_ACR",
                "the workload acr can never be strong"
            )
        );
    }

    #[test]
    fn refuses_bad_values_without_echoing_them() {
        let e = Config::from_env(&V(enabled(&[(
            "ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX",
            "secret-looking-value",
        )])))
        .unwrap_err();
        assert!(!e.to_string().contains("secret-looking-value"));
        assert!(Config::from_env(&V(enabled(&[(
            "ADMIN_BRIDGE_OPS_BIND_ADDRESS",
            "0.0.0.0:8190"
        )])))
        .is_err());
        assert!(Config::from_env(&V(enabled(&[("CF_SERVICE_ID", "cfmeet.calling")]))).is_err());
        assert!(Config::from_env(&V(enabled(&[("ADMIN_BRIDGE_ENABLED", "yes")]))).is_err());
        assert!(Config::from_env(&V(enabled(&[("ADMIN_BRIDGE_MODULE_ACCOUNTS", "1")]))).is_err());
    }

    #[test]
    fn module_switches_are_read() {
        let c = Config::from_env(&V(enabled(&[("ADMIN_BRIDGE_MODULE_ACCOUNTS", "true")]))).unwrap();
        assert!(c.switches.module_on(Module::Accounts));
        assert!(!c.switches.module_on(Module::Pricing));
        assert!(c.switches.module_on(Module::Bridge));
    }
}

//! Configuration (environment variables; no secret has a default).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | Privacy Relay's OWN PostgreSQL database |
//! | `PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS` | no | pool size (default 10) |
//! | `PRIVACY_RELAY_BIND_ADDRESS` | no | default `127.0.0.1:8091` |
//! | `PRIVACY_RELAY_ENABLED` | no | feature kill switch, `true` / `false` (default `false`) |
//! | `PRIVACY_RELAY_WORKER_INTERVAL_MS` | no | expiry / enforcement sweep interval (default 1000) |
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.privacy-relay` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_AUTH_KEY_ID` | yes | `kid` of Privacy Relay's signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | yes | PKCS#8 PEM Ed25519 private key FILE |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | Privacy Relay's trust bundle |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300 (default 120) |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | 100–30000 (default 5000) |
//! | `CALLING_INTERNAL_BASE_URL` | no | Calling's internal API (active-call enforcement) |
//!
//! Without `CALLING_INTERNAL_BASE_URL` enforcement jobs stay durably PENDING (authority is
//! still revoked locally; nothing is falsely reported as applied).

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    services, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, TokenTtl, TrustPolicy,
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is required")]
    Missing { var: &'static str },
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var,
        reason: reason.into(),
    }
}

/// The Privacy Relay service identity.
pub fn privacy_relay_service_id() -> ServiceId {
    ServiceId::parse(services::PRIVACY_RELAY).expect("canonical identity")
}

/// Server-side feature kill switch (`PRIVACY_RELAY_ENABLED`). When disabled, new contexts,
/// new sessions, resumes and communication authorizations are refused; restrictive
/// operations (revoke, suspend, complete, updates, expiry, enforcement retry) keep working.
#[derive(Debug, Clone)]
pub struct FeatureFlag(Arc<AtomicBool>);

impl FeatureFlag {
    pub fn new(enabled: bool) -> Self {
        Self(Arc::new(AtomicBool::new(enabled)))
    }

    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Operational toggle (tests, future admin surface).
    pub fn set(&self, enabled: bool) {
        self.0.store(enabled, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub struct RelayConfig {
    database_url: String,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    pub enabled: bool,
    pub worker_interval: Duration,
    pub json_logs: bool,
}

impl RelayConfig {
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|k: &str| std::env::var(k).ok())
    }

    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let database_url = value("DATABASE_URL").ok_or(ConfigError::Missing {
            var: "DATABASE_URL",
        })?;
        if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
            return Err(invalid("DATABASE_URL", "must be a postgres:// URL"));
        }
        let database_max_connections = match value("PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS") {
            None => 10,
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=200).contains(n))
                .ok_or_else(|| {
                    invalid(
                        "PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS",
                        "must be an integer between 1 and 200",
                    )
                })?,
        };
        let bind_address = value("PRIVACY_RELAY_BIND_ADDRESS")
            .unwrap_or_else(|| "127.0.0.1:8091".to_string())
            .parse::<SocketAddr>()
            .map_err(|_| invalid("PRIVACY_RELAY_BIND_ADDRESS", "must be host:port"))?;
        let enabled = match value("PRIVACY_RELAY_ENABLED").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(invalid("PRIVACY_RELAY_ENABLED", "must be true or false")),
        };
        let worker_interval = match value("PRIVACY_RELAY_WORKER_INTERVAL_MS") {
            None => Duration::from_millis(1_000),
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|ms| (100..=60_000).contains(ms))
                .map(Duration::from_millis)
                .ok_or_else(|| {
                    invalid(
                        "PRIVACY_RELAY_WORKER_INTERVAL_MS",
                        "must be an integer between 100 and 60000",
                    )
                })?,
        };
        let json_logs = match value("LOG_FORMAT").as_deref() {
            None | Some("pretty") => false,
            Some("json") => true,
            Some(_) => return Err(invalid("LOG_FORMAT", "must be pretty or json")),
        };
        Ok(Self {
            database_url,
            database_max_connections,
            bind_address,
            enabled,
            worker_interval,
            json_logs,
        })
    }
}

/// Internal service authentication of Privacy Relay: its own identity, key and
/// receiver-controlled trust bundle, and the authenticated client for Calling.
#[derive(Clone)]
pub struct ServiceAuthRuntime {
    signer: Arc<ServiceTokenSigner>,
    verifier: Arc<ServiceTokenVerifier>,
    client: Arc<InternalServiceClient>,
}

impl std::fmt::Debug for ServiceAuthRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAuthRuntime")
            .field("issuer", self.signer.issuer())
            .field("client", &self.client)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("signer identity {0} is not cfmeet.privacy-relay")]
    WrongSignerIdentity(ServiceId),
    #[error("trust bundle receiver {0} is not cfmeet.privacy-relay")]
    WrongReceiver(ServiceId),
    #[error("signer and verifier run in different runtime environments")]
    EnvironmentMismatch,
}

impl ServiceAuthRuntime {
    pub fn new(
        signer: Arc<ServiceTokenSigner>,
        verifier: Arc<ServiceTokenVerifier>,
        client: Arc<InternalServiceClient>,
    ) -> Result<Self, RuntimeError> {
        let me = privacy_relay_service_id();
        if signer.issuer() != &me {
            return Err(RuntimeError::WrongSignerIdentity(signer.issuer().clone()));
        }
        if verifier.receiver() != &me {
            return Err(RuntimeError::WrongReceiver(verifier.receiver().clone()));
        }
        if signer.runtime_environment() != verifier.runtime_environment() {
            return Err(RuntimeError::EnvironmentMismatch);
        }
        Ok(Self {
            signer,
            verifier,
            client,
        })
    }

    pub fn signer(&self) -> &ServiceTokenSigner {
        &self.signer
    }

    pub fn verifier(&self) -> &ServiceTokenVerifier {
        &self.verifier
    }

    pub fn client(&self) -> &Arc<InternalServiceClient> {
        &self.client
    }
}

#[derive(Debug, Clone)]
pub struct ServiceAuthSettings {
    pub runtime_environment: RuntimeEnvironment,
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
    pub trust_bundle_file: PathBuf,
    pub token_ttl: TokenTtl,
    pub http_timeout: Duration,
    pub calling_base_url: Option<String>,
}

impl ServiceAuthSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|k: &str| std::env::var(k).ok())
    }

    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let required = |k: &'static str| value(k).ok_or(ConfigError::Missing { var: k });
        if value("CF_SERVICE_AUTH_PRIVATE_KEY").is_some() {
            return Err(invalid(
                "CF_SERVICE_AUTH_PRIVATE_KEY",
                "inline private keys are not accepted; mount the key as a secret file and set CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            ));
        }
        if let Some(id) = value("CF_SERVICE_ID") {
            if id != services::PRIVACY_RELAY {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.privacy-relay"));
            }
        }
        let runtime_environment = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|e| invalid("CF_RUNTIME_ENVIRONMENT", e.to_string()))?;
        let key_id = KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
            .map_err(|e| invalid("CF_SERVICE_AUTH_KEY_ID", e.to_string()))?;
        let private_key_file = PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?);
        let trust_bundle_file = PathBuf::from(required("CF_SERVICE_TRUST_BUNDLE_FILE")?);
        let token_ttl = match value("CF_SERVICE_TOKEN_TTL_SECONDS") {
            None => TokenTtl::default(),
            Some(v) => v
                .parse::<u64>()
                .ok()
                .and_then(|s| TokenTtl::seconds(s).ok())
                .ok_or_else(|| {
                    invalid(
                        "CF_SERVICE_TOKEN_TTL_SECONDS",
                        "must be an integer between 10 and 300",
                    )
                })?,
        };
        let http_timeout = match value("CF_INTERNAL_HTTP_TIMEOUT_MS") {
            None => Duration::from_millis(5_000),
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|ms| (100..=30_000).contains(ms))
                .map(Duration::from_millis)
                .ok_or_else(|| {
                    invalid(
                        "CF_INTERNAL_HTTP_TIMEOUT_MS",
                        "must be an integer between 100 and 30000",
                    )
                })?,
        };
        Ok(Self {
            runtime_environment,
            key_id,
            private_key_file,
            trust_bundle_file,
            token_ttl,
            http_timeout,
            calling_base_url: value("CALLING_INTERNAL_BASE_URL"),
        })
    }

    /// Loads the key and trust bundle; builds signer, verifier and the Calling client.
    pub fn load(&self, clock: SharedClock) -> Result<ServiceAuthRuntime, ConfigError> {
        let me = privacy_relay_service_id();
        let key =
            ServiceSigningKey::from_pkcs8_pem_file(self.key_id.clone(), &self.private_key_file)
                .map_err(|e| invalid("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", e.to_string()))?;
        let policy = TrustPolicy::from_file(&self.trust_bundle_file, &me, self.runtime_environment)
            .map_err(|e| invalid("CF_SERVICE_TRUST_BUNDLE_FILE", e.to_string()))?;
        let signer = Arc::new(ServiceTokenSigner::new(
            me,
            self.runtime_environment,
            key,
            self.token_ttl,
            clock.clone(),
        ));
        let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock));
        let mut targets = InternalTargets::new(self.runtime_environment);
        if let Some(url) = &self.calling_base_url {
            targets
                .insert(ServiceId::calling(), url)
                .map_err(|e| invalid("CALLING_INTERNAL_BASE_URL", e.to_string()))?;
        }
        let client = Arc::new(
            InternalServiceClient::new(signer.clone(), targets, self.http_timeout)
                .map_err(|e| invalid("CF_INTERNAL_HTTP_TIMEOUT_MS", e.to_string()))?,
        );
        ServiceAuthRuntime::new(signer, verifier, client)
            .map_err(|e| invalid("CF_SERVICE_ID", e.to_string()))
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
    fn feature_flag_defaults_to_disabled_and_rejects_garbage() {
        let base = [("DATABASE_URL", "postgres://u:p@127.0.0.1/db")];
        assert!(!RelayConfig::from_lookup(&lookup(&base)).unwrap().enabled);
        let mut on = base.to_vec();
        on.push(("PRIVACY_RELAY_ENABLED", "true"));
        assert!(RelayConfig::from_lookup(&lookup(&on)).unwrap().enabled);
        let mut bad = base.to_vec();
        bad.push(("PRIVACY_RELAY_ENABLED", "yes"));
        assert!(RelayConfig::from_lookup(&lookup(&bad)).is_err());
        assert!(RelayConfig::from_lookup(&lookup(&[])).is_err());
    }

    #[test]
    fn service_identity_inputs_are_required_and_strict() {
        let base = [
            ("CF_RUNTIME_ENVIRONMENT", "staging"),
            ("CF_SERVICE_AUTH_KEY_ID", "privacy-relay-staging-1"),
            ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", "/run/secrets/relay.pem"),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", "/etc/cfmeet/trust.json"),
        ];
        assert!(ServiceAuthSettings::from_lookup(&lookup(&base)).is_ok());
        for (missing, _) in base {
            let pairs: Vec<_> = base
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            assert!(
                ServiceAuthSettings::from_lookup(&lookup(&pairs)).is_err(),
                "{missing}"
            );
        }
        let mut wrong = base.to_vec();
        wrong.push(("CF_SERVICE_ID", "cfmeet.calling"));
        assert!(ServiceAuthSettings::from_lookup(&lookup(&wrong)).is_err());
        let mut inline = base.to_vec();
        inline.push(("CF_SERVICE_AUTH_PRIVATE_KEY", "-----BEGIN PRIVATE KEY-----"));
        assert!(ServiceAuthSettings::from_lookup(&lookup(&inline)).is_err());
    }
}

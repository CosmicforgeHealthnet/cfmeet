//! Configuration (environment variables; no secret has a default and no secret is ever
//! accepted inline).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `DATABASE_URL` | yes | Telephony's OWN PostgreSQL database |
//! | `TELEPHONY_DATABASE_MAX_CONNECTIONS` | no | pool size (default 10) |
//! | `TELEPHONY_BIND_ADDRESS` | no | default `127.0.0.1:8092` |
//! | `TELEPHONY_ENABLED` | no | service kill switch (default `false`) |
//! | `PUBLIC_OUTBOUND_CALLING_ENABLED` | no | paid outbound origination (default `false`) |
//! | `PUBLIC_INBOUND_CALLING_ENABLED` | no | inbound public calls (default `false`) |
//! | `CALLBACK_RELAY_ENABLED` | no | callback relay legs (default `false`) |
//! | `ENTERPRISE_SIP_ENABLED` | no | Part F internal enterprise SIP devices: new dialogs, answer / prompt / capture (default `false`) |
//! | `TELEPHONY_WORKER_INTERVAL_MS` | no | command / outbox / reconciliation cadence (default 500) |
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.telephony` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_AUTH_KEY_ID` | yes | `kid` of Telephony's signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | yes | PKCS#8 PEM Ed25519 private key FILE |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | Telephony's trust bundle (receiver-authoritative) |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300 (default 120) |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | 100–30000 (default 5000) |
//! | `CALLING_INTERNAL_BASE_URL` | no | Calling's internal API (normalized events, inbound offers) |
//! | `PRIVACY_RELAY_INTERNAL_BASE_URL` | no | Privacy Relay's internal API (contact resolution, inbound mapping) |
//!
//! Disabling Telephony or public calling never affects application RTC (Calling serves it
//! without Telephony). Without `PRIVACY_RELAY_INTERNAL_BASE_URL` every protected
//! origination fails closed before any raw destination exists; without
//! `CALLING_INTERNAL_BASE_URL` normalized events stay durably in the outbox.

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

pub(crate) fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var,
        reason: reason.into(),
    }
}

pub fn telephony_service_id() -> ServiceId {
    ServiceId::parse(services::TELEPHONY).expect("canonical identity")
}

/// An operational kill switch (shared, atomically toggled).
#[derive(Debug, Clone)]
pub struct FeatureFlag(Arc<AtomicBool>);

impl FeatureFlag {
    pub fn new(enabled: bool) -> Self {
        Self(Arc::new(AtomicBool::new(enabled)))
    }

    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn set(&self, enabled: bool) {
        self.0.store(enabled, Ordering::SeqCst);
    }
}

/// The four high-risk public-network switches. Each only ever stops NEW paid effects:
/// terminating, reconciling and finalizing always run.
#[derive(Debug, Clone)]
pub struct TelecomFlags {
    pub telephony: FeatureFlag,
    pub public_outbound: FeatureFlag,
    pub public_inbound: FeatureFlag,
    pub callback_relay: FeatureFlag,
    /// Part F: internal enterprise SIP dialogs (never paid, never public). Stops NEW
    /// dialogs and media effects only; hanging up / reconciling always run.
    pub enterprise_sip: FeatureFlag,
}

impl TelecomFlags {
    pub fn new(telephony: bool, outbound: bool, inbound: bool, callback: bool) -> Self {
        Self {
            telephony: FeatureFlag::new(telephony),
            public_outbound: FeatureFlag::new(outbound),
            public_inbound: FeatureFlag::new(inbound),
            callback_relay: FeatureFlag::new(callback),
            enterprise_sip: FeatureFlag::new(false),
        }
    }

    /// With the enterprise SIP switch set.
    pub fn with_enterprise_sip(self, enabled: bool) -> Self {
        self.enterprise_sip.set(enabled);
        self
    }

    pub fn all_off() -> Self {
        Self::new(false, false, false, false)
    }
}

fn flag(get: &impl Fn(&str) -> Option<String>, var: &'static str) -> Result<bool, ConfigError> {
    match get(var).map(|v| v.trim().to_string()).as_deref() {
        None | Some("") | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(invalid(var, "must be true or false")),
    }
}

#[derive(Debug, Clone)]
pub struct TelephonyConfig {
    database_url: String,
    pub database_max_connections: u32,
    pub bind_address: SocketAddr,
    /// Optional separate listener for `GET /metrics` (`TELEPHONY_METRICS_ADDRESS`).
    pub metrics_address: Option<SocketAddr>,
    pub telephony_enabled: bool,
    pub public_outbound_enabled: bool,
    pub public_inbound_enabled: bool,
    pub callback_relay_enabled: bool,
    pub enterprise_sip_enabled: bool,
    pub worker_interval: Duration,
    pub json_logs: bool,
}

impl TelephonyConfig {
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn flags(&self) -> TelecomFlags {
        TelecomFlags::new(
            self.telephony_enabled,
            self.public_outbound_enabled,
            self.public_inbound_enabled,
            self.callback_relay_enabled,
        )
        .with_enterprise_sip(self.enterprise_sip_enabled)
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
        let database_max_connections = match value("TELEPHONY_DATABASE_MAX_CONNECTIONS") {
            None => 10,
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=200).contains(n))
                .ok_or_else(|| {
                    invalid(
                        "TELEPHONY_DATABASE_MAX_CONNECTIONS",
                        "must be an integer between 1 and 200",
                    )
                })?,
        };
        let bind_address = value("TELEPHONY_BIND_ADDRESS")
            .unwrap_or_else(|| "127.0.0.1:8092".to_string())
            .parse::<SocketAddr>()
            .map_err(|_| invalid("TELEPHONY_BIND_ADDRESS", "must be host:port"))?;
        let metrics_address = value("TELEPHONY_METRICS_ADDRESS")
            .map(|v| v.parse::<SocketAddr>())
            .transpose()
            .map_err(|_| invalid("TELEPHONY_METRICS_ADDRESS", "must be host:port"))?;
        let worker_interval = match value("TELEPHONY_WORKER_INTERVAL_MS") {
            None => Duration::from_millis(500),
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|ms| (50..=60_000).contains(ms))
                .map(Duration::from_millis)
                .ok_or_else(|| {
                    invalid(
                        "TELEPHONY_WORKER_INTERVAL_MS",
                        "must be an integer between 50 and 60000",
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
            metrics_address,
            telephony_enabled: flag(get, "TELEPHONY_ENABLED")?,
            public_outbound_enabled: flag(get, "PUBLIC_OUTBOUND_CALLING_ENABLED")?,
            public_inbound_enabled: flag(get, "PUBLIC_INBOUND_CALLING_ENABLED")?,
            callback_relay_enabled: flag(get, "CALLBACK_RELAY_ENABLED")?,
            enterprise_sip_enabled: flag(get, "ENTERPRISE_SIP_ENABLED")?,
            worker_interval,
            json_logs,
        })
    }
}

/// Internal service authentication of Telephony: its own identity, key and
/// receiver-controlled trust bundle, and the authenticated clients for Calling and
/// Privacy Relay.
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
    #[error("signer identity {0} is not cfmeet.telephony")]
    WrongSignerIdentity(ServiceId),
    #[error("trust bundle receiver {0} is not cfmeet.telephony")]
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
        let me = telephony_service_id();
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
    pub privacy_relay_base_url: Option<String>,
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
            if id != services::TELEPHONY {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.telephony"));
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
            privacy_relay_base_url: value("PRIVACY_RELAY_INTERNAL_BASE_URL"),
        })
    }

    /// Loads the key and trust bundle; builds signer, verifier and the internal client.
    pub fn load(&self, clock: SharedClock) -> Result<ServiceAuthRuntime, ConfigError> {
        let me = telephony_service_id();
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
        if let Some(url) = &self.privacy_relay_base_url {
            targets
                .insert(ServiceId::privacy_relay(), url)
                .map_err(|e| invalid("PRIVACY_RELAY_INTERNAL_BASE_URL", e.to_string()))?;
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
    fn every_public_network_flag_defaults_to_off() {
        let base = [("DATABASE_URL", "postgres://u:p@127.0.0.1/db")];
        let c = TelephonyConfig::from_lookup(&lookup(&base)).unwrap();
        assert!(!c.telephony_enabled);
        assert!(!c.public_outbound_enabled);
        assert!(!c.public_inbound_enabled);
        assert!(!c.callback_relay_enabled);
        for var in [
            "TELEPHONY_ENABLED",
            "PUBLIC_OUTBOUND_CALLING_ENABLED",
            "PUBLIC_INBOUND_CALLING_ENABLED",
            "CALLBACK_RELAY_ENABLED",
        ] {
            let mut on = base.to_vec();
            on.push((var, "true"));
            assert!(TelephonyConfig::from_lookup(&lookup(&on)).is_ok());
            let mut bad = base.to_vec();
            bad.push((var, "1"));
            assert!(
                TelephonyConfig::from_lookup(&lookup(&bad)).is_err(),
                "{var}"
            );
        }
        assert!(TelephonyConfig::from_lookup(&lookup(&[])).is_err());
        assert!(TelephonyConfig::from_lookup(&lookup(&[("DATABASE_URL", "mysql://x")])).is_err());
    }

    #[test]
    fn service_auth_requires_files_and_refuses_inline_keys() {
        let base = [
            ("CF_RUNTIME_ENVIRONMENT", "local"),
            ("CF_SERVICE_AUTH_KEY_ID", "telephony-local-1"),
            ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", "/secrets/k.pem"),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", "/secrets/trust.json"),
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
        inline.push(("CF_SERVICE_AUTH_PRIVATE_KEY", "inline-key-material"));
        assert!(ServiceAuthSettings::from_lookup(&lookup(&inline)).is_err());
    }
}

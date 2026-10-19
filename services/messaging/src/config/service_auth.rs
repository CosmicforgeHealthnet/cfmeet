//! Internal service authentication of Messaging (Part B §11 protocol).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.messaging` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_AUTH_KEY_ID` | yes | `kid` of Messaging's signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | yes | PKCS#8 PEM Ed25519 private key FILE |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | Messaging's trust bundle (receiver-authoritative) |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300 (default 120) |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | outbound timeout (100–30000, default 5000) |
//! | `CF_PLATFORM_ACCESS_BASE_URL` | yes (server) | Platform Access, the identity authority |
//! | `CF_SMS_GATEWAY_BASE_URL` | no | SMS Gateway (SMS Bridge transport); unset: bridges are refused |
//!
//! Inbound, every `/internal/v1` route requires a service token whose issuer Messaging's
//! own trust bundle grants the route's permission. The internal operations (Part V H) are
//! `messaging-application.delegate` and `messaging-meeting.sync` (Meeting Service only),
//! `messaging-context-authority.apply` (Privacy Relay only) and
//! `messaging-sms-bridge.deliver` (SMS Gateway only); the service-auth REGISTRY refuses a
//! bundle granting any of them to another issuer. Outbound, Messaging calls Platform Access
//! (credential verification, principal resolution, identity context, the identity change
//! feed). There is no network-location trust, no inline key and no default key.

use std::{fmt, path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    services, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, TokenTtl, TrustPolicy,
};

use super::ConfigError;

pub fn messaging_service_id() -> ServiceId {
    ServiceId::parse(services::MESSAGING).expect("canonical identity")
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::invalid(var, reason.into())
}

/// Messaging's signer, verifier and outbound client.
#[derive(Clone)]
pub struct ServiceAuthRuntime {
    signer: Arc<ServiceTokenSigner>,
    verifier: Arc<ServiceTokenVerifier>,
    client: Arc<InternalServiceClient>,
}

impl fmt::Debug for ServiceAuthRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAuthRuntime")
            .field("issuer", self.signer.issuer())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("signer identity {0} is not cfmeet.messaging")]
    WrongSignerIdentity(ServiceId),
    #[error("trust bundle receiver {0} is not cfmeet.messaging")]
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
        let me = messaging_service_id();
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

    pub fn client(&self) -> Arc<InternalServiceClient> {
        self.client.clone()
    }
}

/// Base-URL variables of the services Messaging calls.
const TARGETS: &[(&str, &str)] = &[
    ("CF_PLATFORM_ACCESS_BASE_URL", services::PLATFORM_ACCESS),
    // Part V H SMS Bridge: carrier transport (sms-bridge.submit).
    ("CF_SMS_GATEWAY_BASE_URL", services::SMS_GATEWAY),
];

#[derive(Debug, Clone)]
pub struct ServiceAuthSettings {
    pub runtime_environment: RuntimeEnvironment,
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
    pub trust_bundle_file: PathBuf,
    pub token_ttl: TokenTtl,
    pub http_timeout: Duration,
    pub targets: Vec<(ServiceId, String)>,
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
            if id != services::MESSAGING {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.messaging"));
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
        let targets = TARGETS
            .iter()
            .filter_map(|(var, service)| {
                value(var).map(|url| (ServiceId::parse(service).expect("canonical"), url))
            })
            .collect();
        Ok(Self {
            runtime_environment,
            key_id,
            private_key_file,
            trust_bundle_file,
            token_ttl,
            http_timeout,
            targets,
        })
    }

    /// Whether Platform Access (required by the server) is configured.
    pub fn has_platform_access(&self) -> bool {
        self.targets
            .iter()
            .any(|(s, _)| s.as_str() == services::PLATFORM_ACCESS)
    }

    /// Loads the key and trust bundle and builds signer, verifier and client; fails closed
    /// on anything malformed.
    pub fn load(&self, clock: SharedClock) -> Result<ServiceAuthRuntime, ConfigError> {
        let me = messaging_service_id();
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
        for (service, url) in &self.targets {
            targets
                .insert(service.clone(), url)
                .map_err(|e| invalid("CF_*_BASE_URL", e.to_string()))?;
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

    const BASE: &[(&str, &str)] = &[
        ("CF_RUNTIME_ENVIRONMENT", "staging"),
        ("CF_SERVICE_AUTH_KEY_ID", "messaging-staging-1"),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            "/run/secrets/messaging.pem",
        ),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", "/etc/cfmeet/trust.json"),
    ];

    #[test]
    fn every_identity_input_is_required_and_inline_keys_or_foreign_ids_are_refused() {
        let settings = ServiceAuthSettings::from_lookup(&lookup(BASE)).unwrap();
        assert!(!settings.has_platform_access());
        for missing in BASE.iter().map(|(k, _)| *k) {
            let pairs: Vec<_> = BASE
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            assert!(
                ServiceAuthSettings::from_lookup(&lookup(&pairs)).is_err(),
                "{missing}"
            );
        }
        for (var, value) in [
            ("CF_SERVICE_ID", "cfmeet.calling"),
            ("CF_SERVICE_AUTH_PRIVATE_KEY", "inline"),
            ("CF_SERVICE_TOKEN_TTL_SECONDS", "301"),
            ("CF_INTERNAL_HTTP_TIMEOUT_MS", "5"),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            assert!(
                ServiceAuthSettings::from_lookup(&lookup(&pairs)).is_err(),
                "{var}"
            );
        }
        let mut with_pa = BASE.to_vec();
        with_pa.push((
            "CF_PLATFORM_ACCESS_BASE_URL",
            "https://platform-access.internal",
        ));
        assert!(ServiceAuthSettings::from_lookup(&lookup(&with_pa))
            .unwrap()
            .has_platform_access());
    }
}

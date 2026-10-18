//! Internal service authentication of Platform Access (Part B §11 protocol).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.platform-access` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_AUTH_KEY_ID` | yes | `kid` of Platform Access's signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | yes | PKCS#8 PEM Ed25519 private key FILE |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | Platform Access's trust bundle (receiver-authoritative) |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300 (default 120) |
//!
//! Platform Access calls no other service; it only receives. Every route but `/health`
//! and `/ready` is under `/internal/v1` and requires a valid service token whose issuer
//! the trust bundle grants the route's permission. There is no network-location trust,
//! no inline key and no default key.

use std::{fmt, path::PathBuf, sync::Arc};

use cf_service_auth::{
    services, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, TokenTtl, TrustPolicy,
};

use super::ConfigError;

pub fn platform_access_service_id() -> ServiceId {
    ServiceId::parse(services::PLATFORM_ACCESS).expect("canonical identity")
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::invalid(var, reason.into())
}

/// Platform Access's verifier (and signer, for symmetry with the other services and for
/// future outbound calls).
#[derive(Clone)]
pub struct ServiceAuthRuntime {
    signer: Arc<ServiceTokenSigner>,
    verifier: Arc<ServiceTokenVerifier>,
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
    #[error("signer identity {0} is not cfmeet.platform-access")]
    WrongSignerIdentity(ServiceId),
    #[error("trust bundle receiver {0} is not cfmeet.platform-access")]
    WrongReceiver(ServiceId),
    #[error("signer and verifier run in different runtime environments")]
    EnvironmentMismatch,
}

impl ServiceAuthRuntime {
    pub fn new(
        signer: Arc<ServiceTokenSigner>,
        verifier: Arc<ServiceTokenVerifier>,
    ) -> Result<Self, RuntimeError> {
        let me = platform_access_service_id();
        if signer.issuer() != &me {
            return Err(RuntimeError::WrongSignerIdentity(signer.issuer().clone()));
        }
        if verifier.receiver() != &me {
            return Err(RuntimeError::WrongReceiver(verifier.receiver().clone()));
        }
        if signer.runtime_environment() != verifier.runtime_environment() {
            return Err(RuntimeError::EnvironmentMismatch);
        }
        Ok(Self { signer, verifier })
    }

    pub fn signer(&self) -> &ServiceTokenSigner {
        &self.signer
    }

    pub fn verifier(&self) -> &ServiceTokenVerifier {
        &self.verifier
    }
}

#[derive(Debug, Clone)]
pub struct ServiceAuthSettings {
    pub runtime_environment: RuntimeEnvironment,
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
    pub trust_bundle_file: PathBuf,
    pub token_ttl: TokenTtl,
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
            if id != services::PLATFORM_ACCESS {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.platform-access"));
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
        Ok(Self {
            runtime_environment,
            key_id,
            private_key_file,
            trust_bundle_file,
            token_ttl,
        })
    }

    /// Loads the key and trust bundle; fails closed on anything malformed.
    pub fn load(&self, clock: SharedClock) -> Result<ServiceAuthRuntime, ConfigError> {
        let me = platform_access_service_id();
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
        ServiceAuthRuntime::new(signer, verifier)
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
    fn requires_files_and_refuses_inline_keys_or_foreign_identity() {
        let base = [
            ("CF_RUNTIME_ENVIRONMENT", "local"),
            ("CF_SERVICE_AUTH_KEY_ID", "platform-access-local-1"),
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

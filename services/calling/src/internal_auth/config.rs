//! Internal service authentication configuration.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.calling` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` (no default) |
//! | `CF_SERVICE_AUTH_KEY_ID` | yes | `kid` of Calling's current signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | yes | PKCS#8 PEM Ed25519 private key (secret file) |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | Calling's trust bundle (contract `cfmeet.platform.service-trust-bundle`) |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300, default 120 |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | 100–30000, default 5000 |
//! | `CF_PRIVACY_RELAY_BASE_URL`, `CF_TELEPHONY_BASE_URL` | no | authenticated API base URLs |
//! | `CF_PLATFORM_ACCESS_BASE_URL` | yes (server) | Platform Access, the identity authority (ADR 0032) |
//! | `CF_NUMBERING_BASE_URL` | no | Numbering (Part IV L): DCN grants; unset = DCN calls fail closed |
//! | `CF_DEVELOPER_PLATFORM_BASE_URL` | with `CALLING_PUBLIC_EVENTS_DESTINATION` `developer-platform` / `both` | the Developer Platform's central event ingestion (`developer-event.publish`, Part VI §53) |
//!
//! There is no default key, no inline-key variable and no shared secret. A missing or
//! malformed key or bundle stops the service. Error messages never contain key material.

use std::{path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, TokenTtl, TrustPolicy,
};

use super::{calling_service_id, ServiceAuthRuntime};
use crate::config::ConfigError;

/// Base-URL variables for the internal services Calling may call.
const TARGETS: &[(&str, &str)] = &[
    (
        "CF_PRIVACY_RELAY_BASE_URL",
        cf_service_auth::services::PRIVACY_RELAY,
    ),
    (
        "CF_TELEPHONY_BASE_URL",
        cf_service_auth::services::TELEPHONY,
    ),
    (
        "CF_PLATFORM_ACCESS_BASE_URL",
        cf_service_auth::services::PLATFORM_ACCESS,
    ),
    // Part IV Workstream L: DCN routing grants and caller identity (optional; without it
    // every DCN operation fails closed).
    (
        "CF_NUMBERING_BASE_URL",
        cf_service_auth::services::NUMBERING,
    ),
    // Part VI §53: Calling's public events to the Developer Platform's central event
    // ingestion (developer-event.publish), when CALLING_PUBLIC_EVENTS_DESTINATION says so.
    (
        "CF_DEVELOPER_PLATFORM_BASE_URL",
        cf_service_auth::services::DEVELOPER_PLATFORM,
    ),
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

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::invalid(var, reason.into())
}

impl ServiceAuthSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
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
            if id != calling_service_id().as_str() {
                return Err(invalid(
                    "CF_SERVICE_ID",
                    format!("must be {}", calling_service_id()),
                ));
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
                value(var).map(|url| (ServiceId::parse(service).expect("canonical identity"), url))
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

    /// Loads the key and trust bundle and builds signer, verifier and client.
    pub fn load(&self, clock: SharedClock) -> Result<ServiceAuthRuntime, ConfigError> {
        let calling = calling_service_id();
        let key =
            ServiceSigningKey::from_pkcs8_pem_file(self.key_id.clone(), &self.private_key_file)
                .map_err(|e| invalid("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", e.to_string()))?;
        let policy =
            TrustPolicy::from_file(&self.trust_bundle_file, &calling, self.runtime_environment)
                .map_err(|e| invalid("CF_SERVICE_TRUST_BUNDLE_FILE", e.to_string()))?;
        let signer = Arc::new(ServiceTokenSigner::new(
            calling,
            self.runtime_environment,
            key,
            self.token_ttl,
            clock.clone(),
        ));
        let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock));
        let mut targets = InternalTargets::new(self.runtime_environment);
        for (service, url) in &self.targets {
            targets.insert(service.clone(), url).map_err(|e| {
                invalid(
                    "CF_PRIVACY_RELAY_BASE_URL / CF_TELEPHONY_BASE_URL",
                    e.to_string(),
                )
            })?;
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
        ("CF_SERVICE_AUTH_KEY_ID", "calling-staging-1"),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            "/run/secrets/calling.pem",
        ),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", "/etc/cfmeet/trust.json"),
    ];

    #[test]
    fn every_identity_input_is_required_without_defaults() {
        assert!(ServiceAuthSettings::from_lookup(lookup(BASE)).is_ok());
        for missing in BASE.iter().map(|(k, _)| *k) {
            let pairs: Vec<_> = BASE
                .iter()
                .filter(|(k, _)| *k != missing)
                .copied()
                .collect();
            assert!(
                matches!(ServiceAuthSettings::from_lookup(lookup(&pairs)), Err(ConfigError::Missing { var }) if var == missing),
                "{missing}"
            );
        }
    }

    #[test]
    fn rejects_wrong_identity_inline_keys_and_bad_values() {
        let with = |extra: (&'static str, &'static str)| {
            let mut pairs = BASE.to_vec();
            pairs.push(extra);
            ServiceAuthSettings::from_lookup(lookup(&pairs))
        };
        assert!(with(("CF_SERVICE_ID", "cfmeet.calling")).is_ok());
        assert!(with(("CF_SERVICE_ID", "cfmeet.telephony")).is_err());
        assert!(with(("CF_SERVICE_AUTH_PRIVATE_KEY", "-----BEGIN PRIVATE KEY-----")).is_err());
        assert!(with(("CF_SERVICE_TOKEN_TTL_SECONDS", "301")).is_err());
        assert!(with(("CF_SERVICE_TOKEN_TTL_SECONDS", "3600")).is_err());
        assert!(with(("CF_SERVICE_TOKEN_TTL_SECONDS", "60")).is_ok());
        assert!(with(("CF_INTERNAL_HTTP_TIMEOUT_MS", "0")).is_err());
        let mut pairs = BASE.to_vec();
        pairs[0] = ("CF_RUNTIME_ENVIRONMENT", "prod");
        assert!(ServiceAuthSettings::from_lookup(lookup(&pairs)).is_err());
    }

    #[test]
    fn missing_key_or_bundle_files_fail_closed() {
        let settings = ServiceAuthSettings::from_lookup(lookup(BASE)).unwrap();
        let err = settings
            .load(Arc::new(cf_service_auth::SystemClock))
            .unwrap_err()
            .to_string();
        assert!(err.contains("CF_SERVICE_AUTH_PRIVATE_KEY_FILE"), "{err}");
    }
}

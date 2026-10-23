//! Internal service authentication of the API Gateway (Part B §11 protocol).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `CF_SERVICE_ID` | no | if set, must be `cfmeet.api-gateway` |
//! | `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging` or `production` |
//! | `CF_SERVICE_AUTH_KEY_ID` | yes | `kid` of the gateway's signing key |
//! | `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | yes | PKCS#8 PEM Ed25519 private key FILE |
//! | `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300 (default 60: forwarded contexts are short-lived) |
//! | `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | Platform Access / upstream transport timeout (100–30000, default 5000) |
//! | `CF_PLATFORM_ACCESS_BASE_URL` | yes | Platform Access: every customer credential is verified there |
//! | `CF_DEVELOPER_PLATFORM_BASE_URL` | no | the Developer Platform upstream (unset: its routes are SERVICE_UNAVAILABLE) |
//!
//! The gateway SIGNS only: it receives no internal call, so it has no trust bundle and no
//! `/internal` surface. Every forwarded request carries a fresh token for exactly one
//! upstream audience with the delegated context derived from Platform Access's authority
//! chain. There is no inline key and no default key.

use std::{fmt, path::PathBuf, sync::Arc, time::Duration};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    services, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    SharedClock, TokenTtl,
};

use super::ConfigError;

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::invalid(var, reason.into())
}

/// The gateway's signer and outbound client.
#[derive(Clone)]
pub struct ServiceAuthRuntime {
    signer: Arc<ServiceTokenSigner>,
    client: Arc<InternalServiceClient>,
    /// Base URLs of the customer-facing upstreams (validated like internal targets).
    upstreams: Arc<std::collections::BTreeMap<String, url::Url>>,
}

impl fmt::Debug for ServiceAuthRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAuthRuntime")
            .field("issuer", self.signer.issuer())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("signer identity {0} is not cfmeet.api-gateway")]
pub struct WrongSignerIdentity(ServiceId);

impl ServiceAuthRuntime {
    pub fn new(
        signer: Arc<ServiceTokenSigner>,
        client: Arc<InternalServiceClient>,
    ) -> Result<Self, WrongSignerIdentity> {
        if signer.issuer() != &ServiceId::api_gateway() {
            return Err(WrongSignerIdentity(signer.issuer().clone()));
        }
        Ok(Self {
            signer,
            client,
            upstreams: Arc::new(Default::default()),
        })
    }

    /// Declares the base URLs of customer-facing upstreams.
    pub fn with_upstreams(
        mut self,
        upstreams: std::collections::BTreeMap<String, url::Url>,
    ) -> Self {
        self.upstreams = Arc::new(upstreams);
        self
    }

    /// The base URL of `service`, when configured.
    pub fn upstream_base(&self, service: &ServiceId) -> Option<&url::Url> {
        self.upstreams.get(service.as_str())
    }

    pub fn upstream_services(&self) -> impl Iterator<Item = &String> {
        self.upstreams.keys()
    }

    pub fn signer(&self) -> &ServiceTokenSigner {
        &self.signer
    }

    pub fn client(&self) -> Arc<InternalServiceClient> {
        self.client.clone()
    }
}

/// Base-URL variables of the services the gateway calls.
const TARGETS: &[(&str, &str)] = &[
    ("CF_PLATFORM_ACCESS_BASE_URL", services::PLATFORM_ACCESS),
    (
        "CF_DEVELOPER_PLATFORM_BASE_URL",
        services::DEVELOPER_PLATFORM,
    ),
    // Part VI §51: the owning services of the public route catalogue.
    ("CF_CALLING_BASE_URL", services::CALLING),
    ("CF_MESSAGING_BASE_URL", services::MESSAGING),
    ("CF_NUMBERING_BASE_URL", services::NUMBERING),
    ("CF_SMS_GATEWAY_BASE_URL", services::SMS_GATEWAY),
    ("CF_MEETING_SERVICE_BASE_URL", services::MEETING_SERVICE),
];

#[derive(Debug, Clone)]
pub struct ServiceAuthSettings {
    pub runtime_environment: RuntimeEnvironment,
    pub key_id: KeyId,
    pub private_key_file: PathBuf,
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
            if id != services::API_GATEWAY {
                return Err(invalid("CF_SERVICE_ID", "must be cfmeet.api-gateway"));
            }
        }
        let runtime_environment = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|e| invalid("CF_RUNTIME_ENVIRONMENT", e.to_string()))?;
        let key_id = KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
            .map_err(|e| invalid("CF_SERVICE_AUTH_KEY_ID", e.to_string()))?;
        let private_key_file = PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?);
        let token_ttl = match value("CF_SERVICE_TOKEN_TTL_SECONDS") {
            None => TokenTtl::seconds(60).expect("valid default"),
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
        let targets: Vec<(ServiceId, String)> = TARGETS
            .iter()
            .filter_map(|(var, service)| {
                value(var).map(|url| (ServiceId::parse(service).expect("canonical"), url))
            })
            .collect();
        if !targets
            .iter()
            .any(|(s, _)| s.as_str() == services::PLATFORM_ACCESS)
        {
            return Err(ConfigError::Missing {
                var: "CF_PLATFORM_ACCESS_BASE_URL",
            });
        }
        Ok(Self {
            runtime_environment,
            key_id,
            private_key_file,
            token_ttl,
            http_timeout,
            targets,
        })
    }

    /// Loads the key and builds signer and client; fails closed on anything malformed.
    pub fn load(&self, clock: SharedClock) -> Result<ServiceAuthRuntime, ConfigError> {
        let key =
            ServiceSigningKey::from_pkcs8_pem_file(self.key_id.clone(), &self.private_key_file)
                .map_err(|e| invalid("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", e.to_string()))?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::api_gateway(),
            self.runtime_environment,
            key,
            self.token_ttl,
            clock,
        ));
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
        let mut upstreams = std::collections::BTreeMap::new();
        for (service, url) in &self.targets {
            if crate::routing::CUSTOMER_UPSTREAMS.contains(&service.as_str()) {
                let parsed =
                    url::Url::parse(url).map_err(|_| invalid("CF_*_BASE_URL", "invalid URL"))?;
                upstreams.insert(service.as_str().to_string(), parsed);
            }
        }
        Ok(ServiceAuthRuntime::new(signer, client)
            .map_err(|e| invalid("CF_SERVICE_ID", e.to_string()))?
            .with_upstreams(upstreams))
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
        ("CF_SERVICE_AUTH_KEY_ID", "api-gateway-staging-1"),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            "/run/secrets/api-gateway.pem",
        ),
        (
            "CF_PLATFORM_ACCESS_BASE_URL",
            "https://platform-access.internal",
        ),
    ];

    #[test]
    fn identity_inputs_and_platform_access_are_required() {
        let settings = ServiceAuthSettings::from_lookup(&lookup(BASE)).unwrap();
        assert_eq!(settings.token_ttl.get(), 60);
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
            ("CF_SERVICE_ID", "cfmeet.platform-access"),
            ("CF_SERVICE_AUTH_PRIVATE_KEY", "inline"),
            ("CF_SERVICE_TOKEN_TTL_SECONDS", "301"),
        ] {
            let mut pairs = BASE.to_vec();
            pairs.push((var, value));
            assert!(
                ServiceAuthSettings::from_lookup(&lookup(&pairs)).is_err(),
                "{var}"
            );
        }
    }
}

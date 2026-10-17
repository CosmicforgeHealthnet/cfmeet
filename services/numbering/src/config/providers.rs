//! Numbering provider adapters (Part IV Workstream G).
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `NUMBERING_PROVIDERS` | no | comma-separated adapters; `synthetic` = the deterministic test provider (refused in staging / production). Empty: no provider operations |
//! | `NUMBERING_SYNTHETIC_PROVIDER_CALLBACK_SECRET` | with `synthetic` | ≥ 32-character secret its callbacks are signed with (test material; never printed) |
//!
//! Real carrier adapters are later workstreams; each will add its own variables here and
//! register through the same [`ProviderRegistry`] restrictions.

use std::{fmt, sync::Arc};

use cf_service_auth::RuntimeEnvironment;
use secrecy::{ExposeSecret, SecretString};

use super::ConfigError;
use crate::{
    domain::SharedClock,
    providers::{deterministic::DeterministicProvider, registry::ProviderRegistry},
};

pub struct ProviderConfig {
    pub adapters: Vec<String>,
    synthetic_secret: Option<SecretString>,
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("adapters", &self.adapters)
            .field(
                "synthetic_secret",
                &self.synthetic_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl ProviderConfig {
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        runtime: RuntimeEnvironment,
    ) -> Result<Self, ConfigError> {
        let get = |key: &str| get(key).filter(|v| !v.trim().is_empty());
        let mut adapters = Vec::new();
        for a in get("NUMBERING_PROVIDERS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            match a {
                "synthetic"
                    if matches!(
                        runtime,
                        RuntimeEnvironment::Staging | RuntimeEnvironment::Production
                    ) =>
                {
                    return Err(ConfigError::invalid(
                        "NUMBERING_PROVIDERS",
                        "the synthetic test provider is refused in staging / production",
                    ))
                }
                "synthetic" => adapters.push(a.to_string()),
                _ => {
                    return Err(ConfigError::invalid(
                        "NUMBERING_PROVIDERS",
                        "unknown provider adapter",
                    ))
                }
            }
        }
        adapters.dedup();
        let synthetic_secret =
            get("NUMBERING_SYNTHETIC_PROVIDER_CALLBACK_SECRET").map(SecretString::from);
        if adapters.iter().any(|a| a == "synthetic")
            && synthetic_secret
                .as_ref()
                .is_none_or(|s| s.expose_secret().len() < 32)
        {
            return Err(ConfigError::invalid(
                "NUMBERING_SYNTHETIC_PROVIDER_CALLBACK_SECRET",
                "the synthetic provider needs a callback secret of at least 32 characters",
            ));
        }
        Ok(Self {
            adapters,
            synthetic_secret,
        })
    }

    /// The registry of configured adapters (`None` when none is configured).
    pub fn registry(
        &self,
        runtime: RuntimeEnvironment,
        clock: SharedClock,
    ) -> Result<Option<Arc<ProviderRegistry>>, ConfigError> {
        if self.adapters.is_empty() {
            return Ok(None);
        }
        let mut registry = ProviderRegistry::new(runtime);
        for a in &self.adapters {
            if a == "synthetic" {
                let secret = self.synthetic_secret.as_ref().expect("validated");
                registry
                    .register(Arc::new(DeterministicProvider::new(
                        clock.clone(),
                        secret.expose_secret().as_bytes(),
                    )))
                    .map_err(|e| ConfigError::invalid("NUMBERING_PROVIDERS", e.to_string()))?;
            }
        }
        Ok(Some(Arc::new(registry)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn read(
        vars: &[(&str, &str)],
        runtime: RuntimeEnvironment,
    ) -> Result<ProviderConfig, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ProviderConfig::from_lookup(|k| map.get(k).cloned(), runtime)
    }

    #[test]
    fn the_synthetic_provider_never_runs_in_protected_runtimes() {
        let secret = (
            "NUMBERING_SYNTHETIC_PROVIDER_CALLBACK_SECRET",
            "synthetic-callback-secret-for-unit-tests-only",
        );
        assert!(read(&[], RuntimeEnvironment::Production)
            .unwrap()
            .adapters
            .is_empty());
        for runtime in [RuntimeEnvironment::Staging, RuntimeEnvironment::Production] {
            assert!(read(&[("NUMBERING_PROVIDERS", "synthetic"), secret], runtime).is_err());
        }
        assert!(
            read(
                &[("NUMBERING_PROVIDERS", "synthetic")],
                RuntimeEnvironment::Local
            )
            .is_err(),
            "secret required"
        );
        assert!(read(
            &[("NUMBERING_PROVIDERS", "carrierx"), secret],
            RuntimeEnvironment::Local
        )
        .is_err());
        let c = read(
            &[("NUMBERING_PROVIDERS", "synthetic"), secret],
            RuntimeEnvironment::Local,
        )
        .unwrap();
        assert!(!format!("{c:?}").contains("unit-tests-only"));
        let clock: SharedClock = Arc::new(crate::domain::SystemClock);
        let registry = c
            .registry(RuntimeEnvironment::Local, clock.clone())
            .unwrap()
            .unwrap();
        assert!(registry.get("synthetic").is_some());
        // Even if configuration were bypassed, the registry refuses it in production.
        assert!(c.registry(RuntimeEnvironment::Production, clock).is_err());
    }
}

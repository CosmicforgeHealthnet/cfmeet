//! Registered provider adapters and their environment restrictions.
//!
//! * A synthetic (test) provider is refused in staging / production runtimes — at
//!   registration, so a misconfigured deployment fails at startup.
//! * Synthetic inventory is served only by the synthetic provider (`synthetic`), real
//!   inventory never is (also enforced on bindings by the domain).
//! * Several real adapters can be registered side by side; each command names its
//!   provider, so adapters are independent.

use std::{collections::BTreeMap, sync::Arc};

use cf_service_auth::RuntimeEnvironment;

use super::{NumberingProvider, ProviderDescriptor};
use crate::numbers::InventorySource;

/// The provider key synthetic inventory is bound to.
pub const SYNTHETIC_PROVIDER: &str = "synthetic";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("a synthetic test provider is refused in staging / production")]
    SyntheticInProtectedRuntime,
    #[error("provider {0} is already registered")]
    Duplicate(String),
    #[error("provider {0} has an invalid descriptor: {1}")]
    InvalidDescriptor(String, &'static str),
}

#[derive(Clone)]
pub struct ProviderRegistry {
    runtime: RuntimeEnvironment,
    providers: BTreeMap<String, Arc<dyn NumberingProvider>>,
}

impl ProviderRegistry {
    pub fn new(runtime: RuntimeEnvironment) -> Self {
        Self {
            runtime,
            providers: BTreeMap::new(),
        }
    }

    pub fn runtime(&self) -> RuntimeEnvironment {
        self.runtime
    }

    /// Registers an adapter (fails closed on environment restrictions).
    pub fn register(&mut self, provider: Arc<dyn NumberingProvider>) -> Result<(), RegistryError> {
        let d = provider.descriptor();
        self.admits(&d)?;
        if self.providers.contains_key(&d.provider_id) {
            return Err(RegistryError::Duplicate(d.provider_id));
        }
        self.providers.insert(d.provider_id, provider);
        Ok(())
    }

    /// Whether an adapter with descriptor `d` may run in this runtime.
    pub fn admits(&self, d: &ProviderDescriptor) -> Result<(), RegistryError> {
        let d = d.clone();
        if !crate::numbers::inventory::valid_provider(&d.provider_id) {
            return Err(RegistryError::InvalidDescriptor(
                d.provider_id,
                "invalid provider key",
            ));
        }
        if d.synthetic != (d.provider_id == SYNTHETIC_PROVIDER) {
            return Err(RegistryError::InvalidDescriptor(
                d.provider_id,
                "the synthetic provider key is reserved for the synthetic test provider",
            ));
        }
        if d.synthetic && d.pstn_routable {
            return Err(RegistryError::InvalidDescriptor(
                d.provider_id,
                "synthetic numbers never route on the PSTN",
            ));
        }
        if d.synthetic
            && matches!(
                self.runtime,
                RuntimeEnvironment::Staging | RuntimeEnvironment::Production
            )
        {
            return Err(RegistryError::SyntheticInProtectedRuntime);
        }
        Ok(())
    }

    pub fn get(&self, provider_id: &str) -> Option<Arc<dyn NumberingProvider>> {
        self.providers.get(provider_id).cloned()
    }

    /// The adapter allowed to serve `provider_id` for inventory of `source`.
    pub fn for_inventory(
        &self,
        provider_id: &str,
        source: InventorySource,
    ) -> Option<Arc<dyn NumberingProvider>> {
        let synthetic = source == InventorySource::SyntheticTest;
        self.get(provider_id)
            .filter(|p| p.descriptor().synthetic == synthetic)
    }

    pub fn descriptors(&self) -> Vec<ProviderDescriptor> {
        self.providers.values().map(|p| p.descriptor()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

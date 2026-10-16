//! Shared application state.

use std::sync::Arc;

use cf_service_auth::ContactDelegationVerifier;

use crate::{
    config::{FeatureFlag, ServiceAuthRuntime},
    contact_vault::ContactVault,
    service::RelayService,
};

#[derive(Clone)]
pub struct AppState {
    pub relay: Arc<RelayService>,
    /// Internal service authentication (the only way into `/internal/v1`).
    pub service_auth: ServiceAuthRuntime,
    /// The Contact Vault (§31–§33).
    pub contacts: Arc<ContactVault>,
    /// Contact Vault access-token verification (the only way into the contact routes);
    /// None = not configured, every contact route fails closed.
    pub contact_delegation: Option<Arc<ContactDelegationVerifier>>,
}

impl AppState {
    /// State without a configured Contact Vault (contact routes answer
    /// CONTACT_VAULT_DISABLED).
    pub fn new(relay: RelayService, service_auth: ServiceAuthRuntime) -> Self {
        let contacts = ContactVault::new(
            relay.db().clone(),
            relay.clock.clone(),
            FeatureFlag::new(false),
            None,
        );
        Self {
            relay: Arc::new(relay),
            service_auth,
            contacts: Arc::new(contacts),
            contact_delegation: None,
        }
    }

    pub fn with_contact_vault(
        mut self,
        contacts: ContactVault,
        delegation: Option<ContactDelegationVerifier>,
    ) -> Self {
        self.contacts = Arc::new(contacts);
        self.contact_delegation = delegation.map(Arc::new);
        self
    }
}

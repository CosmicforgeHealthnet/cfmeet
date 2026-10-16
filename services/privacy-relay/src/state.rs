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
    /// Numbering's routing grant verification (DCN inbound caller protection); None = not
    /// configured, the DCN protection route fails closed.
    pub number_grants: Option<Arc<cf_service_auth::number_grant::NumberGrantVerifier>>,
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
            number_grants: None,
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

    /// Installs Numbering's routing grant verification (DCN inbound callers).
    pub fn with_number_grants(
        mut self,
        verifier: Option<cf_service_auth::number_grant::NumberGrantVerifier>,
    ) -> Self {
        self.number_grants = verifier.map(Arc::new);
        self
    }
}

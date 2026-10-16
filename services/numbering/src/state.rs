//! Shared application state.

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{service_auth::ServiceAuthRuntime, WorkerSettings},
    domain::SharedClock,
    identity::CustomerAuthority,
    numbers::{
        eligibility::EligibilityPolicy,
        kyc::{evidence::EvidenceVault, verifier::ManualKycVerifier, KycService, KycSettings},
        NumberDomain,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub clock: SharedClock,
    pub numbers: NumberDomain,
    /// Customer credential verification (Platform Access).
    pub authority: Arc<dyn CustomerAuthority>,
    /// Internal service authentication.
    pub service_auth: ServiceAuthRuntime,
    pub workers: WorkerSettings,
    /// KYC cases, evidence and verification (Workstream F).
    pub kyc: KycService,
    pub eligibility: Arc<EligibilityPolicy>,
    /// Provider commands, callbacks and reconciliation (Workstream G); `None` when no
    /// provider adapter is configured.
    pub providers: Option<crate::providers::commands::ProviderCommands>,
    /// Limits of the acquisition workflows (Workstreams H–K).
    pub acquisition_settings: crate::acquisition::AcquisitionSettings,
    /// Principal resolution in the trusted scope (Platform Access).
    pub principals: Arc<dyn crate::identity::PrincipalDirectory>,
    /// Routing grant issuance / verification (Workstream K); `None`: not configured (the
    /// grant endpoints refuse, fail closed).
    pub grants: Option<crate::grants::RoutingGrants>,
}

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        authority: Arc<dyn CustomerAuthority>,
        service_auth: ServiceAuthRuntime,
        workers: WorkerSettings,
    ) -> Self {
        let numbers = NumberDomain::new(db.clone(), clock.clone());
        Self {
            // Fail-closed defaults: manual review only, evidence refused until a protected
            // store is configured (`with_kyc`).
            kyc: KycService::new(
                numbers.clone(),
                Arc::new(EvidenceVault::unconfigured()),
                Arc::new(ManualKycVerifier),
                KycSettings::default(),
            ),
            eligibility: Arc::new(EligibilityPolicy::default()),
            providers: None,
            acquisition_settings: Default::default(),
            principals: Arc::new(crate::identity::UnconfiguredPrincipals),
            grants: None,
            numbers,
            db,
            clock,
            authority,
            service_auth,
            workers,
        }
    }

    /// Replaces the KYC service and eligibility policy.
    pub fn with_kyc(mut self, kyc: KycService, eligibility: EligibilityPolicy) -> Self {
        self.kyc = kyc;
        self.eligibility = Arc::new(eligibility);
        self
    }

    /// The acquisition workflows over this state's domain and providers.
    pub fn acquisition(&self) -> crate::acquisition::Acquisition {
        crate::acquisition::Acquisition::new(
            self.numbers.clone(),
            self.providers.clone(),
            self.eligibility.clone(),
            self.acquisition_settings.clone(),
        )
        .with_principals(self.principals.clone())
    }

    /// Installs the routing grant signer (Numbering's dedicated grant key).
    /// `clock` is the grant verifier's clock (the signer carries its own).
    pub fn with_grants(
        mut self,
        signer: cf_service_auth::number_grant::NumberGrantSigner,
        clock: cf_service_auth::SharedClock,
    ) -> Self {
        self.grants = Some(crate::grants::RoutingGrants::new(
            self.numbers.clone(),
            signer,
            clock,
        ));
        self
    }

    /// Installs the principal directory.
    pub fn with_principals(
        mut self,
        principals: Arc<dyn crate::identity::PrincipalDirectory>,
    ) -> Self {
        self.principals = principals;
        self
    }

    /// Replaces the acquisition limits.
    pub fn with_acquisition_settings(
        mut self,
        settings: crate::acquisition::AcquisitionSettings,
    ) -> Self {
        self.acquisition_settings = settings;
        self
    }

    /// Installs the provider command layer.
    pub fn with_providers(
        mut self,
        providers: crate::providers::commands::ProviderCommands,
    ) -> Self {
        self.providers = Some(providers);
        self
    }
}

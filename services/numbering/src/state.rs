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

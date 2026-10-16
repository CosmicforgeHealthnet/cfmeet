//! Number acquisition orchestration (Part IV Workstreams H–K).
//!
//! The customer-facing workflows that compose the A–G foundations — the number domain and
//! its transitions, the capability resolver (E), eligibility and KYC (F) and the durable
//! provider commands (G) — into:
//!
//! * [`search`] — provider-neutral inventory search with freshness, capability and KYC
//!   indicators, bounded pages and per-environment quotas (H);
//! * [`reservations`] — eligibility-bound, idempotent, concurrency-safe reservations with
//!   provider holds, cancellation and the expiry worker (H);
//! * [`provisioning`] — provisioning a reservation into a proven acquisition, provider
//!   voice / SMS route configuration and consumer readiness (I).
//!
//! Every operation runs in the TRUSTED scope from Platform Access; a request body never
//! names a tenant, application or environment. Provider identifiers never leave here.

pub mod provisioning;
pub mod reservations;
pub mod search;

use std::{collections::BTreeSet, sync::Arc};

use chrono::Duration;

use crate::{
    numbers::{eligibility::EligibilityPolicy, EnvironmentKind, NumberDomain},
    providers::commands::ProviderCommands,
};

/// Operator-configured limits of the acquisition workflows.
#[derive(Debug, Clone)]
pub struct AcquisitionSettings {
    /// Largest search page.
    pub search_page_max: u64,
    /// Searches per trusted environment per minute (shared by every instance).
    pub search_requests_per_minute: i32,
    /// Minimum interval between two provider refreshes of the same query.
    pub refresh_interval: Duration,
    /// Back-off after a failed provider refresh.
    pub refresh_failure_backoff: Duration,
    /// How long one search may hold the refresh of a query.
    pub refresh_claim: Duration,
    /// Reservation lifetime when the request names none.
    pub reservation_default_ttl: Duration,
    /// Environments where a temporary hold before eligibility is permitted. PRODUCTION is
    /// never one of them (also database-enforced).
    pub pre_eligibility_hold_environments: BTreeSet<EnvironmentKind>,
}

impl Default for AcquisitionSettings {
    fn default() -> Self {
        Self {
            search_page_max: 50,
            search_requests_per_minute: 120,
            refresh_interval: Duration::seconds(60),
            refresh_failure_backoff: Duration::seconds(30),
            refresh_claim: Duration::seconds(30),
            reservation_default_ttl: Duration::minutes(15),
            pre_eligibility_hold_environments: [EnvironmentKind::Sandbox, EnvironmentKind::Test]
                .into_iter()
                .collect(),
        }
    }
}

impl AcquisitionSettings {
    /// Whether a pre-eligibility hold is permitted in `kind` (never PRODUCTION).
    pub fn permits_pre_eligibility_hold(&self, kind: EnvironmentKind) -> bool {
        kind != EnvironmentKind::Production
            && self.pre_eligibility_hold_environments.contains(&kind)
    }
}

/// The acquisition workflows over Numbering's own state.
#[derive(Clone)]
pub struct Acquisition {
    pub(crate) numbers: NumberDomain,
    pub(crate) providers: Option<ProviderCommands>,
    pub(crate) eligibility: Arc<EligibilityPolicy>,
    pub(crate) settings: AcquisitionSettings,
}

impl Acquisition {
    pub fn new(
        numbers: NumberDomain,
        providers: Option<ProviderCommands>,
        eligibility: Arc<EligibilityPolicy>,
        settings: AcquisitionSettings,
    ) -> Self {
        Self {
            numbers,
            providers,
            eligibility,
            settings,
        }
    }

    pub fn numbers(&self) -> &NumberDomain {
        &self.numbers
    }

    pub fn settings(&self) -> &AcquisitionSettings {
        &self.settings
    }

    pub(crate) fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.numbers.clock().now()
    }

    /// The provider command layer, or `PROVIDER_UNAVAILABLE` (fail closed: nothing is
    /// held, provisioned or released without it).
    pub(crate) fn commands(&self) -> crate::numbers::DomainResult<&ProviderCommands> {
        self.providers.as_ref().ok_or_else(|| {
            crate::numbers::DomainError::precondition(
                "PROVIDER_UNAVAILABLE",
                "no numbering provider is configured",
            )
        })
    }
}

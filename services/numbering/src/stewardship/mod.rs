//! Lifecycle stewardship of a Digital Communications Number after activation (Part IV
//! Workstreams P–W) and the Part III relay-number authority (Part P §§119–120).
//!
//! Everything here ORCHESTRATES the existing authorities — the one lifecycle transition
//! path ([`crate::numbers::lifecycle::transition_in`]), the guarded assignment steps, the
//! durable provider command executor, grant revocation, the outbox and the incident
//! register. Nothing re-implements them:
//!
//! * [`suspension`] (P) — local-first suspension (grants refused and revoked at once),
//!   then the provider SUSPEND; restoration only when ownership, KYC / eligibility,
//!   standing, risk, binding, capabilities and route readiness all pass, after the
//!   provider RESTORE is confirmed.
//! * [`release`] (Q) — the mandatory release order, quarantine with policy, late
//!   activity, the safe completion worker and the reassignment gate.
//! * [`history`] (R) — ownership periods and privileged, audited, masked verification.
//! * [`usage`] (S) — idempotent internal cost / usage evidence.
//! * [`risk`] (T) — allocation decisions, signals, scoring, spend, cost breaker, appeals.
//! * [`porting`] (U) — the provider-independent port-in / port-out model (feature flag).
//! * [`reconcile`] (V) — recurring lifecycle-vs-provider drift checks.
//! * [`controls`] (W) — emergency kill switches.
//! * [`relay`] (Part P) — relay numbers as Number Resources and temporary leases.

pub mod controls;
pub mod history;
pub mod porting;
pub mod reconcile;
pub mod relay;
pub mod release;
pub mod risk;
pub mod suspension;
pub mod usage;

use std::{collections::BTreeMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};

use crate::{
    numbers::{
        eligibility::EligibilityPolicy, stmt, DomainError, DomainResult, NumberDomain, NumberType,
        RiskTier,
    },
    providers::commands::ProviderCommands,
};

/// Quarantine (cooldown) policy: jurisdiction, provider, number class and risk based.
#[derive(Debug, Clone)]
pub struct QuarantinePolicy {
    pub default_days: i64,
    pub by_number_type: BTreeMap<String, i64>,
    pub by_country: BTreeMap<String, i64>,
    /// Synthetic test inventory (sandbox / test only).
    pub synthetic_days: i64,
    /// Added for ELEVATED / HIGH risk numbers.
    pub elevated_risk_extra_days: i64,
    /// Late voice / SMS events after which completion needs an operator review.
    pub review_after_late_events: i64,
    /// Upper bound of an operator extension.
    pub max_extension_days: i64,
}

impl Default for QuarantinePolicy {
    fn default() -> Self {
        Self {
            default_days: 90,
            by_number_type: BTreeMap::from([
                ("MOBILE".into(), 90),
                ("GEOGRAPHIC".into(), 90),
                ("TOLL_FREE".into(), 45),
            ]),
            by_country: BTreeMap::new(),
            synthetic_days: 7,
            elevated_risk_extra_days: 30,
            review_after_late_events: 25,
            max_extension_days: 365,
        }
    }
}

impl QuarantinePolicy {
    /// The cooldown for a number and the policy reference recorded with it.
    pub fn cooldown(
        &self,
        country: &str,
        number_type: NumberType,
        synthetic: bool,
        risk: RiskTier,
    ) -> (i64, String) {
        let (mut days, mut basis) = if synthetic {
            (self.synthetic_days, "SYNTHETIC".to_string())
        } else if let Some(d) = self.by_country.get(country) {
            (*d, format!("COUNTRY_{country}"))
        } else if let Some(d) = self.by_number_type.get(number_type.as_str()) {
            (*d, format!("TYPE_{}", number_type.as_str()))
        } else {
            (self.default_days, "DEFAULT".to_string())
        };
        if matches!(risk, RiskTier::Elevated | RiskTier::High) {
            days += self.elevated_risk_extra_days;
            basis.push_str("_RISK");
        }
        (days.clamp(1, 3650), format!("QP1_{basis}"))
    }
}

/// Settings of the stewardship workflows.
#[derive(Debug, Clone)]
pub struct StewardshipSettings {
    pub quarantine: QuarantinePolicy,
    pub risk: risk::RiskPolicy,
    /// `NUMBERING_PORTING_ENABLED` (default false): porting APIs answer PORTING_DISABLED.
    pub porting_enabled: bool,
    /// Longest relay lease Numbering grants: covers Privacy Relay's longest session
    /// (366 days of validity + 30 days of completion grace).
    pub relay_lease_max: Duration,
}

impl Default for StewardshipSettings {
    fn default() -> Self {
        Self {
            quarantine: QuarantinePolicy::default(),
            risk: risk::RiskPolicy::default(),
            porting_enabled: false,
            relay_lease_max: Duration::days(400),
        }
    }
}

/// The stewardship workflows over Numbering's domain and provider layer.
#[derive(Clone)]
pub struct Stewardship {
    numbers: NumberDomain,
    providers: Option<ProviderCommands>,
    eligibility: Arc<EligibilityPolicy>,
    settings: StewardshipSettings,
    /// The porting boundary (default: no porting, [`porting::UnsupportedPorting`]).
    porting: Arc<dyn porting::PortingGateway>,
}

impl Stewardship {
    pub fn new(
        numbers: NumberDomain,
        providers: Option<ProviderCommands>,
        eligibility: Arc<EligibilityPolicy>,
        settings: StewardshipSettings,
    ) -> Self {
        Self {
            numbers,
            providers,
            eligibility,
            settings,
            porting: Arc::new(porting::UnsupportedPorting),
        }
    }

    /// Installs a porting gateway.
    pub fn with_porting(mut self, gateway: Arc<dyn porting::PortingGateway>) -> Self {
        self.porting = gateway;
        self
    }

    pub fn eligibility(&self) -> &Arc<EligibilityPolicy> {
        &self.eligibility
    }

    pub fn numbers(&self) -> &NumberDomain {
        &self.numbers
    }

    pub fn settings(&self) -> &StewardshipSettings {
        &self.settings
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.numbers.clock().now()
    }

    pub(crate) fn providers(&self) -> DomainResult<&ProviderCommands> {
        self.providers.as_ref().ok_or_else(|| {
            DomainError::precondition(
                "PROVIDER_NOT_CONFIGURED",
                "no provider adapter is configured",
            )
        })
    }

    /// One worker pass of every recurring stewardship task. Returns per-task counts.
    pub async fn run_workers(
        &self,
        ctx: &crate::domain::RequestContext,
        limit: u64,
    ) -> DomainResult<BTreeMap<&'static str, usize>> {
        let mut out = BTreeMap::new();
        out.insert("suspensions", self.advance_suspensions(ctx, limit).await?);
        out.insert("releases", self.advance_releases(ctx, limit).await?);
        out.insert("quarantines", self.complete_quarantines(ctx, limit).await?);
        out.insert("relay_leases", self.expire_relay_leases(ctx, limit).await?);
        out.insert("usage", self.accrue_usage(ctx, limit).await?);
        out.insert("ports", self.advance_ports(ctx, limit).await?);
        Ok(out)
    }
}

#[derive(Debug, FromQueryResult)]
pub(crate) struct Count {
    pub n: i64,
}

pub(crate) async fn count<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> DomainResult<i64> {
    Ok(Count::find_by_statement(stmt(sql, values))
        .one(db)
        .await?
        .map(|c| c.n)
        .unwrap_or(0))
}

#[derive(Debug, FromQueryResult)]
pub(crate) struct IdRow {
    pub id: uuid::Uuid,
}

/// Whether a provider command finished, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    InFlight,
    Succeeded,
    Failed,
    Unsupported,
    Unknown,
}

pub(crate) fn command_outcome(status: &str) -> CommandOutcome {
    match status {
        "SUCCEEDED" | "RECONCILED_SUCCEEDED" => CommandOutcome::Succeeded,
        "FAILED" | "RECONCILED_FAILED" => CommandOutcome::Failed,
        "UNSUPPORTED" => CommandOutcome::Unsupported,
        "UNKNOWN" => CommandOutcome::Unknown,
        _ => CommandOutcome::InFlight,
    }
}

/// A SHA-256 hex digest (number digests never carry the number itself).
pub(crate) fn digest(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value.as_bytes()))
}

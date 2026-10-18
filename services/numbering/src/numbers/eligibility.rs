//! Eligibility policy evaluation (Part IV Workstream F §8).
//!
//! [`NumberDomain::evaluate_eligibility`] decides — and records, append-only, with every
//! reason — whether a trusted scope may request a number of a country, type, ownership
//! class and capability set (optionally one specific resource). It reads the trusted
//! standing of the caller (tenant / application / environment as authenticated by
//! Platform Access), the capability catalog, KYC requirements and the caller's KYC case,
//! plan limits, restrictions and feature flags. It never reserves or provisions.
//!
//! The evaluation fails closed: anything unknown (country or type not in the catalog,
//! unknown mandatory KYC requirements, a restriction flag the requirements do not cover)
//! is a refusal, never an approval. Outcome precedence when several reasons apply:
//! DENIED > UNSUPPORTED_CAPABILITY > RISK_RESTRICTED > LIMIT_EXCEEDED >
//! ADDITIONAL_EVIDENCE_REQUIRED > KYC_REQUIRED > PENDING_REVIEW > ELIGIBLE.
//! An ELIGIBLE decision is valid until the earlier of its TTL and the KYC approval expiry.
//!
//! This is policy evaluation only — the risk scoring engine (Workstream T) is out of
//! scope; the risk input here is the recorded risk tier and operator restriction lists.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    catalog::select_entry,
    kyc::{
        cases::KycService,
        requirements::{resolve_requirements_in, RequirementQuery},
        EligibilityOutcome, KycStatus, RequirementType, SubjectKind, TenantClassification,
    },
    stmt, Capability, CountrySupport, DomainError, DomainResult, EnvironmentKind, InventoryClass,
    NumberDomain, NumberType, OwnerScope, OwnershipClass, RiskTier,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
};

/// Stable reason codes of eligibility decisions.
pub mod reason {
    pub const TENANT_NOT_ACTIVE: &str = "TENANT_NOT_ACTIVE";
    pub const LIVE_TELECOM_NOT_ALLOWED: &str = "LIVE_TELECOM_NOT_ALLOWED";
    pub const FEATURE_DISABLED: &str = "FEATURE_DISABLED";
    pub const COUNTRY_NOT_SUPPORTED: &str = "COUNTRY_NOT_SUPPORTED";
    pub const CATALOG_UNVERIFIED: &str = "CATALOG_UNVERIFIED";
    pub const NUMBER_TYPE_NOT_SUPPORTED: &str = "NUMBER_TYPE_NOT_SUPPORTED";
    pub const CAPABILITY_NOT_SUPPORTED: &str = "CAPABILITY_NOT_SUPPORTED";
    pub const NUMBER_MISMATCH: &str = "NUMBER_MISMATCH";
    pub const OWNERSHIP_SUBJECT_MISMATCH: &str = "OWNERSHIP_SUBJECT_MISMATCH";
    pub const NUMBER_LIMIT_REACHED: &str = "NUMBER_LIMIT_REACHED";
    pub const RISK_RESTRICTED: &str = "RISK_RESTRICTED";
    pub const JURISDICTION_RESTRICTED: &str = "JURISDICTION_RESTRICTED";
    pub const KYC_REQUIREMENTS_UNKNOWN: &str = "KYC_REQUIREMENTS_UNKNOWN";
    pub const KYC_REQUIRED: &str = "KYC_REQUIRED";
    pub const KYC_INCOMPLETE: &str = "KYC_INCOMPLETE";
    pub const KYC_ADDITIONAL_EVIDENCE: &str = "KYC_ADDITIONAL_EVIDENCE";
    pub const KYC_PENDING_REVIEW: &str = "KYC_PENDING_REVIEW";
    pub const KYC_REJECTED: &str = "KYC_REJECTED";
    pub const KYC_REVOKED: &str = "KYC_REVOKED";
    pub const KYC_EXPIRED: &str = "KYC_EXPIRED";
    pub const KYC_REQUIREMENTS_CHANGED: &str = "KYC_REQUIREMENTS_CHANGED";
    pub const PROVIDER_REQUIREMENTS_UNMET: &str = "PROVIDER_REQUIREMENTS_UNMET";

    pub const ALL: &[&str] = &[
        TENANT_NOT_ACTIVE,
        LIVE_TELECOM_NOT_ALLOWED,
        FEATURE_DISABLED,
        COUNTRY_NOT_SUPPORTED,
        CATALOG_UNVERIFIED,
        NUMBER_TYPE_NOT_SUPPORTED,
        CAPABILITY_NOT_SUPPORTED,
        NUMBER_MISMATCH,
        OWNERSHIP_SUBJECT_MISMATCH,
        NUMBER_LIMIT_REACHED,
        RISK_RESTRICTED,
        JURISDICTION_RESTRICTED,
        KYC_REQUIREMENTS_UNKNOWN,
        KYC_REQUIRED,
        KYC_INCOMPLETE,
        KYC_ADDITIONAL_EVIDENCE,
        KYC_PENDING_REVIEW,
        KYC_REJECTED,
        KYC_REVOKED,
        KYC_EXPIRED,
        KYC_REQUIREMENTS_CHANGED,
        PROVIDER_REQUIREMENTS_UNMET,
    ];
}

/// The trusted standing of the caller (from the authenticated authority chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    /// Tenant, application and environment are all ACTIVE (FULL authority).
    pub active: bool,
    pub first_party: bool,
    /// The environment may use live telecom (production only).
    pub live_telecom_allowed: bool,
}

/// What is asked.
#[derive(Debug, Clone)]
pub struct EligibilityRequest {
    pub subject_kind: SubjectKind,
    pub subject_ref: Uuid,
    pub country: String,
    pub number_type: NumberType,
    pub ownership_class: OwnershipClass,
    pub capabilities: Vec<Capability>,
    /// A specific resource being considered (optional).
    pub number_resource_id: Option<Uuid>,
    /// Internal: the provider that would serve the request (provider KYC extensions).
    pub provider_id: Option<String>,
}

/// Operator-configured policy inputs.
#[derive(Debug, Clone)]
pub struct EligibilityPolicy {
    pub version: String,
    pub ttl: Duration,
    /// Master feature flag of number eligibility (off: everything is DENIED).
    pub enabled: bool,
    pub disabled_countries: BTreeSet<String>,
    pub disabled_number_types: BTreeSet<NumberType>,
    /// Countries under an operator risk restriction (e.g. sanctions holds).
    pub risk_restricted_countries: BTreeSet<String>,
    /// Open (non-closed) assignments allowed per environment, by kind.
    pub max_numbers_sandbox: i64,
    pub max_numbers_test: i64,
    pub max_numbers_production: i64,
}

impl Default for EligibilityPolicy {
    fn default() -> Self {
        Self {
            version: "2026-10-18".into(),
            ttl: Duration::minutes(30),
            enabled: true,
            disabled_countries: BTreeSet::new(),
            disabled_number_types: BTreeSet::new(),
            risk_restricted_countries: BTreeSet::new(),
            max_numbers_sandbox: 5,
            max_numbers_test: 25,
            max_numbers_production: 100,
        }
    }
}

impl EligibilityPolicy {
    fn max_numbers(&self, kind: EnvironmentKind) -> i64 {
        match kind {
            EnvironmentKind::Sandbox => self.max_numbers_sandbox,
            EnvironmentKind::Test => self.max_numbers_test,
            EnvironmentKind::Production => self.max_numbers_production,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    pub code: &'static str,
    /// The outcome this reason leads to.
    pub outcome: EligibilityOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_requirements: Vec<RequirementType>,
}

impl Reason {
    fn new(code: &'static str, outcome: EligibilityOutcome) -> Self {
        Self {
            code,
            outcome,
            capability: None,
            missing_requirements: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityDecisionRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub service_account_id: Uuid,
    pub subject_kind: String,
    pub subject_ref: Uuid,
    pub country_code: String,
    pub inventory_class: String,
    pub number_type: String,
    pub ownership_class: String,
    pub number_resource_id: Option<Uuid>,
    pub requested_capabilities: Vec<String>,
    pub outcome: String,
    pub reasons: serde_json::Value,
    pub kyc_case_id: Option<Uuid>,
    pub kyc_case_revision: Option<i64>,
    pub requirement_set_ids: Vec<Uuid>,
    pub catalog_revision: i64,
    pub policy_version: String,
    pub evaluated_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub correlation_id: String,
}

impl EligibilityDecisionRow {
    pub fn outcome(&self) -> EligibilityOutcome {
        EligibilityOutcome::parse(&self.outcome).expect("CHECK-constrained outcome")
    }

    /// An ELIGIBLE decision still in force at `now` for `scope`.
    pub fn valid_for(&self, scope: &OwnerScope, now: DateTime<Utc>) -> bool {
        self.outcome() == EligibilityOutcome::Eligible
            && self.valid_until > now
            && self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }
}

/// Outcome precedence (lower wins).
fn rank(o: EligibilityOutcome) -> u8 {
    use EligibilityOutcome as O;
    match o {
        O::Denied => 0,
        O::UnsupportedCapability => 1,
        O::RiskRestricted => 2,
        O::LimitExceeded => 3,
        O::AdditionalEvidenceRequired => 4,
        O::KycRequired => 5,
        O::PendingReview => 6,
        O::Eligible => 7,
    }
}

/// The overall outcome of a set of reasons.
pub fn overall(reasons: &[Reason]) -> EligibilityOutcome {
    reasons
        .iter()
        .map(|r| r.outcome)
        .min_by_key(|o| rank(*o))
        .unwrap_or(EligibilityOutcome::Eligible)
}

/// Which KYC reason applies to a case (or its absence) at `now`.
fn kyc_reason(
    case: Option<&super::kyc::KycCaseRow>,
    required_sets: &[Uuid],
    now: DateTime<Utc>,
) -> Option<Reason> {
    use EligibilityOutcome as O;
    let Some(case) = case else {
        return Some(Reason::new(reason::KYC_REQUIRED, O::KycRequired));
    };
    let r = match case.status() {
        KycStatus::Draft | KycStatus::EvidenceRequired => {
            Reason::new(reason::KYC_INCOMPLETE, O::KycRequired)
        }
        KycStatus::MoreEvidenceRequired => Reason::new(
            reason::KYC_ADDITIONAL_EVIDENCE,
            O::AdditionalEvidenceRequired,
        ),
        KycStatus::Submitted | KycStatus::UnderReview => {
            Reason::new(reason::KYC_PENDING_REVIEW, O::PendingReview)
        }
        KycStatus::Rejected => Reason::new(reason::KYC_REJECTED, O::Denied),
        KycStatus::Revoked => Reason::new(reason::KYC_REVOKED, O::Denied),
        KycStatus::Expired => Reason::new(reason::KYC_EXPIRED, O::KycRequired),
        KycStatus::Approved if !case.approved_at_time(now) => {
            Reason::new(reason::KYC_EXPIRED, O::KycRequired)
        }
        KycStatus::Approved => {
            // The approval covers the requirement sets in force now (a newer or extra set
            // asks for more evidence).
            if required_sets
                .iter()
                .all(|s| case.requirement_set_ids.contains(s))
            {
                return None;
            }
            Reason::new(
                reason::KYC_REQUIREMENTS_CHANGED,
                O::AdditionalEvidenceRequired,
            )
        }
    };
    Some(r)
}

impl NumberDomain {
    /// Evaluates and records eligibility (see the module rules). Never reserves.
    pub async fn evaluate_eligibility(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        policy: &EligibilityPolicy,
        request: EligibilityRequest,
    ) -> DomainResult<EligibilityDecisionRow> {
        use EligibilityOutcome as O;
        super::catalog::valid_country(&request.country)?;
        if request.number_type == NumberType::Unclassified {
            return Err(DomainError::Validation(
                "eligibility is evaluated for a number type".into(),
            ));
        }
        if request.capabilities.is_empty() {
            return Err(DomainError::Validation(
                "name at least one requested capability".into(),
            ));
        }
        let mut capabilities = request.capabilities.clone();
        capabilities.sort();
        capabilities.dedup();
        if let Some(p) = &request.provider_id {
            if !super::inventory::valid_provider(p) {
                return Err(DomainError::Validation("malformed provider id".into()));
            }
        }
        let now = self.clock.now();
        let class = match scope.environment_kind {
            EnvironmentKind::Production => InventoryClass::Real,
            _ => InventoryClass::Synthetic,
        };
        let mut reasons: Vec<Reason> = Vec::new();

        // 1. Trusted standing, environment and feature flags.
        if !standing.active {
            reasons.push(Reason::new(reason::TENANT_NOT_ACTIVE, O::Denied));
        }
        if class == InventoryClass::Real && !standing.live_telecom_allowed {
            reasons.push(Reason::new(reason::LIVE_TELECOM_NOT_ALLOWED, O::Denied));
        }
        if !policy.enabled
            || policy.disabled_countries.contains(&request.country)
            || policy.disabled_number_types.contains(&request.number_type)
        {
            reasons.push(Reason::new(reason::FEATURE_DISABLED, O::Denied));
        }
        let individual = matches!(
            request.ownership_class,
            OwnershipClass::Personal | OwnershipClass::Professional
        );
        if individual != (request.subject_kind == SubjectKind::Individual) {
            reasons.push(Reason::new(reason::OWNERSHIP_SUBJECT_MISMATCH, O::Denied));
        }

        // Everything below reads one consistent snapshot.
        let txn = self
            .db
            .begin_with_config(
                Some(sea_orm::IsolationLevel::RepeatableRead),
                Some(sea_orm::AccessMode::ReadOnly),
            )
            .await?;
        let catalog_revision = super::catalog::catalog_revision_in(&txn).await?;

        // 2. Country / jurisdiction.
        let country = self
            .current_country_in(&txn, &request.country, class)
            .await?;
        let mut kyc_forced = false;
        match country.as_ref().filter(|c| c.is_effective_at(now)) {
            None => reasons.push(Reason::new(reason::COUNTRY_NOT_SUPPORTED, O::Denied)),
            Some(c) => {
                match CountrySupport::parse(&c.support_status) {
                    Some(CountrySupport::Supported) => {}
                    // A restricted jurisdiction is eligible only under KYC.
                    Some(CountrySupport::Restricted) => kyc_forced = true,
                    _ => reasons.push(Reason::new(reason::COUNTRY_NOT_SUPPORTED, O::Denied)),
                }
                if class == InventoryClass::Real && c.verification_status != "VERIFIED" {
                    reasons.push(Reason::new(reason::CATALOG_UNVERIFIED, O::Denied));
                }
            }
        }

        // 3. Number type and capabilities (catalog, then the specific number).
        let entries: Vec<super::catalog::TypeEntryRow> = super::catalog::TypeEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_type_catalog WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT'",
            vec![request.country.clone().into(), class.as_str().into()],
        ))
        .all(&txn)
        .await?;
        let mut flags: BTreeSet<String> = BTreeSet::new();
        let mut risk_tier = RiskTier::Unassessed;
        let resource = match request.number_resource_id {
            Some(id) => Some(
                super::resources::ResourceRow::find_by_statement(stmt(
                    "SELECT * FROM number_resources WHERE id = $1",
                    vec![id.into()],
                ))
                .one(&txn)
                .await?
                .ok_or(DomainError::NotFound("number resource"))?,
            ),
            None => None,
        };
        let region = resource.as_ref().and_then(|r| r.region.clone());
        let entry = select_entry(
            &entries,
            request.number_type,
            region.as_deref(),
            request.provider_id.as_deref(),
            now,
        );
        match entry {
            None => reasons.push(Reason::new(
                reason::NUMBER_TYPE_NOT_SUPPORTED,
                O::UnsupportedCapability,
            )),
            Some(e) => {
                if class == InventoryClass::Real && e.verification_status != "VERIFIED" {
                    reasons.push(Reason::new(reason::CATALOG_UNVERIFIED, O::Denied));
                }
                for c in &capabilities {
                    if !e.supports(*c) {
                        reasons.push(Reason {
                            capability: Some(*c),
                            ..Reason::new(
                                reason::CAPABILITY_NOT_SUPPORTED,
                                O::UnsupportedCapability,
                            )
                        });
                    }
                }
                flags.extend(e.restriction_flags.iter().cloned());
            }
        }
        if let Some(r) = &resource {
            if r.country_code != request.country
                || r.number_type != request.number_type.as_str()
                || super::InventorySource::parse(&r.inventory_source).map(InventoryClass::of)
                    != Some(class)
            {
                reasons.push(Reason::new(reason::NUMBER_MISMATCH, O::Denied));
            }
            flags.extend(r.restriction_flags.iter().cloned());
            risk_tier = RiskTier::parse(&r.risk_tier).unwrap_or(RiskTier::Unassessed);
        }
        drop(txn);
        if let Some(id) = request.number_resource_id.filter(|_| entry.is_some()) {
            // The authoritative resolver for the specific number (catalog AND evidence).
            let resolution = self.resolve_capabilities(id).await?;
            for c in &capabilities {
                if !resolution.get(*c).supported
                    && !reasons.iter().any(|r| r.capability == Some(*c))
                {
                    reasons.push(Reason {
                        capability: Some(*c),
                        ..Reason::new(reason::CAPABILITY_NOT_SUPPORTED, O::UnsupportedCapability)
                    });
                }
            }
        }

        // 4. Risk / restrictions (recorded inputs only; no scoring engine here).
        if policy.risk_restricted_countries.contains(&request.country)
            || (risk_tier == RiskTier::High && !standing.first_party)
        {
            reasons.push(Reason::new(reason::RISK_RESTRICTED, O::RiskRestricted));
        }

        // 5. Plan / number limits of the environment (numbers held past eligibility).
        #[derive(FromQueryResult)]
        struct Count {
            n: i64,
        }
        let open = Count::find_by_statement(stmt(
            "SELECT COUNT(*)::bigint AS n FROM dcn_assignments \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND status NOT IN ('CLOSED', 'REQUESTED', 'PENDING_ELIGIBILITY')",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
            ],
        ))
        .one(&self.db)
        .await?
        .map(|c| c.n)
        .unwrap_or(0);
        if open >= policy.max_numbers(scope.environment_kind) {
            reasons.push(Reason::new(reason::NUMBER_LIMIT_REACHED, O::LimitExceeded));
        }

        // 6. KYC requirements (including the serving provider's extension) and the case.
        let tenant_classification = if standing.first_party {
            TenantClassification::FirstParty
        } else {
            TenantClassification::External
        };
        let mut kyc_case = None;
        let mut requirement_set_ids = Vec::new();
        match resolve_requirements_in(
            &self.db,
            &RequirementQuery {
                country: request.country.clone(),
                inventory_class: class,
                number_type: request.number_type,
                ownership_class: request.ownership_class,
                provider_id: request.provider_id.clone(),
                tenant_classification,
            },
            now,
        )
        .await
        {
            Err(DomainError::Precondition {
                code: "KYC_REQUIREMENTS_UNKNOWN",
                ..
            }) => {
                reasons.push(Reason::new(reason::KYC_REQUIREMENTS_UNKNOWN, O::Denied));
            }
            Err(e) => return Err(e),
            Ok(req) => {
                requirement_set_ids = req.set_ids.clone();
                let covers =
                    |t: RequirementType| req.mandatory_items().any(|i| i.requirement_type == t);
                // Restriction flags must be covered by the requirements (fail closed).
                let needs_kyc = kyc_forced || flags.contains("KYC_REQUIRED");
                if (needs_kyc && !req.kyc_mandatory)
                    || (flags.contains("LOCAL_PRESENCE_REQUIRED")
                        && !covers(RequirementType::LocalPresence))
                {
                    reasons.push(Reason::new(reason::PROVIDER_REQUIREMENTS_UNMET, O::Denied));
                }
                if req.kyc_mandatory {
                    let case = KycService::latest_case(
                        &self.db,
                        scope,
                        request.subject_ref,
                        &request.country,
                        class,
                        request.number_type,
                        request.ownership_class,
                    )
                    .await?;
                    if let Some(r) = kyc_reason(case.as_ref(), &req.set_ids, now) {
                        reasons.push(r);
                    }
                    if kyc_forced && case.as_ref().is_some_and(|c| c.approved_at_time(now)) {
                        // A restricted jurisdiction under an approval in force.
                    } else if kyc_forced && !reasons.iter().any(|r| r.code.starts_with("KYC_")) {
                        reasons.push(Reason::new(reason::JURISDICTION_RESTRICTED, O::Denied));
                    }
                    kyc_case = case;
                }
            }
        }

        reasons.sort_by_key(|r| (rank(r.outcome), r.code));
        reasons.dedup();
        let outcome = overall(&reasons);
        let mut valid_until = now + policy.ttl;
        if outcome == O::Eligible {
            if let Some(expiry) = kyc_case.as_ref().and_then(|c| c.expires_at) {
                valid_until = valid_until.min(expiry);
            }
        }
        let row = EligibilityDecisionRow::find_by_statement(stmt(
            "INSERT INTO number_eligibility_decisions (id, tenant_id, application_id, environment_id, environment_kind, \
               service_account_id, subject_kind, subject_ref, country_code, inventory_class, number_type, ownership_class, \
               number_resource_id, requested_capabilities, outcome, reasons, kyc_case_id, kyc_case_revision, \
               requirement_set_ids, catalog_revision, policy_version, evaluated_at, valid_until, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24) \
             RETURNING *",
            vec![
                Uuid::new_v4().into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                scope.environment_kind.as_str().into(),
                scope.service_account_id.into(),
                request.subject_kind.as_str().into(),
                request.subject_ref.into(),
                request.country.clone().into(),
                class.as_str().into(),
                request.number_type.as_str().into(),
                request.ownership_class.as_str().into(),
                request.number_resource_id.into(),
                capabilities.iter().map(|c| c.as_str().to_string()).collect::<Vec<_>>().into(),
                outcome.as_str().into(),
                serde_json::to_value(&reasons).expect("serializable").into(),
                kyc_case.as_ref().map(|c| c.id).into(),
                kyc_case.as_ref().map(|c| c.revision).into(),
                requirement_set_ids.into(),
                catalog_revision.into(),
                policy.version.clone().into(),
                now.into(),
                valid_until.into(),
                ctx.correlation_id.as_str().into(),
            ],
        ));
        let txn = self.db.begin().await?;
        let row = row.one(&txn).await?.expect("insert returns the row");
        outbox::enqueue(
            &txn,
            NewEvent {
                event_type: "number.eligibility.evaluated",
                aggregate_type: "ELIGIBILITY_DECISION",
                aggregate_id: row.id,
                scope: Some(EventScope {
                    tenant_id: scope.tenant_id,
                    application_id: scope.application_id,
                    environment_id: scope.environment_id,
                }),
                payload: json!({
                    "eligibility_decision_id": row.id,
                    "outcome": row.outcome,
                    "reason_codes": reasons.iter().map(|r| r.code).collect::<Vec<_>>(),
                    "kyc_case_id": row.kyc_case_id,
                    "valid_until": row.valid_until,
                }),
            },
            ctx,
            now,
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_eligibility_decisions_total",
            &[("outcome", outcome.as_str())],
        );
        Ok(row)
    }

    /// An eligibility decision of the caller's scope.
    pub async fn eligibility_decision(
        &self,
        scope: &OwnerScope,
        id: Uuid,
    ) -> DomainResult<EligibilityDecisionRow> {
        EligibilityDecisionRow::find_by_statement(stmt(
            "SELECT * FROM number_eligibility_decisions WHERE id = $1 AND tenant_id = $2 AND application_id = $3 \
               AND environment_id = $4",
            vec![id.into(), scope.tenant_id.into(), scope.application_id.into(), scope.environment_id.into()],
        ))
        .one(&self.db)
        .await?
        .ok_or(DomainError::NotFound("eligibility decision"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_is_fail_closed() {
        use EligibilityOutcome as O;
        let r = |o| Reason::new("X_REASON", o);
        assert_eq!(overall(&[]), O::Eligible);
        assert_eq!(overall(&[r(O::PendingReview), r(O::Denied)]), O::Denied);
        assert_eq!(
            overall(&[r(O::KycRequired), r(O::UnsupportedCapability)]),
            O::UnsupportedCapability
        );
        assert_eq!(
            overall(&[r(O::LimitExceeded), r(O::RiskRestricted)]),
            O::RiskRestricted
        );
        assert_eq!(
            overall(&[r(O::KycRequired), r(O::AdditionalEvidenceRequired)]),
            O::AdditionalEvidenceRequired
        );
        let unique: BTreeSet<_> = reason::ALL.iter().collect();
        assert_eq!(unique.len(), reason::ALL.len());
    }
}

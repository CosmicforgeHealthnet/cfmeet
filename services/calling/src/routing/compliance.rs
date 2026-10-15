//! Compliance eligibility of paid public-network routes (Part H §78, §81).
//!
//! An environment reaches a destination country that is *compliance-required* only when
//! an operator recorded an ELIGIBLE, unexpired eligibility covering that country
//! (`platform_compliance_eligibility`, set by `platform_admin compliance set`). Which
//! countries are required comes from two sources, unioned: the platform overlay
//! (`CALLING_COMPLIANCE_REQUIRED_COUNTRIES`, which no customer policy can remove) and the
//! customer's routing policy (`complianceRequiredCountries`). `*` means every country.
//! The engine fails closed: an unreadable eligibility row denies the route.
//! `live_telecom_allowed` is independent — both gates must pass.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::engine::Evidence;
use crate::{calls::errors::CallResult, config::ConfigError};

text_enum!(
    /// Operator assessment of an environment.
    EligibilityStatus {
        NotAssessed = "NOT_ASSESSED",
        Eligible = "ELIGIBLE",
        Ineligible = "INELIGIBLE",
    }
);

/// Countries whose paid routes require compliance eligibility (ISO 3166-1 alpha-2, or
/// `*` for every country).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComplianceOverlay {
    pub all: bool,
    pub countries: Vec<String>,
}

fn iso_country(c: &str) -> bool {
    c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase())
}

impl ComplianceOverlay {
    /// Parses a comma-separated list of ISO-2 codes or `*` (empty = nothing required).
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        let mut overlay = Self::default();
        for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if item == "*" {
                overlay.all = true;
            } else if iso_country(item) {
                if !overlay.countries.contains(&item.to_string()) {
                    overlay.countries.push(item.to_string());
                }
            } else {
                return Err("must be a comma-separated list of ISO 3166-1 alpha-2 codes or *");
            }
        }
        overlay.countries.sort();
        Ok(overlay)
    }

    /// `CALLING_COMPLIANCE_REQUIRED_COUNTRIES` (comma list or `*`, default empty).
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        match get("CALLING_COMPLIANCE_REQUIRED_COUNTRIES") {
            None => Ok(Self::default()),
            Some(v) => Self::parse(&v).map_err(|reason| {
                ConfigError::invalid("CALLING_COMPLIANCE_REQUIRED_COUNTRIES", reason.to_string())
            }),
        }
    }

    pub fn requires(&self, country: &str) -> bool {
        self.all || self.countries.iter().any(|c| c == country)
    }
}

/// The persisted operator assessment of one environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityRecord {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub status: String,
    pub countries: Vec<String>,
    pub evidence_ref: Option<String>,
    pub decided_by: String,
    pub decided_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl EligibilityRecord {
    /// ELIGIBLE, unexpired at `now`, and covering `country` (empty list = every country).
    pub fn covers(&self, country: &str, now: DateTime<Utc>) -> bool {
        self.status == EligibilityStatus::Eligible.as_str()
            && self.expires_at.is_none_or(|e| e > now)
            && (self.countries.is_empty() || self.countries.iter().any(|c| c == country))
    }
}

/// The compliance facts the engine decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceFacts {
    pub overlay: ComplianceOverlay,
    /// The environment's eligibility: `Known(None)` = never assessed; `Unknown` = could not
    /// be read (fail closed whenever a country is required).
    pub eligibility: Evidence<Option<EligibilityRecord>>,
}

impl ComplianceFacts {
    /// No overlay and no assessment (the customer policy alone can still require one).
    pub fn not_required() -> Self {
        Self {
            overlay: ComplianceOverlay::default(),
            eligibility: Evidence::Known(None),
        }
    }
}

const COLUMNS: &str = "id, environment_id, status, countries, evidence_ref, decided_by, \
    decided_at, expires_at, version";

/// The environment's eligibility row, if any.
pub async fn load_eligibility<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> CallResult<Option<EligibilityRecord>> {
    Ok(
        EligibilityRecord::find_by_statement(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT {COLUMNS} FROM platform_compliance_eligibility WHERE environment_id = $1"
            ),
            [environment_id.into()],
        ))
        .one(db)
        .await?,
    )
}

/// The compliance facts of `environment_id` under `overlay`. A read failure becomes
/// `Unknown` evidence (the engine denies required countries), never an error that skips
/// the gate.
pub async fn facts<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    overlay: &ComplianceOverlay,
) -> ComplianceFacts {
    let eligibility = match load_eligibility(db, environment_id).await {
        Ok(row) => Evidence::Known(row),
        Err(err) => {
            tracing::error!(error = %err, "compliance eligibility could not be read; routes fail closed");
            Evidence::Unknown
        }
    };
    ComplianceFacts {
        overlay: overlay.clone(),
        eligibility,
    }
}

/// `platform_admin compliance set`: replaces the environment's assessment.
#[derive(Debug, Clone)]
pub struct SetEligibility {
    pub status: EligibilityStatus,
    pub countries: Vec<String>,
    pub evidence_ref: Option<String>,
    pub decided_by: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Upserts the assessment (one row per environment; `version` increments).
pub async fn store_eligibility<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    set: &SetEligibility,
    now: DateTime<Utc>,
) -> CallResult<Option<EligibilityRecord>> {
    let mut countries = set.countries.clone();
    countries.sort();
    countries.dedup();
    Ok(EligibilityRecord::find_by_statement(
        sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            format!(
                "INSERT INTO platform_compliance_eligibility (id, tenant_id, application_id, \
                 environment_id, status, countries, evidence_ref, decided_by, decided_at, \
                 expires_at, version, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1, $9, $9) \
                 ON CONFLICT (environment_id) DO UPDATE SET status = EXCLUDED.status, \
                   countries = EXCLUDED.countries, evidence_ref = EXCLUDED.evidence_ref, \
                   decided_by = EXCLUDED.decided_by, decided_at = EXCLUDED.decided_at, \
                   expires_at = EXCLUDED.expires_at, \
                   version = platform_compliance_eligibility.version + 1, \
                   updated_at = GREATEST(EXCLUDED.updated_at, platform_compliance_eligibility.updated_at) \
                 RETURNING {COLUMNS}"
            ),
            [
                Uuid::new_v4().into(),
                tenant_id.into(),
                application_id.into(),
                environment_id.into(),
                set.status.as_str().into(),
                countries.into(),
                set.evidence_ref.clone().into(),
                set.decided_by.clone().into(),
                now.into(),
                set.expires_at.into(),
            ],
        ),
    )
    .one(db)
    .await?)
}

/// Whether `value` is an opaque evidence reference: `^[A-Za-z0-9._:-]{1,200}$`, starting
/// with a letter or digit. Never a document, an address or a number with a `+`.
pub fn valid_evidence_ref(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 200
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(c))
}

/// Validated input of `platform_admin compliance set`.
fn validated(set: &SetEligibility, now: DateTime<Utc>) -> Result<(), &'static str> {
    if set.countries.len() > 250 || !set.countries.iter().all(|c| iso_country(c)) {
        return Err("countries must be at most 250 ISO 3166-1 alpha-2 codes");
    }
    if set
        .evidence_ref
        .as_deref()
        .is_some_and(|e| !valid_evidence_ref(e))
    {
        return Err("the evidence reference must match ^[A-Za-z0-9._:-]{1,200}$");
    }
    if set.expires_at.is_some_and(|e| e <= now) {
        return Err("the expiry must be in the future");
    }
    Ok(())
}

/// The safe operator view (`compliance show`): status, countries, expiry and whether an
/// evidence reference is recorded — never the reference itself.
pub fn safe_view(record: &EligibilityRecord, now: DateTime<Utc>) -> serde_json::Value {
    serde_json::json!({
        "environmentId": record.environment_id,
        "status": record.status,
        "countries": record.countries,
        "expiresAt": record.expires_at,
        "expired": record.expires_at.is_some_and(|e| e <= now),
        "evidenceRefPresent": record.evidence_ref.is_some(),
        "decidedBy": record.decided_by,
        "decidedAt": record.decided_at,
        "version": record.version,
    })
}

fn audit_entry(
    action: crate::platform::operator::OperatorAction,
    environment: &crate::platform::environments::EnvironmentView,
    record: &EligibilityRecord,
) -> crate::platform::operator::AuditEntry {
    crate::platform::operator::AuditEntry::new(
        action,
        crate::platform::operator::AuditTargetKind::ComplianceEligibility,
        record.id,
    )
    .scope(
        Some(environment.tenant_id.as_uuid()),
        Some(environment.application_id.as_uuid()),
        Some(environment.id.as_uuid()),
    )
    .detail(serde_json::json!({
        "status": record.status,
        "countries": record.countries,
        "expiresAt": record.expires_at,
        "evidenceRefPresent": record.evidence_ref.is_some(),
        "version": record.version,
    }))
}

/// `platform_admin compliance set` (audited, one transaction): replaces the
/// environment's assessment. `decided_by` is the operator.
#[allow(clippy::too_many_arguments)]
pub async fn operator_set(
    db: &sea_orm::DatabaseConnection,
    operator: &crate::platform::operator::OperatorContext,
    environment: &crate::platform::environments::EnvironmentView,
    status: EligibilityStatus,
    countries: Vec<String>,
    evidence_ref: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> crate::platform::PlatformResult<EligibilityRecord> {
    use crate::platform::{operator, PlatformError};
    let set = SetEligibility {
        status,
        countries,
        evidence_ref,
        decided_by: operator.operator().to_string(),
        expires_at,
    };
    validated(&set, now).map_err(|m| PlatformError::Validation(m.to_string()))?;
    let environment = environment.clone();
    operator::audited(db, operator, now, move |txn| {
        Box::pin(async move {
            let record = store_eligibility(
                txn,
                environment.tenant_id.as_uuid(),
                environment.application_id.as_uuid(),
                environment.id.as_uuid(),
                &set,
                now,
            )
            .await
            .map_err(eligibility_error)?
            .ok_or(PlatformError::NotFound("environment"))?;
            let entry = audit_entry(
                operator::OperatorAction::ComplianceEligibilitySet,
                &environment,
                &record,
            );
            Ok((record, entry))
        })
    })
    .await
}

/// `platform_admin compliance revoke` (audited): the environment's assessment becomes
/// INELIGIBLE (countries and evidence cleared). Nothing recorded = not found.
pub async fn operator_revoke(
    db: &sea_orm::DatabaseConnection,
    operator: &crate::platform::operator::OperatorContext,
    environment: &crate::platform::environments::EnvironmentView,
    now: DateTime<Utc>,
) -> crate::platform::PlatformResult<EligibilityRecord> {
    use crate::platform::{operator, PlatformError};
    let environment = environment.clone();
    let decided_by = operator.operator().to_string();
    operator::audited(db, operator, now, move |txn| {
        Box::pin(async move {
            let current = load_eligibility(txn, environment.id.as_uuid())
                .await
                .map_err(eligibility_error)?
                .ok_or(PlatformError::NotFound("compliance eligibility"))?;
            let record = store_eligibility(
                txn,
                environment.tenant_id.as_uuid(),
                environment.application_id.as_uuid(),
                environment.id.as_uuid(),
                &SetEligibility {
                    status: EligibilityStatus::Ineligible,
                    countries: Vec::new(),
                    evidence_ref: None,
                    decided_by,
                    expires_at: None,
                },
                now.max(current.decided_at),
            )
            .await
            .map_err(eligibility_error)?
            .ok_or(PlatformError::NotFound("compliance eligibility"))?;
            let entry = audit_entry(
                operator::OperatorAction::ComplianceEligibilityRevoked,
                &environment,
                &record,
            );
            Ok((record, entry))
        })
    })
    .await
}

fn eligibility_error(e: crate::calls::errors::CallError) -> crate::platform::PlatformError {
    crate::platform::PlatformError::Database {
        summary: format!("compliance eligibility ({})", e.code()),
        connectivity: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_parses_iso_codes_and_star() {
        let o = ComplianceOverlay::parse(" NG, GB ,NG").unwrap();
        assert_eq!(o.countries, vec!["GB", "NG"]);
        assert!(o.requires("NG") && !o.requires("US"));
        assert!(ComplianceOverlay::parse("*").unwrap().requires("US"));
        assert!(ComplianceOverlay::parse("").unwrap() == ComplianceOverlay::default());
        assert!(ComplianceOverlay::parse("ng").is_err());
        assert!(ComplianceOverlay::parse("+234").is_err());
    }

    #[test]
    fn evidence_references_are_bounded_opaque_codes() {
        for ok in ["KYC-2026:0042", "a", "ops.case_17", &"x".repeat(200)] {
            assert!(valid_evidence_ref(ok), "{ok}");
        }
        for bad in [
            "",
            "-lead",
            "has space",
            "+2348012345678",
            "https://docs/x.pdf",
            &"x".repeat(201),
        ] {
            assert!(!valid_evidence_ref(bad), "{bad}");
        }
    }

    #[test]
    fn eligibility_covers_only_eligible_unexpired_matching_rows() {
        let now = Utc::now();
        let mut r = EligibilityRecord {
            id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            status: "ELIGIBLE".into(),
            countries: vec![],
            evidence_ref: None,
            decided_by: "op".into(),
            decided_at: now,
            expires_at: None,
            version: 1,
        };
        assert!(r.covers("NG", now));
        r.countries = vec!["GB".into()];
        assert!(!r.covers("NG", now) && r.covers("GB", now));
        r.expires_at = Some(now - chrono::Duration::seconds(1));
        assert!(!r.covers("GB", now));
        r.expires_at = None;
        r.status = "INELIGIBLE".into();
        assert!(!r.covers("GB", now));
    }
}

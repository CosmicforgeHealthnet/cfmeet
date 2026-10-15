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

//! Authoritative capability resolution (Part IV Workstream E).
//!
//! For one Number Resource and every canonical capability, combines — from ONE
//! consistent snapshot (repeatable-read, read-only transaction):
//!
//! 1. the resource (state, inventory class, evidence-backed number type, region,
//!    restriction flags, revisions);
//! 2. its ACTIVE provider bindings;
//! 3. the country and number-type catalog of its inventory class (provider-specific
//!    entry before generic, region before national; only CURRENT entries effective now);
//! 4. its per-resource capability evidence (status, source, expiry, confirming binding);
//! 5. restrictions and effective dates;
//! 6. provenance and revisions.
//!
//! The result says, per capability, whether it is **supported** (the catalog allows it
//! AND this number has valid evidence for it) and **usable** now (supported, and for
//! routable capabilities an ACTIVE resource with an active binding serving it), with a
//! normalized reason when not, effective limitations and provenance. It fails closed:
//! missing catalog, missing / unverified / expired evidence, an unclassified number or
//! an ended confirming binding all mean "not supported". Rules that are never relaxed:
//! SMS never implies a mobile class; inbound never implies outbound; country support
//! never implies number support; real numbers need VERIFIED catalog facts and VERIFIED
//! evidence; synthetic evidence is never reported as carrier confirmation.

use chrono::{DateTime, Utc};
use sea_orm::{AccessMode, FromQueryResult, IsolationLevel, TransactionTrait};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::{
    bindings::BindingRow,
    capabilities::{current_capabilities_in, CapabilityRow},
    catalog::{select_entry, CountryEntryRow, TypeEntryRow},
    limitations, stmt, Capability, CapabilitySource, CapabilityStatus, DomainError, DomainResult,
    InventoryClass, LifecycleState, NumberDomain, NumberType,
};
use crate::numbers::resources::ResourceRow;

/// Normalized reasons a capability is not supported / not usable.
pub mod reason {
    pub const RESOURCE_NOT_ACTIVE: &str = "RESOURCE_NOT_ACTIVE";
    pub const RESOURCE_UNCERTAIN: &str = "RESOURCE_UNCERTAIN";
    pub const CATALOG_COUNTRY_MISSING: &str = "CATALOG_COUNTRY_MISSING";
    pub const COUNTRY_NOT_SUPPORTED: &str = "COUNTRY_NOT_SUPPORTED";
    pub const CATALOG_UNVERIFIED: &str = "CATALOG_UNVERIFIED";
    pub const NUMBER_TYPE_UNCLASSIFIED: &str = "NUMBER_TYPE_UNCLASSIFIED";
    pub const CATALOG_TYPE_MISSING: &str = "CATALOG_TYPE_MISSING";
    pub const CAPABILITY_EXCLUDED_BY_CATALOG: &str = "CAPABILITY_EXCLUDED_BY_CATALOG";
    pub const CAPABILITY_NOT_IN_CATALOG: &str = "CAPABILITY_NOT_IN_CATALOG";
    pub const NUMBER_CLASS_MISMATCH: &str = "NUMBER_CLASS_MISMATCH";
    pub const EVIDENCE_MISSING: &str = "EVIDENCE_MISSING";
    pub const EVIDENCE_UNVERIFIED: &str = "EVIDENCE_UNVERIFIED";
    pub const EVIDENCE_EXPIRED: &str = "EVIDENCE_EXPIRED";
    pub const EVIDENCE_NOT_YET_EFFECTIVE: &str = "EVIDENCE_NOT_YET_EFFECTIVE";
    pub const EVIDENCE_BINDING_ENDED: &str = "EVIDENCE_BINDING_ENDED";
    pub const CAPABILITY_RESTRICTED: &str = "CAPABILITY_RESTRICTED";
    pub const CAPABILITY_WITHDRAWN: &str = "CAPABILITY_WITHDRAWN";
    pub const CAPABILITY_REVOKED: &str = "CAPABILITY_REVOKED";
    pub const BINDING_MISSING: &str = "BINDING_MISSING";

    pub const ALL: &[&str] = &[
        RESOURCE_NOT_ACTIVE,
        RESOURCE_UNCERTAIN,
        CATALOG_COUNTRY_MISSING,
        COUNTRY_NOT_SUPPORTED,
        CATALOG_UNVERIFIED,
        NUMBER_TYPE_UNCLASSIFIED,
        CATALOG_TYPE_MISSING,
        CAPABILITY_EXCLUDED_BY_CATALOG,
        CAPABILITY_NOT_IN_CATALOG,
        NUMBER_CLASS_MISMATCH,
        EVIDENCE_MISSING,
        EVIDENCE_UNVERIFIED,
        EVIDENCE_EXPIRED,
        EVIDENCE_NOT_YET_EFFECTIVE,
        EVIDENCE_BINDING_ENDED,
        CAPABILITY_RESTRICTED,
        CAPABILITY_WITHDRAWN,
        CAPABILITY_REVOKED,
        BINDING_MISSING,
    ];
}

/// Where a resolved capability's truth comes from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub evidence_record_id: Option<Uuid>,
    pub evidence_status: Option<String>,
    pub evidence_source: Option<String>,
    pub evidence_ref: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Synthetic test data (never a carrier fact).
    pub synthetic: bool,
    /// A VERIFIED provider confirmation of a REAL number through a binding that is still
    /// active — never true for synthetic inventory.
    pub carrier_confirmed: bool,
    pub catalog_entry_id: Option<Uuid>,
    pub catalog_entry_revision: Option<i64>,
    pub catalog_verification: Option<String>,
    /// Opaque binding ref serving the capability (routable capabilities).
    pub binding_ref: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveCapability {
    pub capability: Capability,
    pub supported: bool,
    pub usable: bool,
    /// Why it is not supported / not usable (`None`: usable).
    pub reason: Option<&'static str>,
    pub limitations: Value,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityResolution {
    pub number_resource_id: Uuid,
    pub state: String,
    pub inventory_class: InventoryClass,
    pub number_type: String,
    pub resource_revision: i64,
    pub capability_revision: i64,
    pub binding_revision: i64,
    /// The latest published catalog revision this resolution read.
    pub catalog_revision: i64,
    pub resolved_at: DateTime<Utc>,
    pub restriction_flags: Vec<String>,
    pub capabilities: Vec<EffectiveCapability>,
    pub notices: Vec<&'static str>,
}

impl CapabilityResolution {
    pub fn get(&self, c: Capability) -> &EffectiveCapability {
        self.capabilities
            .iter()
            .find(|e| e.capability == c)
            .expect("every canonical capability is resolved")
    }

    /// The usable capabilities, in canonical order.
    pub fn usable(&self) -> Vec<Capability> {
        self.capabilities
            .iter()
            .filter(|e| e.usable)
            .map(|e| e.capability)
            .collect()
    }

    /// The supported capabilities, in canonical order.
    pub fn supported(&self) -> Vec<Capability> {
        self.capabilities
            .iter()
            .filter(|e| e.supported)
            .map(|e| e.capability)
            .collect()
    }
}

struct Inputs {
    resource: ResourceRow,
    records: Vec<CapabilityRow>,
    bindings: Vec<BindingRow>,
    inventory_provider: Option<String>,
    country: Option<CountryEntryRow>,
    types: Vec<TypeEntryRow>,
    catalog_revision: i64,
}

fn class_attribute_type(c: Capability) -> Option<NumberType> {
    match c {
        Capability::MobileNumberClass => Some(NumberType::Mobile),
        Capability::GeographicLocal => Some(NumberType::Geographic),
        Capability::TollFreeBusiness => Some(NumberType::TollFree),
        _ => None,
    }
}

/// The pure resolution rules (deterministic in their inputs and `now`).
fn resolve(i: &Inputs, now: DateTime<Utc>) -> CapabilityResolution {
    let r = &i.resource;
    let class = InventoryClass::of(r.inventory());
    let real = class == InventoryClass::Real;
    let number_type = NumberType::parse(&r.number_type).unwrap_or(NumberType::Unclassified);
    let state = r.lifecycle_state();
    let active_bindings: Vec<&BindingRow> =
        i.bindings.iter().filter(|b| b.status == "ACTIVE").collect();
    let mut caps = Vec::new();
    for &c in Capability::ALL {
        let record = i.records.iter().find(|x| x.capability == c.as_str());
        let serving = active_bindings
            .iter()
            .find(|b| b.capabilities.iter().any(|x| x == c.as_str()))
            .copied();
        let provider = serving
            .or(active_bindings.first().copied())
            .map(|b| b.provider_id.as_str())
            .or(i.inventory_provider.as_deref());
        let entry = (number_type != NumberType::Unclassified)
            .then(|| select_entry(&i.types, number_type, r.region.as_deref(), provider, now))
            .flatten();
        let evidence_binding_active = record
            .and_then(|x| x.binding_id)
            .map(|id| active_bindings.iter().any(|b| b.id == id));
        let country_reason = match &i.country {
            None => Some(reason::CATALOG_COUNTRY_MISSING),
            Some(country) if !country.is_effective_at(now) => Some(reason::CATALOG_COUNTRY_MISSING),
            Some(country) if country.support_status == "UNSUPPORTED" => {
                Some(reason::COUNTRY_NOT_SUPPORTED)
            }
            Some(country) if real && country.verification_status != "VERIFIED" => {
                Some(reason::CATALOG_UNVERIFIED)
            }
            Some(_) if number_type == NumberType::Unclassified => {
                Some(reason::NUMBER_TYPE_UNCLASSIFIED)
            }
            Some(_) => None,
        };
        let type_reason = match entry {
            None => Some(reason::CATALOG_TYPE_MISSING),
            Some(e) if real && e.verification_status != "VERIFIED" => {
                Some(reason::CATALOG_UNVERIFIED)
            }
            Some(e) if e.excludes(c) => Some(reason::CAPABILITY_EXCLUDED_BY_CATALOG),
            Some(e) if !e.supports(c) => Some(reason::CAPABILITY_NOT_IN_CATALOG),
            Some(_) => None,
        };
        let class_reason = class_attribute_type(c)
            .filter(|required| *required != number_type)
            .map(|_| reason::NUMBER_CLASS_MISMATCH);
        let evidence_reason = match record {
            None => Some(reason::EVIDENCE_MISSING),
            Some(x) => match x.status() {
                CapabilityStatus::Restricted => Some(reason::CAPABILITY_RESTRICTED),
                CapabilityStatus::Withdrawn => Some(reason::CAPABILITY_WITHDRAWN),
                CapabilityStatus::Revoked => Some(reason::CAPABILITY_REVOKED),
                CapabilityStatus::Expired => Some(reason::EVIDENCE_EXPIRED),
                _ if x.is_expired_at(now) => Some(reason::EVIDENCE_EXPIRED),
                _ if x.effective_from > now => Some(reason::EVIDENCE_NOT_YET_EFFECTIVE),
                _ if evidence_binding_active == Some(false) => Some(reason::EVIDENCE_BINDING_ENDED),
                CapabilityStatus::Declared if real => Some(reason::EVIDENCE_UNVERIFIED),
                _ => None,
            },
        };
        // The country first, then the number's own class (an SMS-capable geographic
        // number is never mobile, whatever any entry says), then the type entry, then the
        // evidence.
        let not_supported = country_reason
            .or(class_reason)
            .or(type_reason)
            .or(evidence_reason);
        let usable_reason = if not_supported.is_some() {
            not_supported
        } else if c.is_routable() {
            if state == LifecycleState::UnknownProviderState {
                Some(reason::RESOURCE_UNCERTAIN)
            } else if state != LifecycleState::Active {
                Some(reason::RESOURCE_NOT_ACTIVE)
            } else if serving.is_none() {
                Some(reason::BINDING_MISSING)
            } else {
                None
            }
        } else {
            None
        };
        let synthetic = !real || record.is_some_and(|x| x.source() == CapabilitySource::Synthetic);
        let carrier_confirmed = real
            && record.is_some_and(|x| {
                x.source() == CapabilitySource::ProviderConfirmation
                    && x.status() == CapabilityStatus::Verified
                    && x.grants_at(now)
                    && evidence_binding_active == Some(true)
            });
        caps.push(EffectiveCapability {
            capability: c,
            supported: not_supported.is_none(),
            usable: usable_reason.is_none(),
            reason: usable_reason,
            limitations: limitations::effective(
                c,
                entry.map(|e| &e.limitations).unwrap_or(&Value::Null),
                record.map(|x| &x.limitations).unwrap_or(&Value::Null),
            ),
            provenance: Provenance {
                evidence_record_id: record.map(|x| x.id),
                evidence_status: record.map(|x| x.status.clone()),
                evidence_source: record.map(|x| x.source.clone()),
                evidence_ref: record.and_then(|x| x.evidence_ref),
                verified_at: record.and_then(|x| x.verified_at),
                expires_at: record.and_then(|x| x.expires_at),
                synthetic,
                carrier_confirmed,
                catalog_entry_id: entry.map(|e| e.id),
                catalog_entry_revision: entry.map(|e| e.catalog_revision),
                catalog_verification: entry.map(|e| e.verification_status.clone()),
                binding_ref: if c.is_routable() {
                    serving.map(|b| b.id)
                } else {
                    None
                },
            },
        });
    }
    let mut restriction_flags: Vec<String> = r.restriction_flags.clone();
    for extra in i.country.iter().flat_map(|c| c.restriction_flags.iter()) {
        if !restriction_flags.contains(extra) {
            restriction_flags.push(extra.clone());
        }
    }
    restriction_flags.sort();
    let voice_or_sms = caps.iter().any(|e| {
        e.supported
            && matches!(
                e.capability,
                Capability::VoiceInbound
                    | Capability::VoiceOutbound
                    | Capability::SmsInbound
                    | Capability::SmsOutbound
                    | Capability::MmsRich
            )
    });
    CapabilityResolution {
        number_resource_id: r.id,
        state: r.state.clone(),
        inventory_class: class,
        number_type: r.number_type.clone(),
        resource_revision: r.revision,
        capability_revision: r.capability_revision,
        binding_revision: r.binding_revision,
        catalog_revision: i.catalog_revision,
        resolved_at: now,
        restriction_flags,
        capabilities: caps,
        notices: if voice_or_sms {
            vec![limitations::EXTERNAL_VERIFICATION_NOTICE]
        } else {
            vec![]
        },
    }
}

impl NumberDomain {
    /// Resolves every capability of a resource (see the module rules).
    pub async fn resolve_capabilities(
        &self,
        resource_id: Uuid,
    ) -> DomainResult<CapabilityResolution> {
        let now = self.clock.now();
        let txn = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await?;
        let resource = ResourceRow::find_by_statement(stmt(
            "SELECT * FROM number_resources WHERE id = $1",
            vec![resource_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("number resource"))?;
        let records = current_capabilities_in(&txn, resource_id).await?;
        let bindings = BindingRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_bindings WHERE number_resource_id = $1 AND status = 'ACTIVE' \
             ORDER BY binding_revision",
            vec![resource_id.into()],
        ))
        .all(&txn)
        .await?;
        #[derive(FromQueryResult)]
        struct Provider {
            provider_id: Option<String>,
        }
        let inventory_provider = Provider::find_by_statement(stmt(
            "SELECT provider_id FROM number_inventory_snapshots WHERE number_resource_id = $1 \
             ORDER BY observed_at DESC, sequence DESC LIMIT 1",
            vec![resource_id.into()],
        ))
        .one(&txn)
        .await?
        .and_then(|p| p.provider_id);
        let class = InventoryClass::of(resource.inventory());
        let country = CountryEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_country_catalog WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT'",
            vec![resource.country_code.clone().into(), class.as_str().into()],
        ))
        .one(&txn)
        .await?;
        let types = TypeEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_type_catalog WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT'",
            vec![resource.country_code.clone().into(), class.as_str().into()],
        ))
        .all(&txn)
        .await?;
        #[derive(FromQueryResult)]
        struct Rev {
            n: i64,
        }
        let catalog_revision = Rev::find_by_statement(stmt(
            "SELECT COALESCE(MAX(revision), 0) AS n FROM number_catalog_revisions",
            vec![],
        ))
        .one(&txn)
        .await?
        .map(|r| r.n)
        .unwrap_or(0);
        txn.commit().await?;
        Ok(resolve(
            &Inputs {
                resource,
                records,
                bindings,
                inventory_provider,
                country,
                types,
                catalog_revision,
            },
            now,
        ))
    }

    /// One capability, or `CAPABILITY_NOT_SUPPORTED` (with the reason) — and, when
    /// `usable` is required, `CAPABILITY_NOT_USABLE`. Never falls back to another number.
    pub async fn require_capability(
        &self,
        resource_id: Uuid,
        capability: Capability,
        usable: bool,
    ) -> DomainResult<EffectiveCapability> {
        let resolution = self.resolve_capabilities(resource_id).await?;
        let e = resolution.get(capability).clone();
        if !e.supported {
            return Err(DomainError::precondition(
                "CAPABILITY_NOT_SUPPORTED",
                format!(
                    "{capability} is not supported for this number ({})",
                    e.reason.unwrap_or("UNKNOWN")
                ),
            ));
        }
        if usable && !e.usable {
            return Err(DomainError::precondition(
                "CAPABILITY_NOT_USABLE",
                format!(
                    "{capability} is not usable now ({})",
                    e.reason.unwrap_or("UNKNOWN")
                ),
            ));
        }
        Ok(e)
    }
}

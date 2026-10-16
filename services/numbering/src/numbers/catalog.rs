//! The authoritative, versioned country / number-type catalog (Part IV Workstream E).
//!
//! * Every change publishes exactly one gap-free catalog revision
//!   (`number_catalog_revisions`) and closes the CURRENT entry it replaces (SUPERSEDED at
//!   that revision): history is never rewritten.
//! * Real and synthetic inventory have separate catalogs ([`InventoryClass`]). Synthetic
//!   entries are `SYNTHETIC_TEST` / `SYNTHETIC` by construction and never describe a real
//!   number; real entries are VERIFIED only with evidence. Nothing here is inferred: an
//!   empty catalog means nothing is supported (fail closed).
//! * A number-type entry states supported AND explicitly unsupported capabilities;
//!   anything in neither list is unknown, which the resolver treats as unsupported.
//!   Number-class attributes follow the type (`MOBILE_NUMBER_CLASS` only on MOBILE).
//! * Country support never establishes support for an individual number: the resolver
//!   also requires per-resource evidence ([`super::resolution`]).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    e164, limitations, stmt, types::reason_code, Actor, Capability, CatalogSource,
    CatalogVerification, CountrySupport, DomainError, DomainResult, InventoryClass, LocalityClass,
    NumberDomain, NumberType, RestrictionFlag,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountryEntryRow {
    pub id: Uuid,
    pub country_code: String,
    pub inventory_class: String,
    pub calling_code: String,
    pub support_status: String,
    pub restriction_flags: Vec<String>,
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub source: String,
    pub verification_status: String,
    pub evidence_ref: Option<Uuid>,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub catalog_revision: i64,
    pub status: String,
    pub superseded_at_revision: Option<i64>,
    pub recorded_by: String,
    pub recorded_at: DateTime<Utc>,
}

impl CountryEntryRow {
    pub fn is_effective_at(&self, now: DateTime<Utc>) -> bool {
        self.status == "CURRENT"
            && self.effective_from <= now
            && self.effective_to.is_none_or(|t| t > now)
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeEntryRow {
    pub id: Uuid,
    pub country_code: String,
    pub inventory_class: String,
    pub number_type: String,
    pub locality_class: String,
    pub region: Option<String>,
    /// Internal provider applicability (never exposed to customers).
    #[serde(skip)]
    pub provider_id: Option<String>,
    pub supported_capabilities: Vec<String>,
    pub unsupported_capabilities: Vec<String>,
    pub limitations: Value,
    pub restriction_flags: Vec<String>,
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub source: String,
    pub verification_status: String,
    pub evidence_ref: Option<Uuid>,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub catalog_revision: i64,
    pub status: String,
    pub superseded_at_revision: Option<i64>,
    pub recorded_by: String,
    pub recorded_at: DateTime<Utc>,
}

impl TypeEntryRow {
    pub fn is_effective_at(&self, now: DateTime<Utc>) -> bool {
        self.status == "CURRENT"
            && self.effective_from <= now
            && self.effective_to.is_none_or(|t| t > now)
    }

    pub fn supports(&self, c: Capability) -> bool {
        self.supported_capabilities.iter().any(|s| s == c.as_str())
    }

    pub fn excludes(&self, c: Capability) -> bool {
        self.unsupported_capabilities
            .iter()
            .any(|s| s == c.as_str())
    }
}

/// Provenance common to every catalog fact.
#[derive(Debug, Clone)]
pub struct CatalogProvenance {
    pub source: CatalogSource,
    pub verification: CatalogVerification,
    pub evidence_ref: Option<Uuid>,
    /// Defaults to now.
    pub effective_from: Option<DateTime<Utc>>,
    /// Expiry of the fact (`None`: until superseded).
    pub effective_to: Option<DateTime<Utc>>,
}

impl CatalogProvenance {
    /// Synthetic test catalog data (never VERIFIED, never real).
    pub fn synthetic() -> Self {
        Self {
            source: CatalogSource::SyntheticTest,
            verification: CatalogVerification::Synthetic,
            evidence_ref: None,
            effective_from: None,
            effective_to: None,
        }
    }

    pub fn verified(source: CatalogSource, evidence_ref: Uuid) -> Self {
        Self {
            source,
            verification: CatalogVerification::Verified,
            evidence_ref: Some(evidence_ref),
            effective_from: None,
            effective_to: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CountryEntry {
    pub country: String,
    pub inventory_class: InventoryClass,
    pub support: CountrySupport,
    pub restriction_flags: Vec<RestrictionFlag>,
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub provenance: CatalogProvenance,
}

#[derive(Debug, Clone)]
pub struct TypeEntry {
    pub country: String,
    pub inventory_class: InventoryClass,
    pub number_type: NumberType,
    pub locality: LocalityClass,
    pub region: Option<String>,
    /// Provider applicability (`None`: every provider).
    pub provider_id: Option<String>,
    pub supported: Vec<Capability>,
    pub unsupported: Vec<Capability>,
    pub limitations: Value,
    pub restriction_flags: Vec<RestrictionFlag>,
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub provenance: CatalogProvenance,
}

fn validate_provenance(
    class: InventoryClass,
    p: &CatalogProvenance,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    let synthetic = class == InventoryClass::Synthetic;
    if synthetic != (p.source == CatalogSource::SyntheticTest)
        || synthetic != (p.verification == CatalogVerification::Synthetic)
    {
        return Err(DomainError::Validation(
            "synthetic catalog entries are SYNTHETIC_TEST / SYNTHETIC, and only they are".into(),
        ));
    }
    if p.verification == CatalogVerification::Verified && p.evidence_ref.is_none() {
        return Err(DomainError::precondition(
            "EVIDENCE_REQUIRED",
            "a VERIFIED catalog fact needs evidence",
        ));
    }
    let from = p.effective_from.unwrap_or(now);
    if p.effective_to.is_some_and(|to| to <= from) {
        return Err(DomainError::Validation(
            "a catalog fact expires after it takes effect".into(),
        ));
    }
    Ok(())
}

fn sorted(caps: &[Capability]) -> Vec<String> {
    let mut v: Vec<String> = caps.iter().map(|c| c.as_str().to_string()).collect();
    v.sort();
    v.dedup();
    v
}

fn flags(flags: &[RestrictionFlag]) -> Vec<String> {
    let mut v: Vec<String> = flags.iter().map(|f| f.as_str().to_string()).collect();
    v.sort();
    v.dedup();
    v
}

/// Opens the next catalog revision inside `txn` (serialized by the catalog lock).
async fn next_revision(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    actor: &Actor,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<i64> {
    reason_code(reason)?;
    txn.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtext('number_catalog'))",
        vec![],
    ))
    .await?;
    #[derive(FromQueryResult)]
    struct Next {
        n: i64,
    }
    let revision = Next::find_by_statement(stmt(
        "SELECT COALESCE(MAX(revision), 0) + 1 AS n FROM number_catalog_revisions",
        vec![],
    ))
    .one(txn)
    .await?
    .map(|r| r.n)
    .unwrap_or(1);
    txn.execute(stmt(
        "INSERT INTO number_catalog_revisions (revision, actor_kind, actor_ref, reason_code, correlation_id, published_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        vec![
            revision.into(),
            actor.kind.as_str().into(),
            actor.reference.clone().into(),
            reason.into(),
            ctx.correlation_id.as_str().into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(revision)
}

#[allow(clippy::too_many_arguments)]
async fn catalog_event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    revision: i64,
    entry_kind: &str,
    entry_id: Uuid,
    country: &str,
    class: InventoryClass,
    number_type: Option<NumberType>,
    change: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.catalog.revised",
            aggregate_type: "NUMBER_CATALOG",
            aggregate_id: Uuid::nil(),
            scope: None,
            payload: json!({
                "catalog_revision": revision,
                "entry_kind": entry_kind,
                "entry_id": entry_id,
                "country_code": country,
                "inventory_class": class.as_str(),
                "number_type": number_type.map(|t| t.as_str()),
                "change": change,
                "reason_code": reason,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

impl NumberDomain {
    /// Publishes a country entry (superseding the current one of its class).
    pub async fn publish_country(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reason: &str,
        entry: CountryEntry,
    ) -> DomainResult<CountryEntryRow> {
        let now = self.clock.now();
        let calling_code = e164::calling_code(&entry.country).ok_or_else(|| {
            DomainError::precondition(
                "COUNTRY_UNKNOWN",
                "the country is not in Numbering's E.164 plan table",
            )
        })?;
        validate_provenance(entry.inventory_class, &entry.provenance, now)?;
        let txn = self.db.begin().await?;
        let revision = next_revision(&txn, ctx, &actor, reason, now).await?;
        txn.execute(stmt(
            "UPDATE number_country_catalog SET status = 'SUPERSEDED', superseded_at_revision = $3 \
             WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT'",
            vec![
                entry.country.clone().into(),
                entry.inventory_class.as_str().into(),
                revision.into(),
            ],
        ))
        .await?;
        let p = &entry.provenance;
        let row = CountryEntryRow::find_by_statement(stmt(
            "INSERT INTO number_country_catalog (id, country_code, inventory_class, calling_code, support_status, \
               restriction_flags, jurisdiction_policy_ref, source, verification_status, evidence_ref, effective_from, \
               effective_to, catalog_revision, recorded_by, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                entry.country.clone().into(),
                entry.inventory_class.as_str().into(),
                calling_code.into(),
                entry.support.as_str().into(),
                flags(&entry.restriction_flags).into(),
                entry.jurisdiction_policy_ref.into(),
                p.source.as_str().into(),
                p.verification.as_str().into(),
                p.evidence_ref.into(),
                p.effective_from.unwrap_or(now).into(),
                p.effective_to.into(),
                revision.into(),
                actor.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        catalog_event(
            &txn,
            ctx,
            revision,
            "COUNTRY",
            row.id,
            &entry.country,
            entry.inventory_class,
            None,
            "PUBLISHED",
            reason,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Publishes a number-type entry (superseding the current one with the same country,
    /// class, type, region and provider applicability).
    pub async fn publish_number_type(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reason: &str,
        entry: TypeEntry,
    ) -> DomainResult<TypeEntryRow> {
        let now = self.clock.now();
        validate_provenance(entry.inventory_class, &entry.provenance, now)?;
        limitations::validate(&entry.limitations)?;
        if entry
            .supported
            .iter()
            .any(|c| entry.unsupported.contains(c))
        {
            return Err(DomainError::Validation(
                "a capability is either supported or unsupported".into(),
            ));
        }
        for (class_cap, required) in [
            (Capability::MobileNumberClass, NumberType::Mobile),
            (Capability::GeographicLocal, NumberType::Geographic),
            (Capability::TollFreeBusiness, NumberType::TollFree),
        ] {
            if entry.supported.contains(&class_cap) && entry.number_type != required {
                return Err(DomainError::precondition(
                    "NUMBER_CLASS_MISMATCH",
                    format!("{class_cap} applies to {required} numbers only (SMS support never makes a number mobile)"),
                ));
            }
        }
        if let Some(p) = &entry.provider_id {
            if !super::inventory::valid_provider(p)
                || (entry.inventory_class == InventoryClass::Synthetic) != (p == "synthetic")
            {
                return Err(DomainError::Validation(
                    "invalid provider applicability".into(),
                ));
            }
        }
        if let Some(region) = &entry.region {
            if region.trim().is_empty() || region.len() > 100 {
                return Err(DomainError::Validation(
                    "the region is 1–100 characters".into(),
                ));
            }
        }
        let txn = self.db.begin().await?;
        let country = self
            .current_country_in(&txn, &entry.country, entry.inventory_class)
            .await?
            .filter(|c| c.support_status != "UNSUPPORTED")
            .ok_or_else(|| {
                DomainError::precondition(
                    "COUNTRY_NOT_SUPPORTED",
                    "publish a supported country entry before its number types",
                )
            })?;
        let revision = next_revision(&txn, ctx, &actor, reason, now).await?;
        txn.execute(stmt(
            "UPDATE number_type_catalog SET status = 'SUPERSEDED', superseded_at_revision = $6 \
             WHERE country_code = $1 AND inventory_class = $2 AND number_type = $3 \
               AND COALESCE(region, '') = COALESCE($4, '') AND COALESCE(provider_id, '') = COALESCE($5, '') \
               AND status = 'CURRENT'",
            vec![
                entry.country.clone().into(),
                entry.inventory_class.as_str().into(),
                entry.number_type.as_str().into(),
                entry.region.clone().into(),
                entry.provider_id.clone().into(),
                revision.into(),
            ],
        ))
        .await?;
        let p = &entry.provenance;
        let row = TypeEntryRow::find_by_statement(stmt(
            "INSERT INTO number_type_catalog (id, country_code, inventory_class, number_type, locality_class, region, \
               provider_id, supported_capabilities, unsupported_capabilities, limitations, restriction_flags, \
               jurisdiction_policy_ref, source, verification_status, evidence_ref, effective_from, effective_to, \
               catalog_revision, recorded_by, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                country.country_code.clone().into(),
                entry.inventory_class.as_str().into(),
                entry.number_type.as_str().into(),
                entry.locality.as_str().into(),
                entry.region.clone().into(),
                entry.provider_id.clone().into(),
                sorted(&entry.supported).into(),
                sorted(&entry.unsupported).into(),
                entry.limitations.clone().into(),
                flags(&entry.restriction_flags).into(),
                entry.jurisdiction_policy_ref.into(),
                p.source.as_str().into(),
                p.verification.as_str().into(),
                p.evidence_ref.into(),
                p.effective_from.unwrap_or(now).into(),
                p.effective_to.into(),
                revision.into(),
                actor.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        catalog_event(
            &txn,
            ctx,
            revision,
            "NUMBER_TYPE",
            row.id,
            &entry.country,
            entry.inventory_class,
            Some(entry.number_type),
            "PUBLISHED",
            reason,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Withdraws a current number-type entry (no successor): resolution falls back to the
    /// generic entry, or fails closed.
    pub async fn withdraw_number_type(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reason: &str,
        entry_id: Uuid,
    ) -> DomainResult<i64> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = TypeEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_type_catalog WHERE id = $1 AND status = 'CURRENT'",
            vec![entry_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("catalog entry"))?;
        let revision = next_revision(&txn, ctx, &actor, reason, now).await?;
        txn.execute(stmt(
            "UPDATE number_type_catalog SET status = 'SUPERSEDED', superseded_at_revision = $2 WHERE id = $1",
            vec![entry_id.into(), revision.into()],
        ))
        .await?;
        let class = InventoryClass::parse(&current.inventory_class).expect("CHECK-constrained");
        catalog_event(
            &txn,
            ctx,
            revision,
            "NUMBER_TYPE",
            entry_id,
            &current.country_code,
            class,
            NumberType::parse(&current.number_type),
            "WITHDRAWN",
            reason,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(revision)
    }

    async fn current_country_in<C: ConnectionTrait>(
        &self,
        db: &C,
        country: &str,
        class: InventoryClass,
    ) -> DomainResult<Option<CountryEntryRow>> {
        Ok(CountryEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_country_catalog WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT'",
            vec![country.into(), class.as_str().into()],
        ))
        .one(db)
        .await?)
    }

    /// The current country entry of a class (effective or not).
    pub async fn country_entry(
        &self,
        country: &str,
        class: InventoryClass,
    ) -> DomainResult<Option<CountryEntryRow>> {
        self.current_country_in(&self.db, country, class).await
    }

    /// The current number-type entries of a country and class.
    pub async fn number_type_entries(
        &self,
        country: &str,
        class: InventoryClass,
    ) -> DomainResult<Vec<TypeEntryRow>> {
        Ok(TypeEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_type_catalog WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT' \
             ORDER BY number_type, region NULLS FIRST, provider_id NULLS FIRST",
            vec![country.into(), class.as_str().into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// Every version of the catalog entries of a country (oldest first).
    pub async fn number_type_history(
        &self,
        country: &str,
        class: InventoryClass,
    ) -> DomainResult<Vec<TypeEntryRow>> {
        Ok(TypeEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_type_catalog WHERE country_code = $1 AND inventory_class = $2 ORDER BY catalog_revision",
            vec![country.into(), class.as_str().into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// The latest published catalog revision (0: empty catalog).
    pub async fn catalog_revision(&self) -> DomainResult<i64> {
        #[derive(FromQueryResult)]
        struct Rev {
            n: i64,
        }
        Ok(Rev::find_by_statement(stmt(
            "SELECT COALESCE(MAX(revision), 0) AS n FROM number_catalog_revisions",
            vec![],
        ))
        .one(&self.db)
        .await?
        .map(|r| r.n)
        .unwrap_or(0))
    }
}

/// The catalog entry governing a number: provider-specific (for `provider`) before
/// generic, region-specific (matching the number's region) before national. Only
/// CURRENT entries effective at `now` qualify.
pub fn select_entry<'a>(
    entries: &'a [TypeEntryRow],
    number_type: NumberType,
    region: Option<&str>,
    provider: Option<&str>,
    now: DateTime<Utc>,
) -> Option<&'a TypeEntryRow> {
    entries
        .iter()
        .filter(|e| e.number_type == number_type.as_str() && e.is_effective_at(now))
        .filter(|e| e.region.is_none() || e.region.as_deref() == region)
        .filter(|e| e.provider_id.is_none() || e.provider_id.as_deref() == provider)
        .max_by_key(|e| {
            (
                e.provider_id.is_some(),
                e.region.is_some(),
                e.catalog_revision,
            )
        })
}

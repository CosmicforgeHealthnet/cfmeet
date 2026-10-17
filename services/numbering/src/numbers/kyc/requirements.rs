//! The versioned KYC requirement catalog (Part IV Workstream F §5).
//!
//! A requirement set applies to a selector — country and inventory class (always), and
//! optionally number type, ownership class, tenant classification — and is either a BASE
//! set or a PROVIDER_EXTENSION (extra items a specific provider demands). Resolution for a
//! request takes the most specific current BASE set (number type before ownership class
//! before tenant classification) plus every matching extension, and returns the exact
//! snapshot a case preserves. No BASE set means the requirements are unknown: production
//! (and synthetic) requests FAIL CLOSED with `KYC_REQUIREMENTS_UNKNOWN`. Synthetic sets are
//! explicitly non-production (`SYNTHETIC` class, `SYNTHETIC_TEST` source).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::types::{EvidenceKind, RequirementScope, RequirementType, TenantClassification};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
    numbers::{
        stmt, types::reason_code, Actor, CatalogSource, DomainError, DomainResult, InventoryClass,
        NumberDomain, NumberType, OwnershipClass,
    },
};

/// One requirement item of a set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementItem {
    #[serde(rename = "type")]
    pub requirement_type: RequirementType,
    pub mandatory: bool,
    /// Evidence kinds accepted for it (non-empty).
    pub accepted_evidence: Vec<EvidenceKind>,
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementSetRow {
    pub id: Uuid,
    pub selector_key: String,
    pub scope_kind: String,
    pub country_code: String,
    pub inventory_class: String,
    pub number_type: Option<String>,
    pub ownership_class: Option<String>,
    /// Internal provider applicability.
    #[serde(skip)]
    pub provider_id: Option<String>,
    pub tenant_classification: Option<String>,
    pub kyc_mandatory: bool,
    pub items: Value,
    pub approval_validity_days: i32,
    pub revalidation_days: Option<i32>,
    pub source: String,
    pub source_version: String,
    pub version: i32,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub status: String,
    pub recorded_by: String,
    pub recorded_at: DateTime<Utc>,
}

impl RequirementSetRow {
    pub fn items(&self) -> Vec<RequirementItem> {
        serde_json::from_value(self.items.clone()).unwrap_or_default()
    }

    fn effective_at(&self, now: DateTime<Utc>) -> bool {
        self.status == "CURRENT"
            && self.effective_from <= now
            && self.effective_to.is_none_or(|t| t > now)
    }
}

/// A requirement set to publish.
#[derive(Debug, Clone)]
pub struct RequirementSet {
    pub scope: RequirementScope,
    pub country: String,
    pub inventory_class: InventoryClass,
    pub number_type: Option<NumberType>,
    pub ownership_class: Option<OwnershipClass>,
    pub provider_id: Option<String>,
    pub tenant_classification: Option<TenantClassification>,
    /// `false` (BASE only): this selector requires no KYC (explicitly decided).
    pub kyc_mandatory: bool,
    pub items: Vec<RequirementItem>,
    pub approval_validity_days: i32,
    pub revalidation_days: Option<i32>,
    pub source: CatalogSource,
    pub source_version: String,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
}

/// What to resolve requirements for.
#[derive(Debug, Clone)]
pub struct RequirementQuery {
    pub country: String,
    pub inventory_class: InventoryClass,
    pub number_type: NumberType,
    pub ownership_class: OwnershipClass,
    pub provider_id: Option<String>,
    pub tenant_classification: TenantClassification,
}

/// The resolved requirements (and the exact snapshot a case preserves).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRequirements {
    pub set_ids: Vec<Uuid>,
    pub kyc_mandatory: bool,
    pub items: Vec<RequirementItem>,
    pub approval_validity_days: i32,
    pub revalidation_days: Option<i32>,
    pub synthetic: bool,
    pub snapshot: Value,
}

impl ResolvedRequirements {
    pub fn mandatory_items(&self) -> impl Iterator<Item = &RequirementItem> {
        self.items.iter().filter(|i| i.mandatory)
    }
}

fn selector_key(s: &RequirementSet) -> String {
    let opt = |v: Option<&str>| v.unwrap_or("*").to_string();
    [
        s.scope.as_str().to_string(),
        s.country.clone(),
        s.inventory_class.as_str().to_string(),
        opt(s.number_type.map(|t| t.as_str())),
        opt(s.ownership_class.map(|o| o.as_str())),
        opt(s.provider_id.as_deref()),
        opt(s.tenant_classification.map(|t| t.as_str())),
    ]
    .join("|")
}

fn validate(s: &RequirementSet, now: DateTime<Utc>) -> DomainResult<()> {
    if s.country.len() != 2 || !s.country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(DomainError::Validation(
            "country is ISO 3166-1 alpha-2".into(),
        ));
    }
    let synthetic = s.inventory_class == InventoryClass::Synthetic;
    if synthetic != (s.source == CatalogSource::SyntheticTest) {
        return Err(DomainError::Validation(
            "synthetic requirement sets are SYNTHETIC_TEST and only they are (never production policy)".into(),
        ));
    }
    if (s.scope == RequirementScope::ProviderExtension) != s.provider_id.is_some() {
        return Err(DomainError::Validation(
            "a provider extension names its provider, and only it does".into(),
        ));
    }
    if let Some(p) = &s.provider_id {
        if !crate::numbers::inventory::valid_provider(p) || synthetic != (p == "synthetic") {
            return Err(DomainError::Validation("invalid provider".into()));
        }
    }
    if s.scope == RequirementScope::Base && s.kyc_mandatory == s.items.is_empty() {
        return Err(DomainError::Validation(
            "a BASE set is KYC-mandatory exactly when it lists requirements".into(),
        ));
    }
    if s.items.len() > 16 {
        return Err(DomainError::Validation(
            "at most 16 requirement items".into(),
        ));
    }
    let mut seen = Vec::new();
    for item in &s.items {
        if seen.contains(&item.requirement_type) {
            return Err(DomainError::Validation(
                "each requirement type appears once".into(),
            ));
        }
        seen.push(item.requirement_type);
        if item.accepted_evidence.is_empty()
            || item.accepted_evidence.iter().any(|k| {
                k.default_requirement() != item.requirement_type
                    && !(item.requirement_type == super::types::RequirementType::LocalPresence
                        && *k == EvidenceKind::ProofOfAddress)
            })
        {
            return Err(DomainError::Validation(
                "a requirement accepts evidence kinds that can prove it".into(),
            ));
        }
    }
    if !(1..=3660).contains(&s.approval_validity_days)
        || s.revalidation_days
            .is_some_and(|r| r < 1 || r > s.approval_validity_days)
    {
        return Err(DomainError::Validation(
            "validity and revalidation are bounded days".into(),
        ));
    }
    if s.source_version.is_empty()
        || s.source_version.len() > 64
        || !s
            .source_version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(DomainError::Validation("invalid source version".into()));
    }
    let from = s.effective_from.unwrap_or(now);
    if s.effective_to.is_some_and(|to| to <= from) {
        return Err(DomainError::Validation(
            "a requirement set expires after it takes effect".into(),
        ));
    }
    Ok(())
}

impl NumberDomain {
    /// Publishes a requirement set (next version of its selector; the previous one is
    /// superseded — cases keep the snapshot they were opened against).
    pub async fn publish_requirements(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reason: &str,
        set: RequirementSet,
    ) -> DomainResult<RequirementSetRow> {
        reason_code(reason)?;
        let now = self.clock.now();
        validate(&set, now)?;
        let key = selector_key(&set);
        let txn = self.db.begin().await?;
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock(hashtext($1))",
            vec![key.clone().into()],
        ))
        .await?;
        #[derive(FromQueryResult)]
        struct V {
            v: i32,
        }
        let version = V::find_by_statement(stmt(
            "SELECT COALESCE(MAX(version), 0) + 1 AS v FROM number_kyc_requirements WHERE selector_key = $1",
            vec![key.clone().into()],
        ))
        .one(&txn)
        .await?
        .map(|v| v.v)
        .unwrap_or(1);
        txn.execute(stmt(
            "UPDATE number_kyc_requirements SET status = 'SUPERSEDED' WHERE selector_key = $1 AND status = 'CURRENT'",
            vec![key.clone().into()],
        ))
        .await?;
        let row = RequirementSetRow::find_by_statement(stmt(
            "INSERT INTO number_kyc_requirements (id, selector_key, scope_kind, country_code, inventory_class, number_type, \
               ownership_class, provider_id, tenant_classification, kyc_mandatory, items, approval_validity_days, \
               revalidation_days, source, source_version, version, effective_from, effective_to, recorded_by, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                key.into(),
                set.scope.as_str().into(),
                set.country.clone().into(),
                set.inventory_class.as_str().into(),
                set.number_type.map(|t| t.as_str().to_string()).into(),
                set.ownership_class.map(|o| o.as_str().to_string()).into(),
                set.provider_id.clone().into(),
                set.tenant_classification.map(|t| t.as_str().to_string()).into(),
                set.kyc_mandatory.into(),
                serde_json::to_value(&set.items).expect("serializable").into(),
                set.approval_validity_days.into(),
                set.revalidation_days.into(),
                set.source.as_str().into(),
                set.source_version.clone().into(),
                version.into(),
                set.effective_from.unwrap_or(now).into(),
                set.effective_to.into(),
                actor.reference.clone().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        outbox::enqueue(
            &txn,
            NewEvent {
                event_type: "number.kyc-requirements.published",
                aggregate_type: "KYC_REQUIREMENTS",
                aggregate_id: row.id,
                scope: None,
                payload: json!({
                    "requirement_set_id": row.id,
                    "scope_kind": row.scope_kind,
                    "country_code": row.country_code,
                    "inventory_class": row.inventory_class,
                    "number_type": row.number_type,
                    "ownership_class": row.ownership_class,
                    "version": row.version,
                    "kyc_mandatory": row.kyc_mandatory,
                    "reason_code": reason,
                }),
            },
            ctx,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Resolves the requirements of a request (fail closed: `KYC_REQUIREMENTS_UNKNOWN`).
    pub async fn resolve_requirements(
        &self,
        q: &RequirementQuery,
    ) -> DomainResult<ResolvedRequirements> {
        resolve_requirements_in(&self.db, q, self.clock.now()).await
    }

    /// The current requirement sets of a country and class (customer-readable: provider
    /// extensions are reported without their provider).
    pub async fn requirement_sets(
        &self,
        country: &str,
        class: InventoryClass,
    ) -> DomainResult<Vec<RequirementSetRow>> {
        Ok(RequirementSetRow::find_by_statement(stmt(
            "SELECT * FROM number_kyc_requirements WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT' \
             ORDER BY selector_key",
            vec![country.into(), class.as_str().into()],
        ))
        .all(&self.db)
        .await?)
    }
}

pub(crate) async fn resolve_requirements_in<C: ConnectionTrait>(
    db: &C,
    q: &RequirementQuery,
    now: DateTime<Utc>,
) -> DomainResult<ResolvedRequirements> {
    let rows = RequirementSetRow::find_by_statement(stmt(
        "SELECT * FROM number_kyc_requirements WHERE country_code = $1 AND inventory_class = $2 AND status = 'CURRENT'",
        vec![q.country.clone().into(), q.inventory_class.as_str().into()],
    ))
    .all(db)
    .await?;
    let matches = |r: &&RequirementSetRow| {
        r.effective_at(now)
            && r.number_type
                .as_deref()
                .is_none_or(|t| t == q.number_type.as_str())
            && r.ownership_class
                .as_deref()
                .is_none_or(|o| o == q.ownership_class.as_str())
            && r.tenant_classification
                .as_deref()
                .is_none_or(|t| t == q.tenant_classification.as_str())
    };
    let specificity = |r: &RequirementSetRow| {
        (r.number_type.is_some() as u8) * 4
            + (r.ownership_class.is_some() as u8) * 2
            + r.tenant_classification.is_some() as u8
    };
    let base = rows
        .iter()
        .filter(|r| r.scope_kind == "BASE")
        .filter(matches)
        .max_by_key(|r| specificity(r))
        .ok_or_else(|| {
            DomainError::precondition(
                "KYC_REQUIREMENTS_UNKNOWN",
                "no KYC requirement set covers this country, number type and ownership class",
            )
        })?;
    let extensions: Vec<&RequirementSetRow> = rows
        .iter()
        .filter(|r| r.scope_kind == "PROVIDER_EXTENSION")
        .filter(matches)
        .filter(|r| r.provider_id.is_some() && r.provider_id == q.provider_id)
        .collect();
    let mut items = base.items();
    let mut set_ids = vec![base.id];
    let mut validity = base.approval_validity_days;
    let mut revalidation = base.revalidation_days;
    for e in &extensions {
        set_ids.push(e.id);
        validity = validity.min(e.approval_validity_days);
        revalidation = match (revalidation, e.revalidation_days) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        for item in e.items() {
            match items
                .iter_mut()
                .find(|i| i.requirement_type == item.requirement_type)
            {
                Some(existing) => {
                    existing.mandatory |= item.mandatory;
                    existing
                        .accepted_evidence
                        .retain(|k| item.accepted_evidence.contains(k));
                }
                None => items.push(item),
            }
        }
    }
    items.sort_by_key(|i| i.requirement_type);
    let kyc_mandatory = base.kyc_mandatory || items.iter().any(|i| i.mandatory);
    let synthetic = q.inventory_class == InventoryClass::Synthetic;
    let snapshot = json!({
        "requirementSetIds": set_ids,
        "versions": std::iter::once(base).chain(extensions.iter().copied()).map(|r| json!({
            "id": r.id, "scopeKind": r.scope_kind, "version": r.version,
            "source": r.source, "sourceVersion": r.source_version,
        })).collect::<Vec<_>>(),
        "kycMandatory": kyc_mandatory,
        "items": items,
        "approvalValidityDays": validity,
        "revalidationDays": revalidation,
        "nonProduction": synthetic,
        "resolvedAt": now,
    });
    Ok(ResolvedRequirements {
        set_ids,
        kyc_mandatory,
        items,
        approval_validity_days: validity,
        revalidation_days: revalidation,
        synthetic,
        snapshot,
    })
}

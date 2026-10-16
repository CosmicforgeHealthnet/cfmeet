//! Catalog and capability routes (Workstream E).
//!
//! * `GET /v1/numbers/catalog/countries/{countryCode}` (`numbers.search` or
//!   `numbers.read`): the catalog of the caller's inventory class — SANDBOX / TEST
//!   environments see the synthetic test catalog, PRODUCTION the real one. Generic
//!   entries only: provider applicability is internal.
//! * `GET /v1/numbers/assignments/{id}/capabilities` (`numbers.read`): the resolved
//!   capabilities of the caller's own assigned number.
//! * `POST /v1/admin/catalog/countries`, `POST /v1/admin/catalog/number-types`,
//!   `POST /v1/admin/catalog/number-types/{entryId}/withdraw`,
//!   `POST /v1/admin/number-resources/{id}/capabilities/{capability}/revoke`
//!   (`numbers.admin`, first-party, `X-CF-Operator`).
//!
//! The internal view (`GET /internal/v1/number-resources/{id}/capabilities`) is in
//! [`super::internal`].

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::{AdminCaller, CustomerCaller},
};
use crate::{
    domain::current_context,
    numbers::{
        catalog::{CatalogProvenance, CountryEntry, CountryEntryRow, TypeEntry, TypeEntryRow},
        resolution::CapabilityResolution,
        Capability, CatalogSource, CatalogVerification, CountrySupport, EnvironmentKind,
        InventoryClass, LocalityClass, NumberType, RestrictionFlag,
    },
    state::AppState,
};

pub const NUMBERS_SEARCH: &str = "numbers.search";

pub fn customer_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/numbers/catalog/countries/:country_code",
            get(catalog_country),
        )
        .route(
            "/numbers/assignments/:assignment_id/capabilities",
            get(assignment_capabilities),
        )
}

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/admin/catalog/countries", post(publish_country))
        .route("/admin/catalog/number-types", post(publish_number_type))
        .route(
            "/admin/catalog/number-types/:entry_id/withdraw",
            post(withdraw_number_type),
        )
        .route(
            "/admin/number-resources/:number_resource_id/capabilities/:capability/revoke",
            post(revoke_capability),
        )
}

fn ctx() -> crate::domain::RequestContext {
    current_context().unwrap_or_else(crate::domain::RequestContext::generate)
}

pub fn country_view(c: &CountryEntryRow) -> Value {
    json!({
        "entryId": c.id,
        "countryCode": c.country_code,
        "inventoryClass": c.inventory_class,
        "callingCode": c.calling_code,
        "supportStatus": c.support_status,
        "restrictionFlags": c.restriction_flags,
        "source": c.source,
        "verificationStatus": c.verification_status,
        "effectiveFrom": c.effective_from,
        "effectiveTo": c.effective_to,
        "catalogRevision": c.catalog_revision,
    })
}

pub fn type_view(t: &TypeEntryRow) -> Value {
    json!({
        "entryId": t.id,
        "countryCode": t.country_code,
        "inventoryClass": t.inventory_class,
        "numberType": t.number_type,
        "localityClass": t.locality_class,
        "region": t.region,
        "supportedCapabilities": t.supported_capabilities,
        "unsupportedCapabilities": t.unsupported_capabilities,
        "limitations": t.limitations,
        "restrictionFlags": t.restriction_flags,
        "source": t.source,
        "verificationStatus": t.verification_status,
        "effectiveFrom": t.effective_from,
        "effectiveTo": t.effective_to,
        "catalogRevision": t.catalog_revision,
    })
}

/// The contract view of a resolution (`cfmeet.numbering.common` `CapabilityResolution`).
pub fn resolution_view(r: &CapabilityResolution) -> Value {
    json!({
        "numberResourceId": r.number_resource_id,
        "state": r.state,
        "inventoryClass": r.inventory_class,
        "numberType": r.number_type,
        "resourceRevision": r.resource_revision,
        "capabilityRevision": r.capability_revision,
        "bindingRevision": r.binding_revision,
        "catalogRevision": r.catalog_revision,
        "resolvedAt": r.resolved_at,
        "restrictionFlags": r.restriction_flags,
        "capabilities": r.capabilities.iter().map(|c| json!({
            "capability": c.capability,
            "supported": c.supported,
            "usable": c.usable,
            "reason": c.reason,
            "limitations": c.limitations,
            "provenance": {
                "evidenceStatus": c.provenance.evidence_status,
                "evidenceSource": c.provenance.evidence_source,
                "verifiedAt": c.provenance.verified_at,
                "expiresAt": c.provenance.expires_at,
                "synthetic": c.provenance.synthetic,
                "carrierConfirmed": c.provenance.carrier_confirmed,
                "catalogEntryId": c.provenance.catalog_entry_id,
                "catalogEntryRevision": c.provenance.catalog_entry_revision,
                "catalogVerification": c.provenance.catalog_verification,
                "bindingRef": c.provenance.binding_ref,
            },
        })).collect::<Vec<_>>(),
        "notices": r.notices,
    })
}

fn class_of(kind: EnvironmentKind) -> InventoryClass {
    match kind {
        EnvironmentKind::Production => InventoryClass::Real,
        _ => InventoryClass::Synthetic,
    }
}

async fn catalog_country(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(country): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !(caller.0.has_scope(NUMBERS_SEARCH) || caller.0.has_scope(super::customer::NUMBERS_READ)) {
        return Err(ApiError::Code(ErrorCode::InsufficientScope));
    }
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    let class = class_of(caller.0.environment_kind);
    let entry = s
        .numbers
        .country_entry(&country, class)
        .await?
        .ok_or(ApiError::Code(ErrorCode::NotFound))?;
    let types: Vec<Value> = s
        .numbers
        .number_type_entries(&country, class)
        .await?
        .iter()
        .filter(|t| t.provider_id.is_none())
        .map(type_view)
        .collect();
    Ok(Json(json!({
        "country": country_view(&entry),
        "numberTypes": types,
        "notice": crate::numbers::limitations::EXTERNAL_VERIFICATION_NOTICE,
    })))
}

async fn assignment_capabilities(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(super::customer::NUMBERS_READ)?;
    let assignment = s.numbers.assignment(&ctx.owner_scope(), id).await?;
    let resolution = s
        .numbers
        .resolve_capabilities(assignment.number_resource_id)
        .await?;
    Ok(Json(resolution_view(&resolution)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvenanceBody {
    pub source: CatalogSource,
    pub verification_status: CatalogVerification,
    #[serde(default)]
    pub evidence_ref: Option<Uuid>,
    #[serde(default)]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub effective_to: Option<DateTime<Utc>>,
}

impl ProvenanceBody {
    fn into_provenance(self) -> CatalogProvenance {
        CatalogProvenance {
            source: self.source,
            verification: self.verification_status,
            evidence_ref: self.evidence_ref,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishCountryBody {
    pub country_code: String,
    pub inventory_class: InventoryClass,
    pub support_status: CountrySupport,
    #[serde(default)]
    pub restriction_flags: Vec<RestrictionFlag>,
    #[serde(default)]
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub provenance: ProvenanceBody,
    pub reason_code: String,
}

async fn publish_country(
    State(s): State<AppState>,
    admin: AdminCaller,
    Json(body): Json<PublishCountryBody>,
) -> Result<(axum::http::StatusCode, Json<Value>), ApiError> {
    let row = s
        .numbers
        .publish_country(
            &ctx(),
            admin.actor,
            &body.reason_code,
            CountryEntry {
                country: body.country_code,
                inventory_class: body.inventory_class,
                support: body.support_status,
                restriction_flags: body.restriction_flags,
                jurisdiction_policy_ref: body.jurisdiction_policy_ref,
                provenance: body.provenance.into_provenance(),
            },
        )
        .await?;
    Ok((axum::http::StatusCode::CREATED, Json(country_view(&row))))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishTypeBody {
    pub country_code: String,
    pub inventory_class: InventoryClass,
    pub number_type: NumberType,
    pub locality_class: LocalityClass,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub supported_capabilities: Vec<Capability>,
    #[serde(default)]
    pub unsupported_capabilities: Vec<Capability>,
    #[serde(default = "empty_object")]
    pub limitations: Value,
    #[serde(default)]
    pub restriction_flags: Vec<RestrictionFlag>,
    #[serde(default)]
    pub jurisdiction_policy_ref: Option<Uuid>,
    pub provenance: ProvenanceBody,
    pub reason_code: String,
}

fn empty_object() -> Value {
    json!({})
}

async fn publish_number_type(
    State(s): State<AppState>,
    admin: AdminCaller,
    Json(body): Json<PublishTypeBody>,
) -> Result<(axum::http::StatusCode, Json<Value>), ApiError> {
    if body.number_type == NumberType::Unclassified {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    let row = s
        .numbers
        .publish_number_type(
            &ctx(),
            admin.actor,
            &body.reason_code,
            TypeEntry {
                country: body.country_code,
                inventory_class: body.inventory_class,
                number_type: body.number_type,
                locality: body.locality_class,
                region: body.region,
                provider_id: body.provider_id,
                supported: body.supported_capabilities,
                unsupported: body.unsupported_capabilities,
                limitations: body.limitations,
                restriction_flags: body.restriction_flags,
                jurisdiction_policy_ref: body.jurisdiction_policy_ref,
                provenance: body.provenance.into_provenance(),
            },
        )
        .await?;
    Ok((axum::http::StatusCode::CREATED, Json(type_view(&row))))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasonBody {
    pub reason_code: String,
}

async fn withdraw_number_type(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(entry_id): Path<Uuid>,
    Json(body): Json<ReasonBody>,
) -> Result<Json<Value>, ApiError> {
    let revision = s
        .numbers
        .withdraw_number_type(&ctx(), admin.actor, &body.reason_code, entry_id)
        .await?;
    Ok(Json(
        json!({ "entryId": entry_id, "catalogRevision": revision }),
    ))
}

async fn revoke_capability(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path((resource_id, capability)): Path<(Uuid, String)>,
    Json(body): Json<ReasonBody>,
) -> Result<Json<Value>, ApiError> {
    let capability =
        Capability::parse(&capability).ok_or(ApiError::Code(ErrorCode::ValidationError))?;
    let change = s
        .numbers
        .revoke_capability(
            &ctx(),
            admin.actor,
            resource_id,
            capability,
            &body.reason_code,
        )
        .await?;
    Ok(Json(json!({
        "numberResourceId": resource_id,
        "capability": capability,
        "status": change.capability.status,
        "changeKind": change.change_kind,
        "capabilityRevision": change.capability_revision,
        "impactedAssignmentIds": change.impacted_assignments,
    })))
}

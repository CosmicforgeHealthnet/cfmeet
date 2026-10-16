//! KYC and eligibility routes (Workstream F).
//!
//! Customer (`/v1`, scope-checked, always the caller's TRUSTED scope; other scopes' cases
//! are "not found"):
//! * `GET  /numbers/kyc/requirements/{countryCode}/{numberType}/{ownershipClass}` (`numbers.kyc.read`)
//! * `POST /numbers/kyc/cases` (`numbers.kyc.manage`, `Idempotency-Key`)
//! * `GET  /numbers/kyc/cases/{caseId}`, `GET /numbers/kyc/cases/{caseId}/history` (`numbers.kyc.read`)
//! * `POST /numbers/kyc/cases/{caseId}/evidence` (`numbers.kyc.manage`; base64 content, ≤ 5 MiB decoded)
//! * `POST /numbers/kyc/cases/{caseId}/submit`, `POST /numbers/kyc/cases/{caseId}/appeals` (`numbers.kyc.manage`)
//! * `POST /numbers/eligibility`, `GET /numbers/eligibility/{decisionId}` (`numbers.kyc.read`)
//!
//! Administration (`numbers.admin`, first-party, `X-CF-Operator`):
//! * `POST /admin/kyc/requirements` — publish a requirement set
//! * `GET  /admin/kyc/cases/{caseId}` — case, evidence metadata, decisions, verifications
//! * `POST /admin/kyc/cases/{caseId}/decisions` — the manual reviewer's decision
//! * `POST /admin/kyc/cases/{caseId}/revoke`
//! * `POST /admin/kyc/evidence/{evidenceId}/access` — privileged, audited content read
//!
//! Evidence content appears ONLY in the privileged access response (never cached); every
//! other view carries ids, kinds, statuses and reason codes.

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
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
    identity::TrustedContext,
    numbers::{
        eligibility::{EligibilityDecisionRow, EligibilityRequest, Standing},
        kyc::{
            cases::{AppealRow, DecisionRow, EvidenceRow},
            evidence::{EvidenceBytes, MAX_EVIDENCE_BYTES},
            requirements::{RequirementItem, RequirementQuery, RequirementSet, RequirementSetRow},
            EvidenceKind, KycCaseRow, ManualReview, NewCase, RequirementScope, RequirementType,
            ReviewDecision, SubjectKind, TenantClassification,
        },
        Capability, CatalogSource, EnvironmentKind, InventoryClass, NumberType, OwnershipClass,
    },
    state::AppState,
};

pub const NUMBERS_KYC_READ: &str = "numbers.kyc.read";
pub const NUMBERS_KYC_MANAGE: &str = "numbers.kyc.manage";
pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Evidence requests carry up to 5 MiB of content as base64 inside JSON.
pub const EVIDENCE_BODY_LIMIT: usize = 8 * 1024 * 1024;

pub fn customer_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/numbers/kyc/requirements/:country_code/:number_type/:ownership_class",
            get(requirements),
        )
        .route("/numbers/kyc/cases", post(create_case))
        .route("/numbers/kyc/cases/:case_id", get(get_case))
        .route("/numbers/kyc/cases/:case_id/history", get(case_history))
        .route(
            "/numbers/kyc/cases/:case_id/evidence",
            post(attach_evidence).layer(DefaultBodyLimit::max(EVIDENCE_BODY_LIMIT)),
        )
        .route("/numbers/kyc/cases/:case_id/submit", post(submit_case))
        .route("/numbers/kyc/cases/:case_id/appeals", post(open_appeal))
        .route("/numbers/eligibility", post(evaluate_eligibility))
        .route("/numbers/eligibility/:decision_id", get(get_eligibility))
}

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/admin/kyc/requirements", post(publish_requirements))
        .route("/admin/kyc/cases/:case_id", get(admin_case))
        .route("/admin/kyc/cases/:case_id/decisions", post(review_case))
        .route("/admin/kyc/cases/:case_id/revoke", post(revoke_case))
        .route(
            "/admin/kyc/evidence/:evidence_id/access",
            post(read_evidence),
        )
}

fn ctx() -> crate::domain::RequestContext {
    current_context().unwrap_or_else(crate::domain::RequestContext::generate)
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let mut values = headers.get_all(IDEMPOTENCY_HEADER).iter();
    match (values.next(), values.next()) {
        (None, _) => Ok(None),
        (Some(v), None) => v
            .to_str()
            .ok()
            .filter(|k| crate::infra::idempotency::valid_key(k))
            .map(|k| Some(k.to_string()))
            .ok_or(ApiError::Code(ErrorCode::ValidationError)),
        _ => Err(ApiError::Code(ErrorCode::ValidationError)),
    }
}

fn class_of(kind: EnvironmentKind) -> InventoryClass {
    match kind {
        EnvironmentKind::Production => InventoryClass::Real,
        _ => InventoryClass::Synthetic,
    }
}

fn tenant_classification(c: &TrustedContext) -> TenantClassification {
    if c.first_party {
        TenantClassification::FirstParty
    } else {
        TenantClassification::External
    }
}

fn created<T: serde::Serialize>(replayed: bool, body: T) -> Response {
    let status = if replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    let mut r = (status, Json(body)).into_response();
    if replayed {
        r.headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    r
}

pub fn case_view(c: &KycCaseRow, missing: &[RequirementType]) -> Value {
    json!({
        "kycCaseId": c.id,
        "status": c.status,
        "revision": c.revision,
        "subject": { "subjectKind": c.subject_kind, "subjectRef": c.subject_ref },
        "countryCode": c.country_code,
        "inventoryClass": c.inventory_class,
        "numberType": c.number_type,
        "ownershipClass": c.ownership_class,
        "purposeCode": c.purpose_code,
        "requirements": {
            "requirementSetIds": c.requirement_set_ids,
            "items": c.requirement_snapshot["items"],
            "approvalValidityDays": c.requirement_snapshot["approvalValidityDays"],
            "nonProduction": c.requirement_snapshot["nonProduction"],
        },
        "missingRequirements": missing,
        "decisionReason": c.decision_reason,
        "verifierKind": c.verifier_kind,
        "submittedAt": c.submitted_at,
        "decidedAt": c.decided_at,
        "approvedAt": c.approved_at,
        "expiresAt": c.expires_at,
        "revalidateAt": c.revalidate_at,
        "revokedAt": c.revoked_at,
        "createdAt": c.created_at,
        "updatedAt": c.updated_at,
    })
}

pub fn evidence_view(e: &EvidenceRow) -> Value {
    json!({
        "evidenceId": e.id,
        "kycCaseId": e.case_id,
        "evidenceKind": e.evidence_kind,
        "requirementType": e.requirement_type,
        "contentType": e.content_type,
        "sizeBytes": e.size_bytes,
        "status": e.status,
        "submittedAt": e.submitted_at,
        "retentionUntil": e.retention_until,
        "purgedAt": e.purged_at,
    })
}

pub fn decision_view(d: &DecisionRow) -> Value {
    json!({
        "decisionId": d.id,
        "caseRevision": d.case_revision,
        "fromStatus": d.from_status,
        "toStatus": d.to_status,
        "decisionKind": d.decision_kind,
        "verifierKind": d.verifier_kind,
        "actorKind": d.actor_kind,
        "reasonCode": d.reason_code,
        "evidenceIds": d.evidence_ids,
        "detail": d.detail,
        "decidedAt": d.decided_at,
    })
}

pub fn appeal_view(a: &AppealRow) -> Value {
    json!({
        "appealId": a.id,
        "kycCaseId": a.case_id,
        "appealedDecisionId": a.appealed_decision_id,
        "status": a.status,
        "reasonCode": a.reason_code,
        "submittedAt": a.submitted_at,
        "resolvedAt": a.resolved_at,
        "resolutionDecisionId": a.resolution_decision_id,
    })
}

pub fn eligibility_view(d: &EligibilityDecisionRow) -> Value {
    json!({
        "eligibilityDecisionId": d.id,
        "outcome": d.outcome,
        "reasons": d.reasons,
        "subject": { "subjectKind": d.subject_kind, "subjectRef": d.subject_ref },
        "countryCode": d.country_code,
        "inventoryClass": d.inventory_class,
        "numberType": d.number_type,
        "ownershipClass": d.ownership_class,
        "numberResourceId": d.number_resource_id,
        "requestedCapabilities": d.requested_capabilities,
        "kycCaseId": d.kyc_case_id,
        "kycCaseRevision": d.kyc_case_revision,
        "requirementSetIds": d.requirement_set_ids,
        "catalogRevision": d.catalog_revision,
        "policyVersion": d.policy_version,
        "evaluatedAt": d.evaluated_at,
        "validUntil": d.valid_until,
    })
}

fn requirement_set_view(r: &RequirementSetRow, customer: bool) -> Value {
    let mut v = json!({
        "requirementSetId": r.id,
        "scopeKind": r.scope_kind,
        "countryCode": r.country_code,
        "inventoryClass": r.inventory_class,
        "numberType": r.number_type,
        "ownershipClass": r.ownership_class,
        "tenantClassification": r.tenant_classification,
        "kycMandatory": r.kyc_mandatory,
        "items": r.items,
        "approvalValidityDays": r.approval_validity_days,
        "revalidationDays": r.revalidation_days,
        "source": r.source,
        "sourceVersion": r.source_version,
        "version": r.version,
        "effectiveFrom": r.effective_from,
        "effectiveTo": r.effective_to,
    });
    if !customer {
        v["providerId"] = json!(r.provider_id);
    }
    v
}

async fn requirements(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path((country, number_type, ownership)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(NUMBERS_KYC_READ)?;
    let number_type = NumberType::parse(&number_type)
        .filter(|t| *t != NumberType::Unclassified)
        .ok_or(ApiError::Code(ErrorCode::ValidationError))?;
    let ownership =
        OwnershipClass::parse(&ownership).ok_or(ApiError::Code(ErrorCode::ValidationError))?;
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    // Customer-facing requirements are the BASE requirements (provider extensions are
    // applied when a case is opened for a serving provider).
    let resolved = s
        .numbers
        .resolve_requirements(&RequirementQuery {
            country: country.clone(),
            inventory_class: class_of(ctx.environment_kind),
            number_type,
            ownership_class: ownership,
            provider_id: None,
            tenant_classification: tenant_classification(ctx),
        })
        .await?;
    Ok(Json(json!({
        "countryCode": country,
        "inventoryClass": class_of(ctx.environment_kind),
        "numberType": number_type,
        "ownershipClass": ownership,
        "kycMandatory": resolved.kyc_mandatory,
        "requirementSetIds": resolved.set_ids,
        "items": resolved.items,
        "approvalValidityDays": resolved.approval_validity_days,
        "revalidationDays": resolved.revalidation_days,
        "nonProduction": resolved.synthetic,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCaseBody {
    pub subject_kind: SubjectKind,
    pub subject_ref: Uuid,
    pub country_code: String,
    pub number_type: NumberType,
    pub ownership_class: OwnershipClass,
    pub purpose_code: String,
}

async fn create_case(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateCaseBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_MANAGE)?;
    let key = idempotency_key(&headers)?;
    let out = s
        .kyc
        .create_case(
            &ctx(),
            &tc.owner_scope(),
            NewCase {
                subject_kind: body.subject_kind,
                subject_ref: body.subject_ref,
                country: body.country_code,
                number_type: body.number_type,
                ownership_class: body.ownership_class,
                purpose_code: body.purpose_code,
                provider_id: None,
                tenant_classification: tenant_classification(tc),
            },
            key.as_deref(),
        )
        .await?;
    let missing = s.kyc.missing_requirements(&out.value).await?;
    Ok(created(out.replayed, case_view(&out.value, &missing)))
}

async fn get_case(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_READ)?;
    let case = s.kyc.case(&tc.owner_scope(), id).await?;
    let missing = s.kyc.missing_requirements(&case).await?;
    let evidence = s.kyc.evidence(id).await?;
    let mut view = case_view(&case, &missing);
    view["evidence"] = json!(evidence.iter().map(evidence_view).collect::<Vec<_>>());
    Ok(Json(view))
}

async fn case_history(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_READ)?;
    let case = s.kyc.case(&tc.owner_scope(), id).await?;
    let decisions = s.kyc.decisions(case.id).await?;
    let appeals = s.kyc.appeals(case.id).await?;
    Ok(Json(json!({
        "kycCaseId": case.id,
        "decisions": decisions.iter().map(decision_view).collect::<Vec<_>>(),
        "appeals": appeals.iter().map(appeal_view).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceBody {
    pub evidence_kind: EvidenceKind,
    pub content_type: String,
    pub content_base64: String,
}

impl std::fmt::Debug for EvidenceBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvidenceBody")
            .field("evidence_kind", &self.evidence_kind)
            .field("content_type", &self.content_type)
            .field("content_base64", &"<redacted>")
            .finish()
    }
}

async fn attach_evidence(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<EvidenceBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_MANAGE)?;
    let key = idempotency_key(&headers)?;
    if body.content_base64.len() > MAX_EVIDENCE_BYTES.div_ceil(3) * 4 + 4 {
        return Err(ApiError::Code(ErrorCode::PayloadTooLarge));
    }
    let bytes = EvidenceBytes::new(
        base64::engine::general_purpose::STANDARD
            .decode(body.content_base64.as_bytes())
            .map_err(|_| ApiError::Code(ErrorCode::ValidationError))?,
    );
    drop(body.content_base64);
    let out = s
        .kyc
        .attach_evidence(
            &ctx(),
            &tc.owner_scope(),
            id,
            body.evidence_kind,
            &body.content_type,
            bytes,
            key.as_deref(),
        )
        .await?;
    Ok(created(out.replayed, evidence_view(&out.value)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisionBody {
    pub expected_revision: i64,
}

async fn submit_case(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RevisionBody>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_MANAGE)?;
    let key = idempotency_key(&headers)?;
    let out = s
        .kyc
        .submit_case(
            &ctx(),
            &tc.owner_scope(),
            id,
            body.expected_revision,
            key.as_deref(),
        )
        .await?;
    Ok(Json(case_view(&out.value, &[])))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasonBody {
    pub reason_code: String,
}

async fn open_appeal(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReasonBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_MANAGE)?;
    let key = idempotency_key(&headers)?;
    let out = s
        .kyc
        .open_appeal(
            &ctx(),
            &tc.owner_scope(),
            id,
            &body.reason_code,
            key.as_deref(),
        )
        .await?;
    Ok(created(out.replayed, appeal_view(&out.value)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EligibilityBody {
    pub subject_kind: SubjectKind,
    pub subject_ref: Uuid,
    pub country_code: String,
    pub number_type: NumberType,
    pub ownership_class: OwnershipClass,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub number_resource_id: Option<Uuid>,
}

async fn evaluate_eligibility(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Json(body): Json<EligibilityBody>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_READ)?;
    if body.capabilities.len() > Capability::ALL.len() {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    let decision = s
        .numbers
        .evaluate_eligibility(
            &ctx(),
            &tc.owner_scope(),
            Standing {
                active: tc.full_authority,
                first_party: tc.first_party,
                live_telecom_allowed: tc.live_telecom_allowed,
            },
            &s.eligibility,
            EligibilityRequest {
                subject_kind: body.subject_kind,
                subject_ref: body.subject_ref,
                country: body.country_code,
                number_type: body.number_type,
                ownership_class: body.ownership_class,
                capabilities: body.capabilities,
                number_resource_id: body.number_resource_id,
                provider_id: None,
            },
        )
        .await?;
    Ok(Json(eligibility_view(&decision)))
}

async fn get_eligibility(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_KYC_READ)?;
    let d = s
        .numbers
        .eligibility_decision(&tc.owner_scope(), id)
        .await?;
    Ok(Json(eligibility_view(&d)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishRequirementsBody {
    pub scope_kind: RequirementScope,
    pub country_code: String,
    pub inventory_class: InventoryClass,
    #[serde(default)]
    pub number_type: Option<NumberType>,
    #[serde(default)]
    pub ownership_class: Option<OwnershipClass>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub tenant_classification: Option<TenantClassification>,
    pub kyc_mandatory: bool,
    #[serde(default)]
    pub items: Vec<RequirementItem>,
    pub approval_validity_days: i32,
    #[serde(default)]
    pub revalidation_days: Option<i32>,
    pub source: CatalogSource,
    pub source_version: String,
    #[serde(default)]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub effective_to: Option<DateTime<Utc>>,
    pub reason_code: String,
}

async fn publish_requirements(
    State(s): State<AppState>,
    admin: AdminCaller,
    Json(b): Json<PublishRequirementsBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let row = s
        .numbers
        .publish_requirements(
            &ctx(),
            admin.actor,
            &b.reason_code,
            RequirementSet {
                scope: b.scope_kind,
                country: b.country_code,
                inventory_class: b.inventory_class,
                number_type: b.number_type,
                ownership_class: b.ownership_class,
                provider_id: b.provider_id,
                tenant_classification: b.tenant_classification,
                kyc_mandatory: b.kyc_mandatory,
                items: b.items,
                approval_validity_days: b.approval_validity_days,
                revalidation_days: b.revalidation_days,
                source: b.source,
                source_version: b.source_version,
                effective_from: b.effective_from,
                effective_to: b.effective_to,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(requirement_set_view(&row, false))))
}

async fn admin_case(
    State(s): State<AppState>,
    _admin: AdminCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let case = s
        .kyc
        .case_any(id)
        .await?
        .ok_or(ApiError::Code(ErrorCode::NotFound))?;
    let missing = s.kyc.missing_requirements(&case).await?;
    let mut view = case_view(&case, &missing);
    view["scope"] = json!({
        "tenantId": case.tenant_id,
        "applicationId": case.application_id,
        "environmentId": case.environment_id,
        "environmentKind": case.environment_kind,
    });
    view["reviewer"] = json!({ "kind": case.reviewer_kind, "ref": case.reviewer_ref });
    view["evidence"] = json!(s
        .kyc
        .evidence(id)
        .await?
        .iter()
        .map(evidence_view)
        .collect::<Vec<_>>());
    view["decisions"] = json!(s
        .kyc
        .decisions(id)
        .await?
        .iter()
        .map(decision_view)
        .collect::<Vec<_>>());
    view["appeals"] = json!(s
        .kyc
        .appeals(id)
        .await?
        .iter()
        .map(appeal_view)
        .collect::<Vec<_>>());
    view["verifications"] = json!(s
        .kyc
        .verifications(id)
        .await?
        .iter()
        .map(|v| json!({
            "verificationId": v.id,
            "caseRevision": v.case_revision,
            "purpose": v.purpose,
            "verifierKind": v.verifier_kind,
            "status": v.status,
            "outcome": v.outcome,
            "startedAt": v.started_at,
            "completedAt": v.completed_at,
        }))
        .collect::<Vec<_>>());
    Ok(Json(view))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewBody {
    pub decision: ReviewDecision,
    pub reason_code: String,
    pub evidence_ids: Vec<Uuid>,
    pub expected_revision: i64,
    #[serde(default)]
    pub missing_requirements: Vec<RequirementType>,
    #[serde(default)]
    pub valid_for_days: Option<i64>,
}

async fn review_case(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(b): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let (case, decision) = s
        .kyc
        .review_case(
            &ctx(),
            &admin.actor,
            id,
            ManualReview {
                decision: b.decision,
                reason_code: b.reason_code,
                evidence_ids: b.evidence_ids,
                expected_revision: b.expected_revision,
                missing: b.missing_requirements,
                valid_for_days: b.valid_for_days,
            },
        )
        .await?;
    let missing = s.kyc.missing_requirements(&case).await?;
    Ok(Json(json!({
        "case": case_view(&case, &missing),
        "decision": decision_view(&decision),
    })))
}

async fn revoke_case(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(b): Json<ReasonBody>,
) -> Result<Json<Value>, ApiError> {
    let case = s
        .kyc
        .revoke_case(&ctx(), &admin.actor, id, &b.reason_code)
        .await?;
    Ok(Json(case_view(&case, &[])))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessBody {
    pub purpose_code: String,
}

async fn read_evidence(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(b): Json<AccessBody>,
) -> Result<Response, ApiError> {
    let (row, bytes) = s
        .kyc
        .read_evidence(&ctx(), &admin.actor, id, &b.purpose_code)
        .await?;
    let mut view = evidence_view(&row);
    view["contentBase64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes.expose()));
    let mut r = Json(view).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

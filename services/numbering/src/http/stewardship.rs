//! Lifecycle stewardship APIs (Workstreams P–X).
//!
//! Customer (`/v1`, Platform Access credential, the caller's TRUSTED scope only — another
//! scope's record is indistinguishable from a missing one):
//!
//! * `POST /numbers/assignments/{id}/suspend`, `…/restore` (`numbers.manage`),
//!   `GET …/suspension` (`numbers.read`) — Workstream P;
//! * `POST /numbers/assignments/{id}/release` (`numbers.manage`, `Idempotency-Key`),
//!   `GET /numbers/releases/{releaseId}`, `GET /numbers/assignments/{id}/quarantine`
//!   (`numbers.read`), `POST /numbers/reassignments` (`numbers.assign`,
//!   `Idempotency-Key`) — Workstream Q;
//! * `GET /numbers/usage?from&to` (`numbers.read`) — quantities only, never provider
//!   costs (Workstream S);
//! * `GET /numbers/risk-decisions/{id}`, `POST …/appeals` (`numbers.read` /
//!   `numbers.manage`) — Workstream T;
//! * `/numbers/ports…` (`numbers.manage` / `numbers.read`, `NUMBERING_PORTING_ENABLED`)
//!   — Workstream U.
//!
//! Operators (`/v1/admin`, `numbers.admin` on a first-party application + `X-CF-Operator`):
//! suspension / restoration / release of any number, quarantine review, audited ownership
//! periods / verification / lifecycle evidence (a purpose and case reference on every
//! read), usage with provider costs, risk decisions and appeal decisions, kill switches,
//! an on-demand drift reconciliation sweep, relay-number retirement.
//!
//! Internal routes: [`super::stewardship_internal`].

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    acquisitions::standing,
    assignments::{TargetsBody, NUMBERS_ASSIGN},
    customer::{assignment_view, NUMBERS_READ},
    error::ApiError,
    middleware::{AdminCaller, CustomerCaller},
    numbers::{ctx, required_idempotency_key, NUMBERS_MANAGE},
};
use crate::{
    acquisition::{assignments::ReassignmentRequest, reservations::customer_actor},
    numbers::{eligibility::Standing, Capability, DomainError, OwnerKind, OwnershipClass},
    state::AppState,
    stewardship::{
        controls::{Control, ControlRow},
        history::{AccessJustification, AccessPurpose, OwnershipClaim},
        porting::{NewPort, PortDirection, PortRow},
        release::{NewRelease, QuarantineReview, QuarantineRow, ReleasePolicy, ReleaseRow},
        risk::DecisionRow,
        suspension::{SuspendRequest, SuspensionRow, SuspensionSource},
    },
};

pub fn customer_routes() -> Router<AppState> {
    Router::new()
        .route("/numbers/assignments/:assignment_id/suspend", post(suspend))
        .route("/numbers/assignments/:assignment_id/restore", post(restore))
        .route(
            "/numbers/assignments/:assignment_id/suspension",
            get(get_suspension),
        )
        .route("/numbers/assignments/:assignment_id/release", post(release))
        .route(
            "/numbers/assignments/:assignment_id/quarantine",
            get(get_quarantine),
        )
        .route("/numbers/releases/:release_id", get(get_release))
        .route("/numbers/reassignments", post(reassign))
        .route("/numbers/usage", get(usage))
        .route("/numbers/risk-decisions/:decision_id", get(get_decision))
        .route("/numbers/risk-decisions/:decision_id/appeals", post(appeal))
        .route("/numbers/ports", get(list_ports).post(create_port))
        .route("/numbers/ports/:port_id", get(get_port))
        .route("/numbers/ports/:port_id/evidence", post(port_evidence))
        .route("/numbers/ports/:port_id/submit", post(submit_port))
        .route("/numbers/ports/:port_id/cancel", post(cancel_port))
}

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/number-resources/:number_resource_id/suspend",
            post(admin_suspend),
        )
        .route(
            "/admin/number-resources/:number_resource_id/restore",
            post(admin_restore),
        )
        .route(
            "/admin/number-resources/:number_resource_id/release",
            post(admin_release),
        )
        .route(
            "/admin/number-resources/:number_resource_id/quarantine",
            get(admin_quarantine),
        )
        .route(
            "/admin/quarantines/:quarantine_id/review",
            post(admin_review),
        )
        .route(
            "/admin/number-resources/:number_resource_id/ownership-periods",
            post(admin_periods),
        )
        .route(
            "/admin/number-resources/:number_resource_id/ownership-verifications",
            post(admin_verify),
        )
        .route(
            "/admin/number-resources/:number_resource_id/lifecycle-evidence",
            post(admin_evidence),
        )
        .route(
            "/admin/number-resources/:number_resource_id/usage",
            get(admin_usage),
        )
        .route(
            "/admin/number-resources/:number_resource_id/risk-decisions",
            get(admin_decisions),
        )
        .route(
            "/admin/risk-decisions/:decision_id/appeal-decisions",
            post(admin_appeal_decision),
        )
        .route("/admin/controls", get(admin_controls))
        .route("/admin/controls/:control", post(admin_set_control))
        .route("/admin/reconciliation/lifecycle", post(admin_reconcile))
        .route(
            "/admin/relay-numbers/:relay_inventory_ref/retire",
            post(admin_retire_relay),
        )
}

// ---------------------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------------------

fn suspension_view(s: &SuspensionRow) -> Value {
    json!({
        "suspensionId": s.id,
        "numberResourceId": s.number_resource_id,
        "assignmentId": s.assignment_id,
        "source": s.source,
        "reasonCode": s.reason_code,
        "status": s.status,
        "providerState": s.provider_state,
        "suspendedAt": s.suspended_at,
        "restoreRequestedAt": s.restore_requested_at,
        "restoreBlockedReason": s.restore_blocked_reason,
        "restoreChecks": s.restore_checks,
        "restoredAt": s.restored_at,
        "endedReason": s.ended_reason,
        "revision": s.revision,
    })
}

fn release_view(r: &ReleaseRow) -> Value {
    json!({
        "releaseId": r.id,
        "numberResourceId": r.number_resource_id,
        "assignmentId": r.assignment_id,
        "policy": r.policy,
        "reasonCode": r.reason_code,
        "status": r.status,
        "requestedAt": r.requested_at,
        "effectiveAt": r.effective_at,
        "grantsStoppedAt": r.grants_stopped_at,
        "routingRemovedAt": r.routing_removed_at,
        "detachedAt": r.detached_at,
        "providerReleasedAt": r.provider_released_at,
        "quarantineId": r.quarantine_id,
        "failureReason": r.failure_reason,
        "revision": r.revision,
    })
}

fn quarantine_view(q: &QuarantineRow) -> Value {
    json!({
        "quarantineId": q.id,
        "numberResourceId": q.number_resource_id,
        "disposition": q.disposition,
        "policyRef": q.policy_ref,
        "cooldownDays": q.cooldown_days,
        "startedAt": q.started_at,
        "quarantineUntil": q.quarantine_until,
        "status": q.status,
        "reviewRequired": q.review_required,
        "reviewDecision": q.review_decision,
        "extensions": q.extensions,
        "providerConfirmed": q.provider_confirmed_at.is_some(),
        "completedAt": q.completed_at,
        "completionReason": q.completion_reason,
        "revision": q.revision,
    })
}

fn decision_view(d: &DecisionRow) -> Value {
    json!({
        "riskDecisionId": d.id,
        "decisionKind": d.decision_kind,
        "numberResourceId": d.number_resource_id,
        "outcome": d.outcome,
        "score": d.score,
        "reasons": d.reasons,
        "policyVersion": d.policy_version,
        "supersedesId": d.supersedes_id,
        "decidedAt": d.decided_at,
    })
}

fn port_view(p: &PortRow) -> Value {
    json!({
        "portRequestId": p.id,
        "direction": p.direction,
        "numberResourceId": p.number_resource_id,
        "assignmentId": p.assignment_id,
        "country": p.country_code,
        "ownershipClass": p.ownership_class,
        "kycCaseId": p.kyc_case_ref,
        "evidenceIds": p.evidence_refs,
        "status": p.status,
        "statusReason": p.status_reason,
        "requestedAt": p.requested_at,
        "completedAt": p.completed_at,
        "revision": p.revision,
    })
}

fn control_view(c: &ControlRow) -> Value {
    json!({
        "control": c.control_key,
        "engaged": c.engaged,
        "reasonCode": c.reason_code,
        "changedBy": c.changed_by,
        "changedAt": c.changed_at,
        "revision": c.revision,
    })
}

pub(crate) fn with_status(status: StatusCode, replayed: bool, body: Value) -> Response {
    let mut r = (if replayed { StatusCode::OK } else { status }, Json(body)).into_response();
    if replayed {
        r.headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    r
}

// ---------------------------------------------------------------------------------------
// P — suspension and restoration
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SuspendBody {
    reason_code: String,
    #[serde(default)]
    evidence_ref: Option<Uuid>,
}

async fn suspend(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<SuspendBody>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let assignment = s.numbers.assignment(&scope, id).await?;
    let source = match OwnershipClass::parse(&assignment.ownership_class).map(|c| c.owner_kind()) {
        Some(OwnerKind::Organization) => SuspensionSource::Organization,
        _ => SuspensionSource::Customer,
    };
    let row = s
        .stewardship()
        .suspend(
            &ctx(),
            Some(&scope),
            customer_actor(&scope),
            SuspendRequest {
                assignment_id: id,
                reason_code: body.reason_code,
                source,
                evidence_ref: body.evidence_ref,
            },
        )
        .await?;
    Ok(Json(suspension_view(&row)))
}

async fn restore(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let row = s
        .stewardship()
        .restore(
            &ctx(),
            Some(&scope),
            customer_actor(&scope),
            id,
            standing(tc),
        )
        .await?;
    Ok(Json(suspension_view(&row)))
}

async fn get_suspension(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let row = s
        .stewardship()
        .suspension_of(Some(&tc.owner_scope()), id)
        .await?;
    Ok(Json(suspension_view(&row)))
}

// ---------------------------------------------------------------------------------------
// Q — release, quarantine, reassignment
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseBody {
    reason_code: String,
    policy: ReleasePolicy,
    #[serde(default)]
    effective_at: Option<DateTime<Utc>>,
}

async fn release(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReleaseBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let key = required_idempotency_key(&headers)?;
    let scope = tc.owner_scope();
    let (row, replayed) = s
        .stewardship()
        .request_release(
            &ctx(),
            Some(&scope),
            customer_actor(&scope),
            NewRelease {
                assignment_id: id,
                reason_code: body.reason_code,
                policy: body.policy,
                effective_at: body.effective_at,
            },
            &key,
        )
        .await?;
    Ok(with_status(
        StatusCode::ACCEPTED,
        replayed,
        release_view(&row),
    ))
}

async fn get_release(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    Ok(Json(release_view(
        &s.stewardship()
            .release_request(Some(&tc.owner_scope()), id)
            .await?,
    )))
}

async fn get_quarantine(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    Ok(Json(quarantine_view(
        &s.stewardship()
            .quarantine_for_assignment(&tc.owner_scope(), id)
            .await?,
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReassignBody {
    number_resource_id: Uuid,
    ownership_class: OwnershipClass,
    #[serde(default)]
    owner_principal_id: Option<Uuid>,
    routing: TargetsBody,
    capabilities: Vec<Capability>,
    eligibility_decision_id: Uuid,
    #[serde(default)]
    pool_id: Option<Uuid>,
}

async fn reassign(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<ReassignBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_ASSIGN)?;
    let key = required_idempotency_key(&headers)?;
    let out = s
        .acquisition()
        .reassign(
            &ctx(),
            &tc.owner_scope(),
            standing(tc),
            ReassignmentRequest {
                number_resource_id: body.number_resource_id,
                ownership_class: body.ownership_class,
                owner_principal_id: body.owner_principal_id,
                targets: body.routing.targets(),
                capabilities: body.capabilities,
                eligibility_decision_id: body.eligibility_decision_id,
                pool_id: body.pool_id,
            },
            &key,
        )
        .await?;
    Ok(with_status(
        StatusCode::CREATED,
        out.replayed,
        json!({ "assignment": assignment_view(&out.assignment) }),
    ))
}

// ---------------------------------------------------------------------------------------
// S — usage; T — risk
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UsageQuery {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
}

async fn usage(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Query(q): Query<UsageQuery>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let rows = s
        .stewardship()
        .usage_summary(&tc.owner_scope(), q.from, q.to)
        .await?;
    // Customers see quantities; provider costs are internal evidence, never prices.
    let data: Vec<Value> = rows
        .iter()
        .map(|u| {
            json!({
                "numberResourceId": u.number_resource_id,
                "assignmentId": u.assignment_id,
                "usageKind": u.usage_kind,
                "unit": u.unit,
                "quantity": u.quantity,
                "records": u.records,
                "finalRecords": u.final_records,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "from": q.from, "to": q.to })))
}

async fn get_decision(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    Ok(Json(decision_view(
        &s.stewardship()
            .decision(Some(&tc.owner_scope()), id)
            .await?,
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppealBody {
    #[serde(default)]
    statement_ref: Option<Uuid>,
}

async fn appeal(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<AppealBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let row = s
        .stewardship()
        .file_appeal(
            &ctx(),
            &scope,
            customer_actor(&scope),
            id,
            body.statement_ref,
        )
        .await?;
    Ok(with_status(StatusCode::CREATED, false, decision_view(&row)))
}

// ---------------------------------------------------------------------------------------
// U — portability
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortBody {
    direction: PortDirection,
    #[serde(default)]
    e164: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    assignment_id: Option<Uuid>,
    ownership_class: OwnershipClass,
    #[serde(default)]
    kyc_case_id: Option<Uuid>,
    #[serde(default)]
    evidence_ids: Vec<Uuid>,
}

async fn create_port(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<PortBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let key = required_idempotency_key(&headers)?;
    let scope = tc.owner_scope();
    let (row, replayed) = s
        .stewardship()
        .request_port(
            &ctx(),
            &scope,
            customer_actor(&scope),
            NewPort {
                direction: body.direction,
                number: body.e164,
                country: body.country,
                assignment_id: body.assignment_id,
                ownership_class: body.ownership_class,
                kyc_case_id: body.kyc_case_id,
                evidence_refs: body.evidence_ids,
            },
            &key,
        )
        .await?;
    Ok(with_status(StatusCode::CREATED, replayed, port_view(&row)))
}

async fn list_ports(
    State(s): State<AppState>,
    caller: CustomerCaller,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let rows = s
        .stewardship()
        .port_requests(&tc.owner_scope(), 100)
        .await?;
    Ok(Json(
        json!({ "data": rows.iter().map(port_view).collect::<Vec<_>>() }),
    ))
}

async fn get_port(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    Ok(Json(port_view(
        &s.stewardship()
            .port_request(Some(&tc.owner_scope()), id)
            .await?,
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortEvidenceBody {
    evidence_ids: Vec<Uuid>,
    #[serde(default)]
    kyc_case_id: Option<Uuid>,
}

async fn port_evidence(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<PortEvidenceBody>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let row = s
        .stewardship()
        .add_port_evidence(
            &ctx(),
            &scope,
            customer_actor(&scope),
            id,
            body.evidence_ids,
            body.kyc_case_id,
        )
        .await?;
    Ok(Json(port_view(&row)))
}

async fn submit_port(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let row = s
        .stewardship()
        .submit_port(&ctx(), &scope, customer_actor(&scope), id)
        .await?;
    Ok(Json(port_view(&row)))
}

async fn cancel_port(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let scope = tc.owner_scope();
    let row = s
        .stewardship()
        .cancel_port(&ctx(), &scope, customer_actor(&scope), id)
        .await?;
    Ok(Json(port_view(&row)))
}

// ---------------------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------------------

async fn open_assignment(s: &AppState, resource_id: Uuid) -> Result<Uuid, ApiError> {
    Ok(s.numbers
        .open_assignment_of(resource_id)
        .await?
        .ok_or(DomainError::NotFound("assignment"))?
        .id)
}

async fn admin_suspend(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<SuspendBody>,
) -> Result<Json<Value>, ApiError> {
    let assignment_id = open_assignment(&s, id).await?;
    let row = s
        .stewardship()
        .suspend(
            &ctx(),
            None,
            admin.actor.clone(),
            SuspendRequest {
                assignment_id,
                reason_code: body.reason_code,
                source: SuspensionSource::Operator,
                evidence_ref: body.evidence_ref,
            },
        )
        .await?;
    Ok(Json(suspension_view(&row)))
}

async fn admin_restore(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let assignment_id = open_assignment(&s, id).await?;
    // The operator's own standing is not the owner's: the owner's tenant standing is
    // re-checked through eligibility / KYC; an operator restore asserts an active owner.
    let row = s
        .stewardship()
        .restore(
            &ctx(),
            None,
            admin.actor.clone(),
            assignment_id,
            Standing {
                active: true,
                first_party: false,
                live_telecom_allowed: false,
            },
        )
        .await?;
    Ok(Json(suspension_view(&row)))
}

async fn admin_release(
    State(s): State<AppState>,
    admin: AdminCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReleaseBody>,
) -> Result<Response, ApiError> {
    let key = required_idempotency_key(&headers)?;
    let assignment_id = open_assignment(&s, id).await?;
    let (row, replayed) = s
        .stewardship()
        .request_release(
            &ctx(),
            None,
            admin.actor.clone(),
            NewRelease {
                assignment_id,
                reason_code: body.reason_code,
                policy: body.policy,
                effective_at: body.effective_at,
            },
            &key,
        )
        .await?;
    Ok(with_status(
        StatusCode::ACCEPTED,
        replayed,
        release_view(&row),
    ))
}

async fn admin_quarantine(
    State(s): State<AppState>,
    _admin: AdminCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(quarantine_view(
        &s.stewardship().quarantine_of(id).await?,
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ReviewDecision {
    Approve,
    Extend,
    Hold,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewBody {
    decision: ReviewDecision,
    #[serde(default)]
    days: Option<i64>,
}

async fn admin_review(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let decision = match (body.decision, body.days) {
        (ReviewDecision::Approve, None) => QuarantineReview::Approve,
        (ReviewDecision::Hold, None) => QuarantineReview::Hold,
        (ReviewDecision::Extend, Some(days)) => QuarantineReview::Extend { days },
        _ => return Err(DomainError::Validation("days is given with EXTEND only".into()).into()),
    };
    let row = s
        .stewardship()
        .review_quarantine(&ctx(), &admin.actor, id, decision)
        .await?;
    Ok(Json(quarantine_view(&row)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JustificationBody {
    purpose: AccessPurpose,
    case_ref: String,
}

impl JustificationBody {
    fn justification(self) -> AccessJustification {
        AccessJustification {
            purpose: self.purpose,
            case_ref: self.case_ref,
        }
    }
}

async fn admin_periods(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<JustificationBody>,
) -> Result<Json<Value>, ApiError> {
    let periods = s
        .stewardship()
        .ownership_periods(&ctx(), &admin.actor, id, &body.justification())
        .await?;
    Ok(Json(json!({ "data": periods })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerifyOwnershipBody {
    tenant_id: Uuid,
    owner_ref: Uuid,
    at: DateTime<Utc>,
    purpose: AccessPurpose,
    case_ref: String,
}

async fn admin_verify(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<VerifyOwnershipBody>,
) -> Result<Json<Value>, ApiError> {
    let out = s
        .stewardship()
        .verify_ownership(
            &ctx(),
            &admin.actor,
            id,
            &OwnershipClaim {
                tenant_id: body.tenant_id,
                owner_ref: body.owner_ref,
                at: body.at,
            },
            &AccessJustification {
                purpose: body.purpose,
                case_ref: body.case_ref,
            },
        )
        .await?;
    Ok(Json(serde_json::to_value(out).expect("serializable")))
}

async fn admin_evidence(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<JustificationBody>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.stewardship()
            .lifecycle_evidence(&ctx(), &admin.actor, id, &body.justification())
            .await?,
    ))
}

async fn admin_usage(
    State(s): State<AppState>,
    _admin: AdminCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows = s.stewardship().usage_of(id).await?;
    Ok(Json(json!({ "data": rows })))
}

async fn admin_decisions(
    State(s): State<AppState>,
    _admin: AdminCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows = s.stewardship().decisions_of(id).await?;
    Ok(Json(
        json!({ "data": rows.iter().map(decision_view).collect::<Vec<_>>() }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppealDecisionBody {
    upheld: bool,
}

async fn admin_appeal_decision(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<AppealDecisionBody>,
) -> Result<Json<Value>, ApiError> {
    let row = s
        .stewardship()
        .decide_appeal(&ctx(), &admin.actor, id, body.upheld)
        .await?;
    Ok(Json(decision_view(&row)))
}

async fn admin_controls(
    State(s): State<AppState>,
    _admin: AdminCaller,
) -> Result<Json<Value>, ApiError> {
    let rows = s.stewardship().controls().await?;
    let data: Vec<Value> = Control::ALL
        .iter()
        .map(|c| {
            rows.iter()
                .find(|r| r.control_key == c.as_str())
                .map(control_view)
                .unwrap_or_else(|| json!({ "control": c.as_str(), "engaged": false }))
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlBody {
    engaged: bool,
    reason_code: String,
}

async fn admin_set_control(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(control): Path<String>,
    Json(body): Json<ControlBody>,
) -> Result<Json<Value>, ApiError> {
    let control = Control::parse(&control).ok_or(DomainError::NotFound("control"))?;
    let row = s
        .stewardship()
        .set_control(&admin.actor, control, body.engaged, &body.reason_code)
        .await?;
    Ok(Json(control_view(&row)))
}

async fn admin_reconcile(
    State(s): State<AppState>,
    _admin: AdminCaller,
) -> Result<Json<Value>, ApiError> {
    let report = s.stewardship().reconcile_lifecycle(&ctx(), 500).await?;
    Ok(Json(serde_json::to_value(report).expect("serializable")))
}

async fn admin_retire_relay(
    State(s): State<AppState>,
    admin: AdminCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = s
        .stewardship()
        .retire_relay_number(&admin.actor, id)
        .await?;
    Ok(Json(json!({
        "relayInventoryRef": row.relay_inventory_ref,
        "numberResourceId": row.number_resource_id,
        "status": row.status,
    })))
}

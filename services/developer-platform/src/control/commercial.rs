//! Commercial metadata (Section 50 §6.4): versioned plans, effective-dated plan
//! assignments and billing-account REFERENCES. No price, card, charge, tax, wallet or
//! balance exists here; future billing consumes these identifiers.
//!
//! * Plans are immutable versions (`code` + `version`); operators publish new versions
//!   and retire old ones.
//! * Assignments (tenant-wide or a stricter project assignment) are an append-only
//!   history: a new assignment supersedes the open one exactly once.
//! * The billing account is customer-maintained metadata (OWNER / BILLING): RETAIL or
//!   CONTRACT. First-party internal cost allocation (INTERNAL_COST_CENTER + cost-centre
//!   reference) is operator-only.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize,
    pagination::Page,
    stmt, Caller,
};
use crate::{
    domain::{rbac::Action, RequestContext},
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

pub const PLAN_VIEW: &str = "jsonb_build_object( \
    'id', c.id, 'code', c.code, 'version', c.version, 'displayName', c.display_name, \
    'status', c.status, 'entitlementProfileRef', c.entitlement_profile_ref, \
    'entitlements', c.entitlements, 'effectiveFrom', c.effective_from, \
    'effectiveUntil', c.effective_until, 'createdAt', c.created_at)";

const ASSIGNMENT_VIEW: &str = "jsonb_build_object( \
    'id', a.id, 'organizationId', a.tenant_id, 'projectId', a.application_id, \
    'planId', a.plan_id, 'planCode', c.code, 'planVersion', c.version, 'source', a.source, \
    'effectiveFrom', a.effective_from, 'effectiveUntil', a.effective_until, \
    'supersededBy', a.superseded_by, 'assignedBy', a.assigned_by, 'createdAt', a.created_at)";

const BILLING_VIEW: &str = "jsonb_build_object( \
    'id', b.id, 'organizationId', b.tenant_id, 'classification', b.classification, \
    'status', b.status, 'billingContactMemberId', b.billing_contact_member_id, \
    'currency', b.currency, 'country', b.country, 'externalBillingRef', b.external_billing_ref, \
    'costCenterRef', b.cost_center_ref, 'createdAt', b.created_at, 'updatedAt', b.updated_at, \
    'version', b.version)";

/// Entitlement keys a plan version may define (numbers ≥ 0; the platform API quotas
/// are enforced by the gateway, the others are published for domain enforcement).
pub const ENTITLEMENT_KEYS: &[&str] = &[
    "apiRequestsPerMinute",
    "apiRequestsPerDay",
    "concurrentCalls",
    "meetingParticipants",
    "activeNumbers",
    "smsPerDay",
    "messagingAttachmentBytes",
];

pub fn validate_entitlements(v: &Value) -> Result<(), ApiError> {
    let map = v.as_object().ok_or(ApiError(ErrorCode::ValidationError))?;
    for (k, val) in map {
        if !ENTITLEMENT_KEYS.contains(&k.as_str())
            || !val.as_u64().is_some_and(|n| n <= 10_000_000_000_000)
        {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    Ok(())
}

/// `GET /v1/commercial-plans` — ACTIVE plan versions (no prices exist).
pub async fn list_plans<C: ConnectionTrait>(db: &C) -> Result<Value, ApiError> {
    let rows = db
        .query_all(stmt(
            &format!(
                "SELECT {PLAN_VIEW} AS v FROM devplat_commercial_plans c \
                 WHERE c.status = 'ACTIVE' ORDER BY c.code, c.version DESC"
            ),
            vec![],
        ))
        .await?;
    let items: Result<Vec<Value>, _> = rows.iter().map(|r| r.try_get::<Value>("", "v")).collect();
    Ok(json!({ "items": items?, "nextCursor": null }))
}

async fn billing_account<C: ConnectionTrait>(db: &C, tenant_id: Uuid) -> Result<Value, ApiError> {
    Ok(db
        .query_one(stmt(
            &format!(
                "SELECT {BILLING_VIEW} AS v FROM devplat_billing_accounts b WHERE b.tenant_id = $1"
            ),
            vec![tenant_id.into()],
        ))
        .await?
        .map(|r| r.try_get::<Value>("", "v"))
        .transpose()?
        .unwrap_or(Value::Null))
}

/// `GET /v1/organizations/{id}/commercial` (OWNER / ADMIN / BILLING).
pub async fn organization_commercial(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::CommercialRead).await?;
    let open = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT a.application_id IS NULL AS tenant_level, {ASSIGNMENT_VIEW} AS v, {PLAN_VIEW} AS p \
                 FROM devplat_plan_assignments a JOIN devplat_commercial_plans c ON c.id = a.plan_id \
                 WHERE a.tenant_id = $1 AND a.superseded_by IS NULL ORDER BY a.created_at"
            ),
            vec![tenant_id.into()],
        ))
        .await?;
    let mut current = Value::Null;
    let mut projects = Vec::new();
    for r in &open {
        let mut v: Value = r.try_get("", "v")?;
        v["plan"] = r.try_get("", "p")?;
        if r.try_get::<bool>("", "tenant_level")? {
            current = v;
        } else {
            projects.push(v);
        }
    }
    Ok(json!({
        "organizationId": tenant_id,
        "planAssignment": current,
        "projectPlanAssignments": projects,
        "billingAccount": billing_account(&state.db, tenant_id).await?,
    }))
}

/// `GET /v1/organizations/{id}/plan-assignments` — the immutable effective history.
pub async fn assignment_history(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    page: &Page,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::CommercialRead).await?;
    let (at, id) = page
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT a.created_at AS k_at, a.id AS k_id, {ASSIGNMENT_VIEW} AS v \
                 FROM devplat_plan_assignments a JOIN devplat_commercial_plans c ON c.id = a.plan_id \
                 WHERE a.tenant_id = $1 AND ($2::timestamptz IS NULL OR (a.created_at, a.id) > ($2, $3)) \
                 ORDER BY a.created_at, a.id LIMIT $4"
            ),
            vec![
                tenant_id.into(),
                at.into(),
                id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    super::members::page_of(rows, page)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutBillingAccount {
    pub classification: String,
    #[serde(default)]
    pub billing_contact_member_id: Option<Uuid>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub external_billing_ref: Option<String>,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

fn upper_code(v: Option<&str>, len: usize) -> Result<Option<String>, ApiError> {
    match v {
        None => Ok(None),
        Some(s) if s.len() == len && s.bytes().all(|b| b.is_ascii_uppercase()) => {
            Ok(Some(s.into()))
        }
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

fn external_ref(v: Option<&str>) -> Result<Option<String>, ApiError> {
    match v {
        None => Ok(None),
        Some(s)
            if (1..=128).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b)) =>
        {
            Ok(Some(s.into()))
        }
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

/// `PUT /v1/organizations/{id}/billing-account` (OWNER / BILLING). Reference metadata
/// only: card or bank details are not accepted (unknown fields are refused).
pub async fn put_billing_account(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    body: PutBillingAccount,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::BillingAccountManage).await?;
    let classification = match body.classification.as_str() {
        "RETAIL" | "CONTRACT" => body.classification.clone(),
        // Internal cost allocation is an explicit operator decision.
        "INTERNAL_COST_CENTER" => return Err(ApiError(ErrorCode::PolicyViolation)),
        _ => return Err(ApiError(ErrorCode::ValidationError)),
    };
    let currency = upper_code(body.currency.as_deref(), 3)?;
    let country = upper_code(body.country.as_deref(), 2)?;
    let ext = external_ref(body.external_billing_ref.as_deref())?;
    let txn = state.db.begin().await?;
    if let Some(member) = body.billing_contact_member_id {
        let ok = txn
            .query_one(stmt(
                "SELECT 1 AS x FROM devplat_workspace_members WHERE id = $1 AND tenant_id = $2 AND status = 'ACTIVE'",
                vec![member.into(), tenant_id.into()],
            ))
            .await?
            .is_some();
        if !ok {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    let existing = txn
        .query_one(stmt(
            "SELECT version, classification FROM devplat_billing_accounts WHERE tenant_id = $1 FOR UPDATE",
            vec![tenant_id.into()],
        ))
        .await?;
    let now = Utc::now();
    match existing {
        None => {
            txn.execute(stmt(
                "INSERT INTO devplat_billing_accounts \
                     (id, tenant_id, classification, status, billing_contact_member_id, currency, country, \
                      external_billing_ref, created_at, updated_at) \
                 VALUES ($1, $2, $3, 'ACTIVE', $4, $5, $6, $7, $8, $8)",
                vec![
                    Uuid::new_v4().into(),
                    tenant_id.into(),
                    classification.clone().into(),
                    body.billing_contact_member_id.into(),
                    currency.into(),
                    country.into(),
                    ext.into(),
                    now.into(),
                ],
            ))
            .await?;
        }
        Some(row) => {
            let version: i64 = row.try_get("", "version")?;
            let current: String = row.try_get("", "classification")?;
            if body.expected_version.is_some_and(|v| v != version) {
                return Err(ApiError(ErrorCode::VersionConflict));
            }
            if current == "INTERNAL_COST_CENTER" {
                return Err(ApiError(ErrorCode::PolicyViolation));
            }
            txn.execute(stmt(
                "UPDATE devplat_billing_accounts SET classification = $2, billing_contact_member_id = $3, \
                     currency = $4, country = $5, external_billing_ref = $6, updated_at = $7, \
                     version = version + 1 WHERE tenant_id = $1",
                vec![
                    tenant_id.into(),
                    classification.clone().into(),
                    body.billing_contact_member_id.into(),
                    currency.into(),
                    country.into(),
                    ext.into(),
                    now.into(),
                ],
            ))
            .await?;
        }
    }
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "billing_account.updated",
            target_type: "BILLING_ACCOUNT",
            target_id: tenant_id.to_string(),
            summary: json!({ "classification": classification }),
        },
    )
    .await?;
    let v = billing_account(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(v)
}

// ---------------------------------------------------------------------------
// Operator administration
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishPlan {
    pub code: String,
    pub display_name: String,
    pub entitlement_profile_ref: String,
    pub entitlements: Value,
    #[serde(default)]
    pub effective_from: Option<DateTime<Utc>>,
}

/// Publishes the next version of a plan code.
pub async fn operator_publish_plan<C: TransactionTrait>(
    db: &C,
    body: PublishPlan,
) -> Result<Value, ApiError> {
    let code_ok = (2..=64).contains(&body.code.len())
        && body.code.as_bytes()[0].is_ascii_lowercase()
        && body
            .code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if !code_ok {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let name = super::display_name(&body.display_name)?;
    validate_entitlements(&body.entitlements)?;
    let txn = db.begin().await?;
    txn.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtext('devplat_plan:' || $1))",
        vec![body.code.clone().into()],
    ))
    .await?;
    let id = Uuid::new_v4();
    let now = Utc::now();
    txn.execute(stmt(
        "INSERT INTO devplat_commercial_plans \
             (id, code, version, display_name, status, entitlement_profile_ref, entitlements, effective_from, created_at) \
         SELECT $1, $2, COALESCE(MAX(version), 0) + 1, $3, 'ACTIVE', $4, $5, $6, $7 \
         FROM devplat_commercial_plans WHERE code = $2",
        vec![
            id.into(),
            body.code.clone().into(),
            name.into(),
            body.entitlement_profile_ref.clone().into(),
            body.entitlements.clone().into(),
            body.effective_from.unwrap_or(now).into(),
            now.into(),
        ],
    ))
    .await
    .map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let v = txn
        .query_one(stmt(
            &format!("SELECT {PLAN_VIEW} AS v FROM devplat_commercial_plans c WHERE c.id = $1"),
            vec![id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?
        .try_get::<Value>("", "v")?;
    txn.commit().await?;
    Ok(v)
}

pub async fn operator_retire_plan<C: ConnectionTrait>(
    db: &C,
    plan_id: Uuid,
) -> Result<Value, ApiError> {
    let n = db
        .execute(stmt(
            "UPDATE devplat_commercial_plans SET status = 'RETIRED', \
                 effective_until = COALESCE(effective_until, GREATEST(now(), effective_from + interval '1 microsecond')) \
             WHERE id = $1 AND status = 'ACTIVE'",
            vec![plan_id.into()],
        ))
        .await?
        .rows_affected();
    if n != 1 {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    Ok(db
        .query_one(stmt(
            &format!("SELECT {PLAN_VIEW} AS v FROM devplat_commercial_plans c WHERE c.id = $1"),
            vec![plan_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?
        .try_get::<Value>("", "v")?)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignPlan {
    pub plan_id: Uuid,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    pub source: String,
}

/// Assigns a plan to a tenant (or a project), superseding the open assignment.
pub async fn operator_assign_plan(
    state: &AppState,
    operator: &str,
    request: &RequestContext,
    tenant_id: Uuid,
    body: AssignPlan,
) -> Result<Value, ApiError> {
    if !matches!(
        body.source.as_str(),
        "INTERNAL" | "CONTRACT" | "SELF_SERVICE"
    ) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let txn = state.db.begin().await?;
    super::organization_state(&txn, tenant_id)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let plan_ok = txn
        .query_one(stmt(
            "SELECT 1 AS x FROM devplat_commercial_plans WHERE id = $1 AND status = 'ACTIVE'",
            vec![body.plan_id.into()],
        ))
        .await?
        .is_some();
    if !plan_ok {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    if let Some(p) = body.project_id {
        let ok = txn
            .query_one(stmt(
                "SELECT 1 AS x FROM devplat_project_profiles WHERE application_id = $1 AND tenant_id = $2",
                vec![p.into(), tenant_id.into()],
            ))
            .await?
            .is_some();
        if !ok {
            return Err(ApiError(ErrorCode::NotFound));
        }
    }
    let now = Utc::now();
    let id = Uuid::new_v4();
    let open = txn
        .query_one(stmt(
            "SELECT id FROM devplat_plan_assignments WHERE tenant_id = $1 \
               AND application_id IS NOT DISTINCT FROM $2 AND superseded_by IS NULL FOR UPDATE",
            vec![tenant_id.into(), body.project_id.into()],
        ))
        .await?;
    if let Some(row) = open {
        let old: Uuid = row.try_get("", "id")?;
        txn.execute(stmt(
            "UPDATE devplat_plan_assignments SET superseded_by = $2, \
                 effective_until = GREATEST($3, effective_from + interval '1 microsecond') WHERE id = $1",
            vec![old.into(), id.into(), now.into()],
        ))
        .await?;
    }
    txn.execute(stmt(
        "INSERT INTO devplat_plan_assignments \
             (id, tenant_id, application_id, plan_id, source, effective_from, assigned_by, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $6)",
        vec![
            id.into(),
            tenant_id.into(),
            body.project_id.into(),
            body.plan_id.into(),
            body.source.clone().into(),
            now.into(),
            format!("operator:{operator}").into(),
        ],
    ))
    .await?;
    activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        Event {
            tenant_id,
            action: "plan.assigned",
            target_type: "PLAN_ASSIGNMENT",
            target_id: id.to_string(),
            summary: json!({ "planId": body.plan_id, "projectId": body.project_id, "source": body.source }),
        },
    )
    .await?;
    let v = txn
        .query_one(stmt(
            &format!(
                "SELECT {ASSIGNMENT_VIEW} AS v FROM devplat_plan_assignments a \
                 JOIN devplat_commercial_plans c ON c.id = a.plan_id WHERE a.id = $1"
            ),
            vec![id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?
        .try_get::<Value>("", "v")?;
    txn.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CostCenter {
    pub cost_center_ref: String,
}

/// First-party internal cost allocation: the billing account becomes
/// INTERNAL_COST_CENTER with a cost-centre reference (operator only, audited).
pub async fn operator_set_cost_center(
    state: &AppState,
    operator: &str,
    request: &RequestContext,
    tenant_id: Uuid,
    body: CostCenter,
) -> Result<Value, ApiError> {
    let cc = body.cost_center_ref.trim();
    if !(1..=64).contains(&cc.len())
        || !cc
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let txn = state.db.begin().await?;
    super::organization_state(&txn, tenant_id)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let now = Utc::now();
    txn.execute(stmt(
        "INSERT INTO devplat_billing_accounts (id, tenant_id, classification, status, cost_center_ref, created_at, updated_at) \
         VALUES ($1, $2, 'INTERNAL_COST_CENTER', 'ACTIVE', $3, $4, $4) \
         ON CONFLICT (tenant_id) DO UPDATE SET classification = 'INTERNAL_COST_CENTER', cost_center_ref = $3, \
             external_billing_ref = NULL, updated_at = $4, version = devplat_billing_accounts.version + 1",
        vec![
            Uuid::new_v4().into(),
            tenant_id.into(),
            cc.into(),
            now.into(),
        ],
    ))
    .await?;
    activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        Event {
            tenant_id,
            action: "billing_account.cost_center_set",
            target_type: "BILLING_ACCOUNT",
            target_id: tenant_id.to_string(),
            summary: json!({ "costCenterRef": cc }),
        },
    )
    .await?;
    let v = billing_account(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entitlements_are_a_closed_numeric_vocabulary() {
        assert!(validate_entitlements(
            &json!({ "apiRequestsPerMinute": 600, "apiRequestsPerDay": 100000 })
        )
        .is_ok());
        assert!(validate_entitlements(&json!({ "pricePerMinute": 1 })).is_err());
        assert!(validate_entitlements(&json!({ "apiRequestsPerMinute": -1 })).is_err());
        assert!(validate_entitlements(&json!({ "apiRequestsPerMinute": "600" })).is_err());
        assert!(external_ref(Some("stripe:cus_123")).is_ok());
        assert!(external_ref(Some("4242 4242")).is_err());
        assert!(upper_code(Some("NGN"), 3).is_ok());
        assert!(upper_code(Some("ngn"), 3).is_err());
    }
}

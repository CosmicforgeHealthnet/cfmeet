//! Section 57 §13.3 — the organization's support / incident path (metadata only; no
//! ticketing system).
//!
//! * Members read it (machines: `organizations.read`): support tier / reference, the
//!   primary technical and incident contact MEMBER references (opaque member ids, never
//!   an email or phone number), the organization's production status (derived from the
//!   production-access workflow) and the last known platform incident affecting it.
//! * OWNER / ADMIN (machines: `organizations.manage`) set the contact references; they
//!   must be ACTIVE members of the organization.
//! * Operators set the tier / reference and record the last incident reference.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, production, stmt, Caller,
};
use crate::{
    domain::{rbac::Action, RequestContext},
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

const TIERS: &[&str] = &["COMMUNITY", "STANDARD", "PREMIUM", "ENTERPRISE", "INTERNAL"];

fn reference(v: &str) -> bool {
    let b = v.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(c))
}

/// The profile view (defaults when nothing was recorded yet).
pub async fn view<C: ConnectionTrait>(db: &C, tenant_id: Uuid) -> Result<Value, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT support_tier, support_reference, technical_contact_member_id, \
                    incident_contact_member_id, last_incident_ref, last_incident_at, updated_at, version \
             FROM devplat_support_profiles WHERE tenant_id = $1",
            vec![tenant_id.into()],
        ))
        .await?;
    let production_status = production::organization_production_status(db, tenant_id).await?;
    Ok(match row {
        Some(r) => {
            let incident_ref: Option<String> = r.try_get("", "last_incident_ref")?;
            json!({
                "organizationId": tenant_id,
                "supportTier": r.try_get::<String>("", "support_tier")?,
                "supportReference": r.try_get::<Option<String>>("", "support_reference")?,
                "primaryTechnicalContactMemberId": r.try_get::<Option<Uuid>>("", "technical_contact_member_id")?,
                "incidentContactMemberId": r.try_get::<Option<Uuid>>("", "incident_contact_member_id")?,
                "productionStatus": production_status,
                "lastIncident": incident_ref.map(|i| json!({
                    "reference": i,
                    "occurredAt": r.try_get::<Option<DateTime<Utc>>>("", "last_incident_at").ok().flatten(),
                })),
                "updatedAt": r.try_get::<DateTime<Utc>>("", "updated_at")?,
                "version": r.try_get::<i64>("", "version")?,
            })
        }
        None => json!({
            "organizationId": tenant_id,
            "supportTier": "STANDARD",
            "supportReference": null,
            "primaryTechnicalContactMemberId": null,
            "incidentContactMemberId": null,
            "productionStatus": production_status,
            "lastIncident": null,
            "updatedAt": null,
            "version": 0,
        }),
    })
}

/// `GET /v1/organizations/{id}/support-profile`.
pub async fn get(state: &AppState, caller: &Caller, tenant_id: Uuid) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::OrganizationRead).await?;
    view(&state.db, tenant_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutContacts {
    #[serde(default)]
    pub primary_technical_contact_member_id: Option<Uuid>,
    #[serde(default)]
    pub incident_contact_member_id: Option<Uuid>,
}

async fn active_member<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    member: Option<Uuid>,
) -> Result<(), ApiError> {
    if let Some(m) = member {
        let ok = db
            .query_one(stmt(
                "SELECT 1 FROM devplat_workspace_members WHERE id = $1 AND tenant_id = $2 AND status = 'ACTIVE'",
                vec![m.into(), tenant.into()],
            ))
            .await?
            .is_some();
        if !ok {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    Ok(())
}

/// `PUT /v1/organizations/{id}/support-profile` (contact references).
pub async fn put(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    body: PutContacts,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::SupportProfileManage).await?;
    active_member(
        &state.db,
        tenant_id,
        body.primary_technical_contact_member_id,
    )
    .await?;
    active_member(&state.db, tenant_id, body.incident_contact_member_id).await?;
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_support_profiles (tenant_id, support_tier, technical_contact_member_id, \
           incident_contact_member_id, updated_by, updated_at) VALUES ($1, 'STANDARD', $2, $3, $4, $5) \
         ON CONFLICT (tenant_id) DO UPDATE SET technical_contact_member_id = EXCLUDED.technical_contact_member_id, \
           incident_contact_member_id = EXCLUDED.incident_contact_member_id, updated_by = EXCLUDED.updated_by, \
           updated_at = EXCLUDED.updated_at, version = devplat_support_profiles.version + 1",
        vec![
            tenant_id.into(),
            body.primary_technical_contact_member_id.into(),
            body.incident_contact_member_id.into(),
            caller.actor_ref().into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "support_profile.updated",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({
                "primaryTechnicalContactMemberId": body.primary_technical_contact_member_id,
                "incidentContactMemberId": body.incident_contact_member_id,
            }),
        },
    )
    .await?;
    let v = view(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorProfile {
    pub support_tier: String,
    #[serde(default)]
    pub support_reference: Option<String>,
}

/// `PUT /internal/v1/operator/organizations/{id}/support-profile`.
pub async fn operator_put(
    state: &AppState,
    operator: &str,
    request: &RequestContext,
    tenant_id: Uuid,
    body: OperatorProfile,
) -> Result<Value, ApiError> {
    if !TIERS.contains(&body.support_tier.as_str())
        || body
            .support_reference
            .as_deref()
            .is_some_and(|r| !reference(r))
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    if super::organization_state(&state.db, tenant_id)
        .await?
        .is_none()
    {
        return Err(ApiError(ErrorCode::NotFound));
    }
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_support_profiles (tenant_id, support_tier, support_reference, updated_by, updated_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id) DO UPDATE SET support_tier = EXCLUDED.support_tier, \
           support_reference = EXCLUDED.support_reference, updated_by = EXCLUDED.updated_by, \
           updated_at = EXCLUDED.updated_at, version = devplat_support_profiles.version + 1",
        vec![
            tenant_id.into(),
            body.support_tier.clone().into(),
            body.support_reference.clone().into(),
            format!("operator:{operator}").into(),
            Utc::now().into(),
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
            action: "support_profile.tier_set",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({ "supportTier": body.support_tier, "supportReference": body.support_reference }),
        },
    )
    .await?;
    let v = view(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Incident {
    pub incident_ref: String,
    pub occurred_at: DateTime<Utc>,
}

/// `POST /internal/v1/operator/organizations/{id}/incidents` — the last known platform
/// incident affecting the organization (a reference to the incident record kept by the
/// operations tooling; no ticket content).
pub async fn operator_incident(
    state: &AppState,
    operator: &str,
    request: &RequestContext,
    tenant_id: Uuid,
    body: Incident,
) -> Result<Value, ApiError> {
    if !reference(&body.incident_ref)
        || body.occurred_at > Utc::now() + chrono::Duration::minutes(5)
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    if super::organization_state(&state.db, tenant_id)
        .await?
        .is_none()
    {
        return Err(ApiError(ErrorCode::NotFound));
    }
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_support_profiles (tenant_id, support_tier, last_incident_ref, last_incident_at, \
           updated_by, updated_at) VALUES ($1, 'STANDARD', $2, $3, $4, $5) \
         ON CONFLICT (tenant_id) DO UPDATE SET last_incident_ref = EXCLUDED.last_incident_ref, \
           last_incident_at = EXCLUDED.last_incident_at, updated_by = EXCLUDED.updated_by, \
           updated_at = EXCLUDED.updated_at, version = devplat_support_profiles.version + 1",
        vec![
            tenant_id.into(),
            body.incident_ref.clone().into(),
            body.occurred_at.into(),
            format!("operator:{operator}").into(),
            Utc::now().into(),
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
            action: "support_profile.incident_recorded",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({ "incidentRef": body.incident_ref, "occurredAt": body.occurred_at }),
        },
    )
    .await?;
    let v = view(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(v)
}

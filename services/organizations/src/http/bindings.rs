//! Per-organization service bindings of the organization's technical scope (Blueprint
//! Section 12 follow-up; CAL-007 / ORG-012 / CAL-009; contract §8.5).
//!
//! An operator who created a Platform Access service account and credential INSIDE the
//! organization's own FIRST_PARTY triple (an explicit operator decision, ADR 0058 §1)
//! records their ids here, so the Meeting Service can forward an organization's
//! Enterprise Calling requests with that credential named in its signed delegated
//! context. Organizations stores identifiers only — never a credential value — and
//! verifies nothing about them: Platform Access re-verifies the credential against the
//! triple on every use, so a wrong binding can only refuse.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{middleware::Caller, parse, respond};
use crate::{
    db,
    domain::LifecycleStatus,
    error::{ApiError, ApiResult, ErrorCode},
    provisioning,
    state::AppState,
    store,
};

/// The only consuming service of a binding in this version.
pub const CALLING: &str = "calling";

/// One recorded binding.
#[derive(Debug, Clone, FromQueryResult)]
pub struct BindingRow {
    pub organization_id: Uuid,
    pub capability: String,
    pub service: String,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub bound_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

impl BindingRow {
    pub fn to_json(&self) -> Value {
        json!({
            "organizationId": self.organization_id,
            "capability": self.capability,
            "service": self.service,
            "serviceAccountId": self.service_account_id,
            "credentialId": self.credential_id,
            "version": self.version,
            "boundBy": self.bound_by,
            "createdAt": store::rfc3339(self.created_at),
            "updatedAt": store::rfc3339(self.updated_at),
        })
    }

    /// The compact form carried by `scopes/resolve`.
    pub fn to_resolve_json(&self) -> Value {
        json!({
            "serviceAccountId": self.service_account_id,
            "credentialId": self.credential_id,
            "version": self.version,
        })
    }
}

const BINDING_COLUMNS: &str = "organization_id, capability, service, service_account_id, credential_id, bound_by, created_at, updated_at, version";

/// The binding of `service` for the organization's `capability` scope, if any.
pub async fn binding<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    capability: &str,
    service: &str,
    lock: bool,
) -> Result<Option<BindingRow>, DbErr> {
    db::one(
        c,
        &format!(
            "SELECT {BINDING_COLUMNS} FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![organization.into(), capability.into(), service.into()],
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutBody {
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    /// The current binding version (0 = no binding yet); a mismatch is VERSION_CONFLICT.
    #[serde(default)]
    pub expected_version: Option<i64>,
}

fn not_bound() -> ApiError {
    ApiError::with_message(
        ErrorCode::NotFound,
        "No binding is recorded for this service.",
    )
}

/// `GET /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling`.
pub async fn get_calling(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    store::org(&state.db, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let row = binding(&state.db, id, provisioning::CAPABILITY, CALLING, false)
        .await?
        .ok_or_else(not_bound)?;
    Ok(respond(StatusCode::OK, row.to_json()))
}

/// `PUT /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling`
/// `{serviceAccountId, credentialId, expectedVersion?}` — create or replace; repeating the
/// recorded ids is a no-op (same version). Refused for an archived organization and while
/// the organization has no recorded triple (`PROVISIONING_PENDING`).
pub async fn put_calling(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let (req, _): (PutBody, Value) = parse(&body)?;
    if req.service_account_id.is_nil() || req.credential_id.is_nil() {
        return Err(ApiError::validation(
            "serviceAccountId and credentialId must be non-nil UUIDs",
        ));
    }
    if req.expected_version.is_some_and(|v| v < 0) {
        return Err(ApiError::validation("expectedVersion must be at least 0"));
    }
    let txn = state.db.begin().await?;
    let org = store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if org.status() == LifecycleStatus::Archived {
        return Err(ApiError::new(ErrorCode::OrganizationArchived));
    }
    let has_triple = provisioning::mapping(&txn, id, provisioning::CAPABILITY, false)
        .await?
        .is_some_and(|m| m.tenant_id.is_some());
    if !has_triple {
        return Err(ApiError::new(ErrorCode::ProvisioningPending));
    }
    let current = binding(&txn, id, provisioning::CAPABILITY, CALLING, true).await?;
    let current_version = current.as_ref().map(|b| b.version).unwrap_or(0);
    if req.expected_version.is_some_and(|v| v != current_version) {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    if let Some(b) = current.as_ref().filter(|b| {
        b.service_account_id == req.service_account_id && b.credential_id == req.credential_id
    }) {
        // Unchanged: idempotent, nothing written.
        let doc = b.to_json();
        txn.commit().await?;
        return Ok(respond(StatusCode::OK, doc));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_operator();
    db::exec(
        &txn,
        "INSERT INTO organization_scope_service_bindings \
           (organization_id, capability, service, service_account_id, credential_id, bound_by, created_at, updated_at, version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7, 1) \
         ON CONFLICT (organization_id, capability, service) DO UPDATE SET \
           service_account_id = EXCLUDED.service_account_id, credential_id = EXCLUDED.credential_id, \
           bound_by = EXCLUDED.bound_by, updated_at = EXCLUDED.updated_at, \
           version = organization_scope_service_bindings.version + 1",
        vec![
            id.into(),
            provisioning::CAPABILITY.into(),
            CALLING.into(),
            req.service_account_id.into(),
            req.credential_id.into(),
            ctx.user_id.into(),
            now.into(),
        ],
    )
    .await?;
    let row = binding(&txn, id, provisioning::CAPABILITY, CALLING, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::InternalError))?;
    // Ids only (they are not secrets); the audit summary says what changed.
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "scope.service_binding_set",
        "organization",
        Some(id),
        json!({
            "capability": provisioning::CAPABILITY,
            "service": CALLING,
            "serviceAccountId": row.service_account_id,
            "credentialId": row.credential_id,
            "version": row.version,
            "replaced": current.is_some(),
        }),
    )
    .await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, row.to_json()))
}

/// `DELETE /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling`
/// — idempotent; also allowed for an archived organization (removal only narrows).
pub async fn delete_calling(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let txn = state.db.begin().await?;
    store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let current = binding(&txn, id, provisioning::CAPABILITY, CALLING, true).await?;
    let removed = current.is_some();
    if let Some(b) = current {
        db::exec(
            &txn,
            "DELETE FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3",
            vec![id.into(), provisioning::CAPABILITY.into(), CALLING.into()],
        )
        .await?;
        let now = db::now(&txn).await?;
        store::audit(
            &txn,
            &caller.as_operator(),
            now,
            Some(id),
            "scope.service_binding_cleared",
            "organization",
            Some(id),
            json!({
                "capability": provisioning::CAPABILITY,
                "service": CALLING,
                "serviceAccountId": b.service_account_id,
                "credentialId": b.credential_id,
                "version": b.version,
            }),
        )
        .await?;
    }
    txn.commit().await?;
    Ok(respond(
        StatusCode::OK,
        json!({ "organizationId": id, "capability": provisioning::CAPABILITY, "service": CALLING, "removed": removed }),
    ))
}

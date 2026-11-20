//! Per-organization service bindings of the organization's technical scope (Blueprint
//! Section 12 follow-up; CAL-007 / ORG-012 / CAL-009; contract §8.5).
//!
//! A binding names the Platform Access service account and credential INSIDE the
//! organization's own FIRST_PARTY triple that the Meeting Service uses for the
//! organization's Calling / Messaging requests (signed delegated context). Since ADR 0058
//! Amendment A2 the provisioning saga creates and verifies them automatically
//! (`source = AUTOMATIC`, see [`crate::provisioning`]); an operator may still record one
//! manually (`source = OPERATOR`, break-glass), which automation never replaces.
//! Organizations stores identifiers only — never a credential value; Platform Access
//! re-verifies the credential against the triple on every use, so a wrong binding can
//! only refuse.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    admin_bridge::{self, command, BridgeCommand, Outcome, Risk, Subject},
    middleware::Caller,
    parse, respond,
};
use crate::{
    db,
    domain::LifecycleStatus,
    error::{ApiError, ApiResult, ErrorCode},
    provisioning,
    state::AppState,
    store::{self, OpContext},
};

/// Binding sources (`organization_scope_service_bindings.source`).
pub mod provisioning_source {
    pub const AUTOMATIC: &str = "AUTOMATIC";
    pub const OPERATOR: &str = "OPERATOR";
}

/// Consuming services of a binding: Calling (Section 12 follow-up) and Messaging (final
/// closure: organization-scope Messaging through the Meeting relay).
pub const CALLING: &str = "calling";
pub const MESSAGING: &str = "messaging";
pub const SERVICES: &[&str] = &[CALLING, MESSAGING];

/// The binding service named in the path; unknown services look like a missing binding.
fn service_of(name: &str) -> Result<&'static str, ApiError> {
    SERVICES
        .iter()
        .copied()
        .find(|s| *s == name)
        .ok_or_else(not_bound)
}

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
    pub source: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub credential_expires_at: Option<DateTime<Utc>>,
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
            "source": self.source,
            "verifiedAt": self.verified_at.map(store::rfc3339),
            "credentialExpiresAt": self.credential_expires_at.map(store::rfc3339),
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

const BINDING_COLUMNS: &str = "organization_id, capability, service, service_account_id, credential_id, bound_by, created_at, updated_at, version, source, verified_at, credential_expires_at";

/// The binding of `service` for the organization's `capability` scope, if any (a binding
/// revoked locally — archive, operator removal of an automatic one — is not a binding).
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
            "SELECT {BINDING_COLUMNS} FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3 AND revoked_at IS NULL{}",
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

/// `GET /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}`.
pub async fn get_binding(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, service)): Path<(Uuid, String)>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let service = service_of(&service)?;
    store::org(&state.db, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let row = binding(&state.db, id, provisioning::CAPABILITY, service, false)
        .await?
        .ok_or_else(not_bound)?;
    Ok(respond(StatusCode::OK, row.to_json()))
}

/// `PUT /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}`
/// `{serviceAccountId, credentialId, expectedVersion?}` — create or replace; repeating the
/// recorded ids is a no-op (same version). Refused for an archived organization and while
/// the organization has no recorded triple (`PROVISIONING_PENDING`).
pub async fn put_binding(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, service)): Path<(Uuid, String)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let service = service_of(&service)?;
    let (req, _): (PutBody, Value) = parse(&body)?;
    if req.expected_version.is_some_and(|v| v < 0) {
        return Err(ApiError::validation("expectedVersion must be at least 0"));
    }
    let txn = state.db.begin().await?;
    let row = set_binding(
        &txn,
        &caller.as_operator(),
        id,
        service,
        req.service_account_id,
        req.credential_id,
        req.expected_version,
    )
    .await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, row.to_json()))
}

/// Creates or replaces the OPERATOR binding of `service` (in the caller's transaction,
/// which this locks the organization row of): refused for an archived organization, while
/// the organization has no recorded triple (`PROVISIONING_PENDING`), while a previous
/// binding's revocation is pending, or at another version than `expected` (0 = none).
/// Repeating the recorded ids is a no-op (same version).
async fn set_binding(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    id: Uuid,
    service: &'static str,
    service_account_id: Uuid,
    credential_id: Uuid,
    expected: Option<i64>,
) -> Result<BindingRow, ApiError> {
    if service_account_id.is_nil() || credential_id.is_nil() {
        return Err(ApiError::validation(
            "serviceAccountId and credentialId must be non-nil UUIDs",
        ));
    }
    let org = store::org(txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if org.status() == LifecycleStatus::Archived {
        return Err(ApiError::new(ErrorCode::OrganizationArchived));
    }
    let has_triple = provisioning::mapping(txn, id, provisioning::CAPABILITY, false)
        .await?
        .is_some_and(|m| m.tenant_id.is_some());
    if !has_triple {
        return Err(ApiError::new(ErrorCode::ProvisioningPending));
    }
    // A revoked automatic binding whose credential Platform Access has not revoked yet
    // must not be overwritten (the revocation would be lost).
    let revoking = db::scalar_i64(
        txn,
        "SELECT count(*)::bigint AS n FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3 AND revoked_at IS NOT NULL AND revocation_applied_at IS NULL",
        vec![id.into(), provisioning::CAPABILITY.into(), service.into()],
    )
    .await?;
    if revoking > 0 {
        return Err(ApiError::with_message(
            ErrorCode::VersionConflict,
            "The previous binding's revocation is still being applied; retry shortly.",
        ));
    }
    let current = binding(txn, id, provisioning::CAPABILITY, service, true).await?;
    let current_version = current.as_ref().map(|b| b.version).unwrap_or(0);
    if expected.is_some_and(|v| v != current_version) {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    if let Some(b) = current
        .as_ref()
        .filter(|b| b.service_account_id == service_account_id && b.credential_id == credential_id)
    {
        // Unchanged: idempotent, nothing written.
        return Ok(b.clone());
    }
    let now = db::now(txn).await?;
    db::exec(
        txn,
        "INSERT INTO organization_scope_service_bindings \
           (organization_id, capability, service, service_account_id, credential_id, bound_by, created_at, updated_at, version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7, 1) \
         ON CONFLICT (organization_id, capability, service) DO UPDATE SET \
           service_account_id = EXCLUDED.service_account_id, credential_id = EXCLUDED.credential_id, \
           bound_by = EXCLUDED.bound_by, updated_at = EXCLUDED.updated_at, \
           version = organization_scope_service_bindings.version + 1, source = 'OPERATOR', \
           verified_at = NULL, credential_expires_at = NULL, revoked_at = NULL, revocation_applied_at = NULL, \
           revocation_attempts = 0, revocation_next_at = NULL, revocation_last_error_class = NULL",
        vec![
            id.into(),
            provisioning::CAPABILITY.into(),
            service.into(),
            service_account_id.into(),
            credential_id.into(),
            ctx.user_id.into(),
            now.into(),
        ],
    )
    .await?;
    let row = binding(txn, id, provisioning::CAPABILITY, service, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::InternalError))?;
    // Ids only (they are not secrets); the audit summary says what changed.
    store::audit(
        txn,
        ctx,
        now,
        Some(id),
        "scope.service_binding_set",
        "organization",
        Some(id),
        json!({
            "capability": provisioning::CAPABILITY,
            "service": service,
            "serviceAccountId": row.service_account_id,
            "credentialId": row.credential_id,
            "version": row.version,
            "replaced": current.is_some(),
            "source": "OPERATOR",
        }),
    )
    .await?;
    Ok(row)
}

/// `DELETE /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}`
/// — idempotent; also allowed for an archived organization (removal only narrows).
pub async fn delete_binding(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, service)): Path<(Uuid, String)>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let service = service_of(&service)?;
    let txn = state.db.begin().await?;
    let removed = clear_binding(&txn, &caller.as_operator(), id, service, None)
        .await?
        .is_some();
    txn.commit().await?;
    Ok(respond(
        StatusCode::OK,
        json!({ "organizationId": id, "capability": provisioning::CAPABILITY, "service": service, "removed": removed }),
    ))
}

/// Removes the binding of `service` (in the caller's transaction, which this locks the
/// organization row of), at `expected` (0 = none) when given: an OPERATOR binding is
/// deleted, an AUTOMATIC one is revoked locally and at Platform Access (the revocation
/// sync). `Some(source)` of the removed binding; `None` when there was none (idempotent).
async fn clear_binding(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    id: Uuid,
    service: &'static str,
    expected: Option<i64>,
) -> Result<Option<String>, ApiError> {
    store::org(txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let current = binding(txn, id, provisioning::CAPABILITY, service, true).await?;
    if expected.is_some_and(|v| v != current.as_ref().map(|b| b.version).unwrap_or(0)) {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    let source = current.as_ref().map(|b| b.source.clone());
    if let Some(b) = current
        .as_ref()
        .filter(|b| b.source == provisioning_source::AUTOMATIC)
    {
        // An automatic binding's credential is revoked at Platform Access (the revocation
        // sync, retried until applied); it is not recreated automatically.
        let now = db::now(txn).await?;
        provisioning::request_binding_revocation(txn, ctx, now, id, Some(&b.service)).await?;
    } else if let Some(b) = current {
        db::exec(
            txn,
            "DELETE FROM organization_scope_service_bindings WHERE organization_id = $1 AND capability = $2 AND service = $3 AND revoked_at IS NULL",
            vec![id.into(), provisioning::CAPABILITY.into(), service.into()],
        )
        .await?;
        let now = db::now(txn).await?;
        store::audit(
            txn,
            ctx,
            now,
            Some(id),
            "scope.service_binding_cleared",
            "organization",
            Some(id),
            json!({
                "capability": provisioning::CAPABILITY,
                "service": service,
                "serviceAccountId": b.service_account_id,
                "credentialId": b.credential_id,
                "version": b.version,
            }),
        )
        .await?;
    }
    Ok(source)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeBody {
    /// The current binding version (0 = no binding).
    pub expected_version: Option<i64>,
    /// `SET` (record the OPERATOR binding) or `CLEAR` (remove it).
    pub action: String,
    #[serde(default)]
    pub service_account_id: Option<Uuid>,
    #[serde(default)]
    pub credential_id: Option<Uuid>,
}

fn subject_of(service: &str) -> Subject {
    if service == MESSAGING {
        Subject::MessagingBinding
    } else {
        Subject::CallingBinding
    }
}

/// The admin bridge's answer for a binding: `BOUND` with its version, or `REMOVED`
/// (`null` version: the next `SET` expects 0) / `REVOKED` (an automatic binding whose
/// credential Platform Access revokes). Platform Access ids are not echoed.
fn binding_outcome(id: Uuid, service: &str, row: Option<&BindingRow>, cleared: &str) -> Outcome {
    match row {
        Some(b) => Outcome::new(
            "BOUND",
            Some(b.version),
            json!({
                "organizationId": id,
                "service": service,
                "capability": b.capability,
                "source": b.source,
                "version": b.version,
                "verifiedAt": b.verified_at.map(store::rfc3339),
                "updatedAt": store::rfc3339(b.updated_at),
            }),
        ),
        None => Outcome::new(
            cleared,
            None,
            json!({
                "organizationId": id,
                "service": service,
                "capability": provisioning::CAPABILITY,
                "source": Value::Null,
                "version": Value::Null,
                "verifiedAt": Value::Null,
                "updatedAt": Value::Null,
            }),
        ),
    }
}

/// `POST /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}/change`
/// `{expectedVersion, action: SET | CLEAR, serviceAccountId?, credentialId?}` — the admin
/// bridge's binding change (R3, two people; bridge only): the same rules as `PUT` (SET,
/// both ids required) and `DELETE` (CLEAR), at the binding's version (0 = none).
/// Credential rotation is not offered to the bridge.
pub async fn admin_change_binding(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path((id, service)): Path<(Uuid, String)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let b = BridgeCommand::required(&caller, &headers, Risk::R3)?;
    let service = service_of(&service)?;
    let (req, _): (ChangeBody, Value) = parse(&body)?;
    let expected = admin_bridge::expected_version(req.expected_version, 0)?;
    let set = match (
        req.action.as_str(),
        req.service_account_id,
        req.credential_id,
    ) {
        ("SET", Some(sa), Some(cred)) => Some((sa, cred)),
        ("SET", _, _) => {
            return Err(ApiError::validation(
                "SET requires serviceAccountId and credentialId",
            ))
        }
        ("CLEAR", None, None) => None,
        ("CLEAR", _, _) => {
            return Err(ApiError::validation(
                "CLEAR takes no serviceAccountId or credentialId",
            ))
        }
        _ => return Err(ApiError::validation("action must be SET or CLEAR")),
    };
    let subject = subject_of(service);
    let txn = state.db.begin().await?;
    if let Some(r) =
        admin_bridge::replayed(&txn, &b, command::BINDING_CHANGE, subject, Some(id)).await?
    {
        store::org(&txn, id, false)
            .await?
            .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
        let row = binding(&txn, id, provisioning::CAPABILITY, service, false).await?;
        txn.commit().await?;
        let cleared = if r.state == "REVOKED" {
            "REVOKED"
        } else {
            "REMOVED"
        };
        return Ok(binding_outcome(id, service, row.as_ref(), cleared).response());
    }
    let ctx = caller.as_operator();
    let outcome = match set {
        Some((sa, cred)) => {
            let row = set_binding(&txn, &ctx, id, service, sa, cred, Some(expected)).await?;
            binding_outcome(id, service, Some(&row), "REMOVED")
        }
        None => {
            let source = clear_binding(&txn, &ctx, id, service, Some(expected)).await?;
            let cleared = if source.as_deref() == Some(provisioning_source::AUTOMATIC) {
                "REVOKED"
            } else {
                "REMOVED"
            };
            binding_outcome(id, service, None, cleared)
        }
    };
    let now = db::now(&txn).await?;
    admin_bridge::record(
        &txn,
        &b,
        command::BINDING_CHANGE,
        subject,
        id,
        Some(id),
        &outcome,
        now,
    )
    .await?;
    txn.commit().await?;
    Ok(outcome.response())
}

/// `POST /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}/rotate`
/// — operator credential rotation of an AUTOMATIC binding: Platform Access issues a
/// successor credential (the predecessor is revoked), Organizations verifies the answer
/// server-side and records the new credential id (version + 1, audited
/// `scope.service_binding_rotated`). Refused for an operator binding (replace it with
/// `PUT`), an archived organization, or while automatic bindings are not enabled.
pub async fn rotate_binding(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, service)): Path<(Uuid, String)>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let service = service_of(&service)?;
    let org = store::org(&state.db, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if org.status() == LifecycleStatus::Archived {
        return Err(ApiError::new(ErrorCode::OrganizationArchived));
    }
    let Some(port) = state.platform.clone().filter(|p| p.bindings_enabled()) else {
        return Err(ApiError::with_message(
            ErrorCode::ServiceUnavailable,
            "Automatic service bindings are not enabled.",
        ));
    };
    let current = binding(&state.db, id, provisioning::CAPABILITY, service, false)
        .await?
        .ok_or_else(not_bound)?;
    if current.source != provisioning_source::AUTOMATIC {
        return Err(ApiError::validation(
            "Only automatic bindings are rotated here; replace an operator binding with PUT",
        ));
    }
    let triple = provisioning::mapping(&state.db, id, provisioning::CAPABILITY, false)
        .await?
        .and_then(|m| {
            m.tenant_id
                .zip(m.application_id)
                .zip(m.environment_id)
                .map(|((t, a), e)| (t, a, e))
        })
        .ok_or(ApiError::new(ErrorCode::ProvisioningPending))?;
    let facts = match port
        .rotate_binding(id, provisioning::CAPABILITY, service)
        .await
    {
        Ok(f) => f,
        Err(provisioning::PortError::Refused { status: 404, .. }) => return Err(not_bound()),
        Err(e) => {
            tracing::warn!(organization_id = %id, service, error_class = %e.binding_class(), "binding rotation failed");
            return Err(ApiError::new(ErrorCode::ServiceUnavailable));
        }
    };
    if !facts.verifies(id, provisioning::CAPABILITY, service, triple)
        || facts.service_account_id != current.service_account_id
    {
        tracing::error!(organization_id = %id, service, "rotated binding does not verify");
        return Err(ApiError::new(ErrorCode::ServiceUnavailable));
    }
    let txn = state.db.begin().await?;
    store::org(&txn, id, true).await?;
    let now = db::now(&txn).await?;
    provisioning::upsert_automatic_binding(
        &txn,
        &caller.as_operator(),
        now,
        id,
        provisioning::CAPABILITY,
        service,
        &facts,
    )
    .await?;
    let row = binding(&txn, id, provisioning::CAPABILITY, service, false)
        .await?
        .ok_or_else(not_bound)?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, row.to_json()))
}

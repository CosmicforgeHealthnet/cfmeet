//! Organization policies (Blueprint Section 6): read, compare-and-swap update and the
//! effective constraints owner services combine with their own policy (stricter wins).

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use cf_service_auth::InternalPermission;
use sea_orm::TransactionTrait;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    admin_bridge::{self, command, BridgeCommand, Outcome, Risk, Subject},
    authorize,
    middleware::Caller,
    parse, respond,
};
use crate::{
    db,
    domain::{Capability, LifecycleStatus, Role},
    error::{ApiError, ApiResult, ErrorCode},
    policy::{changed_keys, merge, PolicyDocument, OWNER_ONLY_SECTIONS},
    state::AppState,
    store::{self, Event, OpContext, OrgRow, PolicyRow},
};

pub(super) fn policy_json(org: &OrgRow, row: Option<&PolicyRow>, doc: &PolicyDocument) -> Value {
    json!({
        "schemaVersion": 1,
        "organizationId": org.id,
        "revision": row.map(|r| r.revision).unwrap_or(0),
        "policy": doc.to_json(),
        "updatedBy": row.and_then(|r| r.updated_by),
        "updatedAt": row.map(|r| store::rfc3339(r.updated_at)),
        "legacyMeetingProfilePolicy": org.profile_policy,
    })
}

/// The organization for a read: an actor needs `organization.read`; a service read (no
/// actor) gets the authoritative state.
async fn read_org(state: &AppState, caller: &Caller, id: Uuid) -> Result<OrgRow, ApiError> {
    Ok(match caller.acting_user {
        Some(user) => {
            authorize(&state.db, id, user, Capability::OrganizationRead, false)
                .await?
                .org
        }
        None => store::org(&state.db, id, false)
            .await?
            .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?,
    })
}

/// `GET /internal/v1/organizations/{id}/policies`.
pub async fn get(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let org = read_org(&state, &caller, id).await?;
    let row = store::policy_row(&state.db, id, false).await?;
    let doc = store::parse_policy(row.as_ref().map(|r| &r.document))?;
    Ok(respond(
        StatusCode::OK,
        policy_json(&org, row.as_ref(), &doc),
    ))
}

/// `GET /internal/v1/organizations/{id}/policies/effective` — the organization-level
/// constraints for owner services (Meeting, Calling, Messaging, Hub), with the lifecycle
/// and provisioning state they must also honour and the legacy Meeting profile policy.
pub async fn effective(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let org = read_org(&state, &caller, id).await?;
    let row = store::policy_row(&state.db, id, false).await?;
    let doc = store::parse_policy(row.as_ref().map(|r| &r.document))?;
    Ok(respond(
        StatusCode::OK,
        json!({
            "organizationId": org.id,
            "lifecycleStatus": org.lifecycle_status,
            "provisioningState": org.provisioning_state,
            "revision": row.as_ref().map(|r| r.revision).unwrap_or(0),
            "constraints": doc.constraints_json(),
            "legacyMeetingProfilePolicy": org.profile_policy,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateBody {
    pub expected_revision: i64,
    pub policy: Value,
}

/// `PATCH /internal/v1/organizations/{id}/policies` `{expectedRevision, policy}` — a
/// partial document merged into the current one, validated strictly and stored with
/// `revision + 1` (compare-and-swap: a stale `expectedRevision` is 409 VERSION_CONFLICT).
/// Requires `organization.policies.manage`; `delegatedAdministration` is OWNER-only.
pub async fn update(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (UpdateBody, Value) = parse(&body)?;
    if !req.policy.is_object() {
        return Err(ApiError::validation("policy must be an object"));
    }
    // ADR 0064: the `apps` section exists only while app installs are enabled (off: refused
    // exactly like any unknown section).
    if !state.app_installs_enabled && req.policy.get("apps").is_some() {
        return Err(ApiError::validation(
            "the policy has a missing, unknown or mistyped field",
        ));
    }
    let txn = state.db.begin().await?;
    let access = authorize(&txn, id, user, Capability::PoliciesManage, true).await?;
    let row = store::policy_row(&txn, id, true).await?;
    let revision = row.as_ref().map(|r| r.revision).unwrap_or(0);
    if revision != req.expected_revision {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    let current = store::parse_policy(row.as_ref().map(|r| &r.document))?;
    let before = current.to_json();
    let mut merged = before.clone();
    merge(&mut merged, &req.policy);
    let next = PolicyDocument::parse(&merged).map_err(ApiError::validation)?;
    let after = next.to_json();
    let changed = changed_keys(&before, &after);
    if changed.is_empty() {
        return Ok(respond(
            StatusCode::OK,
            policy_json(&access.org, row.as_ref(), &current),
        ));
    }
    let owner_only = changed.iter().any(|k| {
        OWNER_ONLY_SECTIONS
            .iter()
            .any(|s| k.starts_with(&format!("{s}.")))
    });
    if owner_only && access.membership.role() != Role::Owner {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let stored = store_policy(&txn, &ctx, now, id, revision, &after, &changed, Some(user)).await?;
    let doc = policy_json(&access.org, Some(&stored), &next);
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

/// Stores the merged document at `revision + 1` (compare-and-swap on `revision`) with its
/// `policy.updated` event and audit.
#[allow(clippy::too_many_arguments)]
async fn store_policy(
    txn: &sea_orm::DatabaseTransaction,
    ctx: &OpContext,
    now: chrono::DateTime<chrono::Utc>,
    id: Uuid,
    revision: i64,
    after: &Value,
    changed: &[String],
    updated_by: Option<Uuid>,
) -> Result<PolicyRow, ApiError> {
    let stored: PolicyRow = db::one(
        txn,
        "INSERT INTO organization_policies (organization_id, revision, document, updated_by, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) \
         ON CONFLICT (organization_id) DO UPDATE SET revision = EXCLUDED.revision, document = EXCLUDED.document, updated_by = EXCLUDED.updated_by, updated_at = EXCLUDED.updated_at \
         WHERE organization_policies.revision = $6 \
         RETURNING revision, document, updated_by, updated_at",
        vec![
            id.into(),
            (revision + 1).into(),
            after.clone().into(),
            updated_by.into(),
            now.into(),
            revision.into(),
        ],
    )
    .await?
    .ok_or(ApiError::new(ErrorCode::VersionConflict))?;
    store::emit(
        txn,
        ctx,
        now,
        Event {
            organization_id: id,
            aggregate_type: "policy",
            aggregate_id: id,
            aggregate_version: stored.revision,
            event_type: "policy.updated",
            payload: json!({ "policyRevision": stored.revision, "changedKeys": changed }),
        },
    )
    .await?;
    store::audit(
        txn,
        ctx,
        now,
        Some(id),
        "policy.updated",
        "policy",
        Some(id),
        json!({ "fromRevision": revision, "toRevision": stored.revision, "changedKeys": changed }),
    )
    .await?;
    Ok(stored)
}

/// The admin bridge's answer for a policy change: `UPDATED` / `UNCHANGED` and the revision.
fn policy_outcome(
    state: &str,
    organization: Uuid,
    row: Option<&PolicyRow>,
    changed: &[String],
) -> Outcome {
    let revision = row.map(|r| r.revision).unwrap_or(0);
    Outcome::new(
        state,
        Some(revision),
        json!({
            "organizationId": organization,
            "revision": revision,
            "changedKeys": changed,
            "updatedAt": row.map(|r| store::rfc3339(r.updated_at)),
        }),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminUpdateBody {
    /// The current policy revision (0 = never stored: the defaults).
    pub expected_version: Option<i64>,
    pub policy: Value,
}

/// `POST /internal/v1/admin/organizations/{id}/policies/change` `{expectedVersion, policy}`
/// — the admin bridge's staff change of the typed policy (R3, two people; bridge only): the
/// same partial merge and strict validation as an owner's change, compare-and-swap on the
/// revision (`expectedVersion`; stale: 409 `VERSION_CONFLICT`). Staff may change every
/// section, `delegatedAdministration` included (`updatedBy` is `null`: no CF Meet user).
/// An archived organization is refused.
pub async fn admin_change(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let b = BridgeCommand::required(&caller, &headers, Risk::R3)?;
    let (req, _): (AdminUpdateBody, Value) = parse(&body)?;
    let expected = admin_bridge::expected_version(req.expected_version, 0)?;
    if !req.policy.is_object() {
        return Err(ApiError::validation("policy must be an object"));
    }
    if !state.app_installs_enabled && req.policy.get("apps").is_some() {
        return Err(ApiError::validation(
            "the policy has a missing, unknown or mistyped field",
        ));
    }
    let txn = state.db.begin().await?;
    if let Some(r) = admin_bridge::replayed(
        &txn,
        &b,
        command::POLICIES_CHANGE,
        Subject::Policy,
        Some(id),
    )
    .await?
    {
        store::org(&txn, id, false)
            .await?
            .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
        let row = store::policy_row(&txn, id, false).await?;
        txn.commit().await?;
        return Ok(policy_outcome(&r.state, id, row.as_ref(), &[]).response());
    }
    let org = store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if org.status() == LifecycleStatus::Archived {
        return Err(ApiError::new(ErrorCode::OrganizationArchived));
    }
    let row = store::policy_row(&txn, id, true).await?;
    let revision = row.as_ref().map(|r| r.revision).unwrap_or(0);
    admin_bridge::check_version(revision, expected)?;
    let current = store::parse_policy(row.as_ref().map(|r| &r.document))?;
    let before = current.to_json();
    let mut merged = before.clone();
    merge(&mut merged, &req.policy);
    let next = PolicyDocument::parse(&merged).map_err(ApiError::validation)?;
    let after = next.to_json();
    let changed = changed_keys(&before, &after);
    let now = db::now(&txn).await?;
    let outcome = if changed.is_empty() {
        policy_outcome("UNCHANGED", id, row.as_ref(), &changed)
    } else {
        let stored = store_policy(
            &txn,
            &caller.as_operator(),
            now,
            id,
            revision,
            &after,
            &changed,
            None,
        )
        .await?;
        policy_outcome("UPDATED", id, Some(&stored), &changed)
    };
    admin_bridge::record(
        &txn,
        &b,
        command::POLICIES_CHANGE,
        Subject::Policy,
        id,
        Some(id),
        &outcome,
        now,
    )
    .await?;
    txn.commit().await?;
    Ok(outcome.response())
}

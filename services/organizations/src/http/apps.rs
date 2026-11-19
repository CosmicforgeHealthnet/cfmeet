//! ADR 0064 workspace app installs (`ORGANIZATIONS_APP_INSTALLS_ENABLED`): the install
//! policy decision the Meeting Service (the account authority, permission
//! `organization-apps.evaluate`) asks before it lets an account install an app for a whole
//! organization.
//!
//! The decision is non-enumerating: an unknown organization, no membership, a removed or a
//! suspended membership are all `NOT_A_MEMBER`. Then the organization must be ACTIVE and
//! technically provisioned (`ORGANIZATION_NOT_READY`: an organization install lands in the
//! organization's FIRST_PARTY scope), the member's role must reach `apps.installTier`
//! (`ROLE_REQUIRED`; default ADMINS) and the app must be on `apps.allowList` when one is set
//! (`NOT_ALLOWLISTED`).

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{middleware::Caller, parse, respond};
use crate::{
    domain::{LifecycleStatus, MembershipStatus},
    error::{ApiError, ApiResult, ErrorCode},
    policy::AppInstallRefusal,
    state::AppState,
    store,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallCheckBody {
    pub account_id: Uuid,
    pub app_id: Uuid,
}

/// `POST /internal/v1/organizations/{id}/app-install-check` `{accountId, appId}` →
/// `{allowed, reason?}`.
pub async fn install_check(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    if !state.app_installs_enabled {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    caller.require(InternalPermission::OrganizationAppsEvaluate)?;
    let (req, _): (InstallCheckBody, Value) = parse(&body)?;
    // A relayed acting user may only ask about themselves.
    if caller.acting_user.is_some_and(|u| u != req.account_id) {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let decision = decide(&state, id, req.account_id, req.app_id).await?;
    tracing::info!(organization_id = %id, app_id = %req.app_id,
        allowed = decision.is_ok(), reason = decision.err().map(|r| r.as_str()).unwrap_or(""),
        "organization app install check");
    Ok(respond(
        StatusCode::OK,
        match decision {
            Ok(()) => json!({ "allowed": true }),
            Err(reason) => json!({ "allowed": false, "reason": reason.as_str() }),
        },
    ))
}

async fn decide(
    state: &AppState,
    organization: Uuid,
    account: Uuid,
    app: Uuid,
) -> Result<Result<(), AppInstallRefusal>, ApiError> {
    let Some(org) = store::org(&state.db, organization, false).await? else {
        return Ok(Err(AppInstallRefusal::NotAMember));
    };
    let Some(membership) =
        store::current_membership(&state.db, organization, account, false).await?
    else {
        return Ok(Err(AppInstallRefusal::NotAMember));
    };
    if membership.status() != MembershipStatus::Active {
        return Ok(Err(AppInstallRefusal::NotAMember));
    }
    if org.status() != LifecycleStatus::Active || org.provisioning_state != "READY" {
        return Ok(Err(AppInstallRefusal::OrganizationNotReady));
    }
    let policy = store::policy(&state.db, organization).await?;
    Ok(policy.apps().install_decision(membership.role(), app))
}

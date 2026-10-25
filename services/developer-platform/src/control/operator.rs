//! Operator administration of the control plane (`/internal/v1/operator/**`).
//!
//! Reachable only with `developer-platform.administer` (registry-grantable to the
//! operator console alone) AND a named human operator (`x-cf-operator`). Every command
//! writes an OPERATOR activity event. This is where the explicit, audited decisions live
//! that customers can never make themselves: restricting / suspending an organization,
//! publishing plan versions, assigning plans, internal cost allocation, and registering a
//! FIRST_PARTY organization (the tenant's first-party classification is Platform Access's,
//! set by its own operator tooling; it is verified here, never inferred from a name).

use chrono::Utc;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    delegate_with, display_name, email, members, organizations, stmt,
};
use crate::{
    domain::{rbac::Role, RequestContext},
    http::error::{ApiError, ErrorCode},
    platform_access::DelegationCall,
    state::AppState,
};

/// `^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$` (an opaque operator handle).
pub fn valid_operator(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 100
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._@-".contains(c))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterFirstParty {
    pub tenant_id: Uuid,
    pub display_name: String,
    pub owner_email: String,
}

/// Registers the organization of an existing FIRST_PARTY Platform Access tenant: state
/// INVITED with an OWNER invitation (token returned once); the owner's acceptance
/// activates it.
pub async fn register_first_party(
    state: &AppState,
    operator: &str,
    request: &RequestContext,
    body: RegisterFirstParty,
) -> Result<Value, ApiError> {
    let name = display_name(&body.display_name)?;
    let owner = email(&body.owner_email)?;
    let tenant = delegate_with(
        state,
        &request.correlation_id,
        DelegationCall::GetTenant {
            tenant_id: body.tenant_id,
        },
    )
    .await?;
    if tenant.get("classification").and_then(Value::as_str) != Some("FIRST_PARTY") {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let inserted = txn
        .execute(stmt(
            "INSERT INTO devplat_organizations \
                 (tenant_id, display_name, state, state_reason, created_by, created_at, updated_at) \
             VALUES ($1, $2, 'INVITED', 'AWAITING_OWNER', $3, $4, $4) ON CONFLICT DO NOTHING",
            vec![
                body.tenant_id.into(),
                name.clone().into(),
                format!("operator:{operator}").into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected();
    if inserted != 1 {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let (invitation, token) = members::insert_invitation(
        &txn,
        body.tenant_id,
        &owner,
        Role::Owner,
        &format!("operator:{operator}"),
    )
    .await?;
    activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        Event {
            tenant_id: body.tenant_id,
            action: "organization.first_party_registered",
            target_type: "ORGANIZATION",
            target_id: body.tenant_id.to_string(),
            summary: json!({ "displayName": name, "classification": "FIRST_PARTY",
                             "ownerInvitationId": invitation["id"] }),
        },
    )
    .await?;
    let org = organizations::view(&txn, body.tenant_id, None).await?;
    txn.commit().await?;
    let mut invitation = invitation;
    invitation["token"] = Value::from(token);
    Ok(json!({ "organization": org, "ownerInvitation": invitation }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetState {
    pub state: String,
    pub reason: String,
}

pub fn valid_reason(reason: &str) -> bool {
    let b = reason.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// A read model of a tenant for operators (organization + members count + project count).
pub async fn organization_overview<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Value, ApiError> {
    let mut org = organizations::view(db, tenant_id, None).await?;
    let counts = db
        .query_one(stmt(
            "SELECT (SELECT count(*) FROM devplat_workspace_members WHERE tenant_id = $1 AND status = 'ACTIVE') AS members, \
                    (SELECT count(*) FROM devplat_project_profiles WHERE tenant_id = $1) AS projects",
            vec![tenant_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    org["activeMembers"] = Value::from(counts.try_get::<i64>("", "members")?);
    org["projects"] = Value::from(counts.try_get::<i64>("", "projects")?);
    Ok(org)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_handles_and_reasons_are_bounded_codes() {
        assert!(valid_operator("ops.alice@cf"));
        assert!(!valid_operator(""));
        assert!(!valid_operator("has space"));
        assert!(valid_reason("FRAUD_REVIEW"));
        assert!(!valid_reason("fraud review"));
        assert!(!valid_reason(&"A".repeat(65)));
    }
}

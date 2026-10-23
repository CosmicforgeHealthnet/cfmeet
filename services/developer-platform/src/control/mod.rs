//! Section 50 — the developer / enterprise control plane.
//!
//! * [`Caller`]: who acts — a HUMAN developer whose CF Meet session the Meeting Service
//!   verified, or a MACHINE credential Platform Access authenticated (through the API
//!   Gateway) — and the request context.
//! * [`authorize`]: the single authorization decision of every operation: organization
//!   existence and tenant isolation (non-members get NOT_FOUND, never a hint), the
//!   Section 50 role matrix for humans, Platform Access scopes for machines, and the
//!   organization lifecycle state.
//! * [`activity`]: immutable tenant-scoped activity history, written in the mutation's
//!   own transaction (and for denied attempts).
//! * [`idempotency`]: `Idempotency-Key` replay of control-plane mutations.
//!
//! Platform Access stays the identity authority: organizations / projects / environments
//! are Developer Platform metadata keyed to canonical Platform Access ids, and every
//! identity mutation is DELEGATED to Platform Access ([`crate::platform_access`]).

pub mod activity;
pub mod commercial;
pub mod entitlements;
pub mod idempotency;
pub mod members;
pub mod operator;
pub mod organizations;
pub mod pagination;
pub mod projects;

use cf_service_auth::DelegatedPlatformContext;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    domain::{
        rbac::{allows, Action, OrganizationState, Role},
        RequestContext,
    },
    http::error::{ApiError, ErrorCode},
    platform_access::{CanonicalContext, Delegated, DelegationActor, DelegationCall},
    session::HumanUser,
    state::AppState,
};

/// A machine credential the API Gateway authenticated through Platform Access, its
/// delegated context re-validated by Platform Access.
#[derive(Debug, Clone)]
pub struct MachineCaller {
    pub context: CanonicalContext,
    pub delegated: DelegatedPlatformContext,
    pub credential_id: Uuid,
}

#[derive(Debug, Clone)]
pub enum Principal {
    Human(HumanUser),
    Machine(MachineCaller),
}

#[derive(Debug, Clone)]
pub struct Caller {
    pub principal: Principal,
    pub request: RequestContext,
}

impl Caller {
    /// `user:<id>` / `service_account:<id>` (opaque; never an email or a credential).
    pub fn actor_ref(&self) -> String {
        match &self.principal {
            Principal::Human(h) => format!("user:{}", h.user_id),
            Principal::Machine(m) => format!("service_account:{}", m.context.service_account_id),
        }
    }

    pub fn actor_kind(&self) -> &'static str {
        match &self.principal {
            Principal::Human(_) => "USER",
            Principal::Machine(_) => "SERVICE",
        }
    }

    pub fn delegation_actor(&self) -> DelegationActor {
        match &self.principal {
            Principal::Human(h) => DelegationActor::Human(h.user_id),
            Principal::Machine(m) => DelegationActor::ServiceAccount(m.context.service_account_id),
        }
    }

    pub fn human(&self) -> Result<&HumanUser, ApiError> {
        match &self.principal {
            Principal::Human(h) => Ok(h),
            Principal::Machine(_) => Err(ApiError(ErrorCode::PermissionDenied)),
        }
    }

    pub fn user_id(&self) -> Option<Uuid> {
        match &self.principal {
            Principal::Human(h) => Some(h.user_id),
            Principal::Machine(_) => None,
        }
    }
}

/// The scope a machine credential needs for an action (`None`: human console only).
pub fn machine_scope(action: Action) -> Option<&'static str> {
    use Action::*;
    match action {
        OrganizationRead | MembersRead => Some("organizations.read"),
        OrganizationUpdate => Some("organizations.manage"),
        ProjectsRead => Some("projects.read"),
        ProjectsManage | EnvironmentsManage => Some("projects.manage"),
        ActivityRead => Some("audit.read"),
        WebhooksRead => Some("webhooks.read"),
        WebhooksManage => Some("webhooks.manage"),
        UsageRead | LimitsRead => Some("usage.read"),
        // Membership, ownership, closure, billing, credentials and production access are
        // role-gated human operations (or Section 54 / 55).
        OrganizationClose
        | MembersManage
        | OwnershipTransfer
        | UsageExport
        | CommercialRead
        | BillingAccountManage
        | ProductionAccessRequest
        | CredentialsManage => None,
    }
}

/// The outcome of a successful authorization.
#[derive(Debug, Clone)]
pub struct Access {
    pub tenant_id: Uuid,
    pub state: OrganizationState,
    /// `(member id, role)` of a human caller.
    pub member: Option<(Uuid, Role)>,
}

pub(crate) fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// `(state)` of an organization, if it exists.
pub async fn organization_state<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Option<OrganizationState>, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT state FROM devplat_organizations WHERE tenant_id = $1",
            vec![tenant_id.into()],
        ))
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<String>("", "state").ok())
        .and_then(|s| OrganizationState::parse(&s)))
}

/// The ACTIVE membership of a human in an organization.
pub async fn membership<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<Option<(Uuid, Role)>, ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT id, role FROM devplat_workspace_members \
             WHERE tenant_id = $1 AND user_id = $2 AND status = 'ACTIVE'",
            vec![tenant_id.into(), user_id.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => {
            let id: Uuid = r.try_get("", "id")?;
            let role: String = r.try_get("", "role")?;
            Role::parse(&role).map(|role| (id, role))
        }
        None => None,
    })
}

fn state_error(state: OrganizationState) -> ApiError {
    ApiError(match state {
        OrganizationState::Restricted => ErrorCode::OrganizationRestricted,
        OrganizationState::Suspended => ErrorCode::OrganizationSuspended,
        OrganizationState::Closed => ErrorCode::OrganizationClosed,
        OrganizationState::Invited => ErrorCode::OrganizationNotActive,
        OrganizationState::Active => ErrorCode::InternalError,
    })
}

/// The single authorization decision of a control-plane operation on `tenant_id`.
///
/// Non-members (and machine credentials of another tenant) get NOT_FOUND: the existence
/// of another tenant's organization is never confirmed. Members denied by role, scope or
/// lifecycle get a stable 403 and a DENIED activity event.
pub async fn authorize(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    action: Action,
) -> Result<Access, ApiError> {
    let Some(org_state) = organization_state(&state.db, tenant_id).await? else {
        return Err(ApiError(ErrorCode::NotFound));
    };
    let (member, denied) = match &caller.principal {
        Principal::Human(h) => {
            let Some((member_id, role)) = membership(&state.db, tenant_id, h.user_id).await? else {
                return Err(ApiError(ErrorCode::NotFound));
            };
            let denied = (!allows(role, action)).then_some(ErrorCode::PermissionDenied);
            (Some((member_id, role)), denied)
        }
        Principal::Machine(m) => {
            if m.context.tenant_id != tenant_id {
                return Err(ApiError(ErrorCode::NotFound));
            }
            let denied = match machine_scope(action) {
                None => Some(ErrorCode::PermissionDenied),
                Some(scope) => {
                    let allowed = state
                        .platform_access
                        .credential_allows(
                            &m.delegated,
                            &caller.request.correlation_id,
                            m.credential_id,
                            &[scope],
                        )
                        .await
                        .map_err(|e| {
                            tracing::warn!(error = %e, "identity authority unavailable");
                            ApiError(ErrorCode::ServiceUnavailable)
                        })?;
                    (!allowed).then_some(ErrorCode::InsufficientScope)
                }
            };
            (None, denied)
        }
    };
    let denied = denied.or_else(|| (!org_state.permits(action)).then(|| state_error(org_state).0));
    if let Some(code) = denied {
        if action.is_mutation() {
            activity::record_denied(&state.db, caller, tenant_id, action, code).await;
        }
        return Err(ApiError(code));
    }
    Ok(Access {
        tenant_id,
        state: org_state,
        member,
    })
}

/// Runs a delegated Platform Access operation; refusals map to stable errors.
pub async fn delegate(
    state: &AppState,
    caller: &Caller,
    call: DelegationCall,
) -> Result<Value, ApiError> {
    delegate_with(state, &caller.request.correlation_id, call).await
}

/// [`delegate`] continuing an explicit correlation id (operator commands).
pub async fn delegate_with(
    state: &AppState,
    correlation: &cf_service_auth::CorrelationId,
    call: DelegationCall,
) -> Result<Value, ApiError> {
    match state.platform_access.delegate(call, correlation).await {
        Ok(Delegated::Ok(v)) => Ok(v),
        Ok(Delegated::Refused { status, code }) => {
            tracing::info!(status, %code, "platform access refused a delegated operation");
            Err(ApiError(match (status, code.as_str()) {
                (404, _) => ErrorCode::NotFound,
                (_, "POLICY_VIOLATION") => ErrorCode::PolicyViolation,
                (409, _) | (_, "INVALID_STATE") | (_, "CONFLICT") => ErrorCode::InvalidState,
                (400, "VALIDATION_ERROR") => ErrorCode::ValidationError,
                _ => ErrorCode::ServiceUnavailable,
            }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "identity authority unavailable");
            Err(ApiError(ErrorCode::ServiceUnavailable))
        }
    }
}

/// A Platform Access id from a delegated response.
pub fn id_of(v: &Value) -> Result<Uuid, ApiError> {
    v.get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))
}

/// A fresh opaque Platform Access slug (`<prefix>-<20 hex>`): slugs are never a
/// customer-chosen identity.
pub fn opaque_slug(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().simple().to_string()[..20])
}

/// Trimmed display name (1–120 visible characters).
pub fn display_name(value: &str) -> Result<String, ApiError> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > 120 || v.chars().any(char::is_control) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok(v.to_string())
}

/// Lower-cased email address (bounded, one `@`).
pub fn email(value: &str) -> Result<String, ApiError> {
    let v = value.trim().to_ascii_lowercase();
    let ok = v.len() <= 254
        && v.matches('@').count() == 1
        && !v.starts_with('@')
        && !v.ends_with('@')
        && !v.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        Ok(v)
    } else {
        Err(ApiError(ErrorCode::ValidationError))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machines_never_get_human_only_authority() {
        for a in [
            Action::MembersManage,
            Action::OwnershipTransfer,
            Action::OrganizationClose,
            Action::BillingAccountManage,
            Action::CommercialRead,
        ] {
            assert!(machine_scope(a).is_none(), "{a:?}");
        }
        assert_eq!(
            machine_scope(Action::ProjectsManage),
            Some("projects.manage")
        );
        assert_eq!(machine_scope(Action::ActivityRead), Some("audit.read"));
    }

    #[test]
    fn inputs_are_bounded() {
        assert!(display_name("  Acme  ").is_ok());
        assert!(display_name("").is_err());
        assert!(display_name(&"x".repeat(121)).is_err());
        assert!(display_name("a\u{0007}b").is_err());
        assert_eq!(email(" Dev@Example.COM ").unwrap(), "dev@example.com");
        assert!(email("no-at").is_err());
        assert!(email("a@b@c").is_err());
        assert!(opaque_slug("org").starts_with("org-"));
    }
}

//! The Organizations Service as the workspace install policy (ADR 0064, integrate
//! contract §4) and the organization role authority for the Apps screens:
//!
//! * `POST /internal/v1/organizations/{id}/app-install-check` (`organization-apps.evaluate`)
//!   before an ORGANIZATION install is relayed to Platform Access;
//! * organization administrators (OWNER / ADMIN of an ACTIVE organization, ACTIVE
//!   membership) from `memberships/resolve`, for the organization Apps list / revoke.
//!
//! Both need the Organizations Service to be authoritative; otherwise organization
//! installs and organization Apps are unavailable (fail closed).

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::Method;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::services::{
    organizations_authority::{AuthorityError, Call, SharedOrganizationsAuthority},
    platform::authority::AuthorityUnavailable,
};

/// The organization's decision on an app install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallDecision {
    Allowed,
    /// `NOT_A_MEMBER`, `ROLE_REQUIRED`, `NOT_ALLOWLISTED` or `ORGANIZATION_NOT_READY`.
    Refused(String),
}

#[async_trait]
pub trait AppOrganizations: Send + Sync {
    async fn install_check(
        &self,
        organization: Uuid,
        account: Uuid,
        app: Uuid,
    ) -> Result<InstallDecision, AuthorityUnavailable>;

    /// OWNER or ADMIN (ACTIVE membership) of the ACTIVE organization.
    async fn is_admin(
        &self,
        organization: Uuid,
        account: Uuid,
    ) -> Result<bool, AuthorityUnavailable>;
}

pub type SharedAppOrganizations = Arc<dyn AppOrganizations>;

pub const INSTALL_CHECK_REASONS: &[&str] = &[
    "NOT_A_MEMBER",
    "ROLE_REQUIRED",
    "NOT_ALLOWLISTED",
    "ORGANIZATION_NOT_READY",
];

/// Maps the install-check answer (contract §4).
pub fn decision_of(status: u16, body: &Value) -> Result<InstallDecision, AuthorityUnavailable> {
    match status {
        200 => match body.get("allowed").and_then(Value::as_bool) {
            Some(true) => Ok(InstallDecision::Allowed),
            Some(false) => Ok(InstallDecision::Refused(
                body.get("reason")
                    .and_then(Value::as_str)
                    .filter(|r| INSTALL_CHECK_REASONS.contains(r))
                    .unwrap_or("ROLE_REQUIRED")
                    .to_string(),
            )),
            None => Err(AuthorityUnavailable("malformed install decision".into())),
        },
        // App installs off at Organizations (404) or any other refusal: never an allow.
        400..=499 if status != 429 => Ok(InstallDecision::Refused("ORGANIZATION_NOT_READY".into())),
        other => Err(AuthorityUnavailable(format!("HTTP {other}"))),
    }
}

/// Over the signed Organizations client of the organization relay.
pub struct OrganizationsAppPolicy {
    authority: SharedOrganizationsAuthority,
}

impl OrganizationsAppPolicy {
    pub fn new(authority: SharedOrganizationsAuthority) -> Self {
        Self { authority }
    }
}

#[async_trait]
impl AppOrganizations for OrganizationsAppPolicy {
    async fn install_check(
        &self,
        organization: Uuid,
        account: Uuid,
        app: Uuid,
    ) -> Result<InstallDecision, AuthorityUnavailable> {
        let call = Call::new(
            Method::POST,
            format!("/internal/v1/organizations/{organization}/app-install-check"),
        )
        .body(json!({ "accountId": account, "appId": app }));
        let reply = self
            .authority
            .send(&call)
            .await
            .map_err(|e| AuthorityUnavailable(e.0.to_string()))?;
        decision_of(reply.status, &reply.body)
    }

    async fn is_admin(
        &self,
        organization: Uuid,
        account: Uuid,
    ) -> Result<bool, AuthorityUnavailable> {
        match self.authority.resolve_memberships(account).await {
            Ok(memberships) => Ok(memberships.iter().any(|m| {
                m.organization_id == organization
                    && m.status == "ACTIVE"
                    && m.organization_status == "ACTIVE"
                    && matches!(m.role.as_str(), "OWNER" | "ADMIN")
            })),
            Err(AuthorityError::Refused(_)) => Ok(false),
            Err(AuthorityError::Unavailable(u)) => Err(AuthorityUnavailable(u.0.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_decisions_never_fail_open() {
        assert_eq!(
            decision_of(200, &json!({ "allowed": true })).unwrap(),
            InstallDecision::Allowed
        );
        assert_eq!(
            decision_of(
                200,
                &json!({ "allowed": false, "reason": "NOT_ALLOWLISTED" })
            )
            .unwrap(),
            InstallDecision::Refused("NOT_ALLOWLISTED".into())
        );
        assert_eq!(
            decision_of(200, &json!({ "allowed": false, "reason": "<script>" })).unwrap(),
            InstallDecision::Refused("ROLE_REQUIRED".into())
        );
        assert_eq!(
            decision_of(404, &json!({})).unwrap(),
            InstallDecision::Refused("ORGANIZATION_NOT_READY".into())
        );
        assert!(decision_of(200, &json!({})).is_err());
        assert!(decision_of(503, &json!({})).is_err());
    }
}

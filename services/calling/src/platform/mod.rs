//! Platform consumer, credential and principal trust foundation (Part B §7–§10).
//!
//! ```text
//! Tenant ─┬─ Application ─┬─ allowed scopes
//!         │               └─ Environment (SANDBOX | TEST | PRODUCTION)
//!         │                    ├─ Service Account ── API Credential ── granted scopes
//!         │                    └─ Principal (communication participant / entity)
//! ```
//!
//! * [`PlatformService`] owns the administrative operations (create / suspend /
//!   reactivate, scope grants, credential issuance / rotation / revocation, principal
//!   registration, limit policy). The operational CLI (`platform_admin`) and future
//!   administration APIs use the same service.
//! * [`authentication::CredentialAuthenticator`] turns a presented credential into a
//!   [`context::PlatformAuthContext`] (API client identity).
//! * [`principals::PrincipalResolver`] resolves a communication principal inside the
//!   authenticated context's tenant / application / environment (participant identity).
//!   The two identities are separate types and are never derived from each other.
//!
//! First-party and external consumers use exactly the same model and code path; the only
//! difference is explicit configuration (tenant classification, granted scopes).

pub mod applications;
pub mod authentication;
pub mod context;
pub mod credential_format;
pub mod credentials;
pub mod environments;
pub mod error;
pub mod limits;
pub mod principals;
pub mod scopes;
pub mod service_accounts;
pub mod tenants;
pub mod verifier;

use std::sync::Arc;

use sea_orm::DatabaseConnection;

pub use error::{PlatformError, PlatformResult};

use crate::{
    config::CredentialPolicy,
    domain::SharedClock,
    platform::{limits::LimitDefaults, scopes::ScopeRegistry, verifier::VerifierKeyring},
};

/// Administrative domain service for the platform trust foundation.
#[derive(Clone)]
pub struct PlatformService {
    pub(crate) db: DatabaseConnection,
    pub(crate) clock: SharedClock,
    pub(crate) keyring: Arc<VerifierKeyring>,
    pub(crate) registry: Arc<ScopeRegistry>,
    pub(crate) policy: CredentialPolicy,
    pub(crate) limit_defaults: LimitDefaults,
}

impl PlatformService {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        keyring: Arc<VerifierKeyring>,
        registry: Arc<ScopeRegistry>,
        policy: CredentialPolicy,
        limit_defaults: LimitDefaults,
    ) -> Self {
        Self {
            db,
            clock,
            keyring,
            registry,
            policy,
            limit_defaults,
        }
    }

    pub fn registry(&self) -> &ScopeRegistry {
        &self.registry
    }

    pub fn credential_policy(&self) -> &CredentialPolicy {
        &self.policy
    }
}

/// Validates a human display name: trimmed, 1–200 characters, no control characters.
pub(crate) fn display_name(value: &str) -> PlatformResult<String> {
    let trimmed = value.trim();
    let chars = trimmed.chars().count();
    if chars == 0 || chars > 200 || trimmed.chars().any(char::is_control) {
        return Err(PlatformError::Validation(
            "display name must be 1-200 characters without control characters".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Validates optional free text (descriptions, labels).
pub(crate) fn optional_text(
    value: Option<&str>,
    field: &'static str,
    max_chars: usize,
) -> PlatformResult<Option<String>> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) if v.chars().count() <= max_chars && !v.chars().any(char::is_control) => {
            Ok(Some(v.to_string()))
        }
        Some(_) => Err(PlatformError::Validation(format!(
            "{field} must be at most {max_chars} characters without control characters"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_are_trimmed_and_bounded() {
        assert_eq!(display_name("  Acme  ").unwrap(), "Acme");
        assert!(display_name("   ").is_err());
        assert!(display_name(&"x".repeat(201)).is_err());
        assert!(display_name("bad\nname").is_err());
    }

    #[test]
    fn optional_text_is_bounded() {
        assert_eq!(optional_text(None, "label", 10).unwrap(), None);
        assert_eq!(optional_text(Some("  "), "label", 10).unwrap(), None);
        assert_eq!(
            optional_text(Some(" ok "), "label", 10).unwrap(),
            Some("ok".to_string())
        );
        assert!(optional_text(Some(&"x".repeat(11)), "label", 10).is_err());
    }
}

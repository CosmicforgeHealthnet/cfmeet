//! Calling's side of the platform consumer foundation (Part B §7–§10).
//!
//! The identity hierarchy — tenants, applications, SANDBOX / TEST / PRODUCTION
//! environments, service accounts, API credentials and their scopes, communication
//! principals — is owned by Platform Access (`services/platform-access`, ADR 0032), the
//! only service that verifies a customer credential. Calling consumes it:
//!
//! * [`access`]: the authenticated client of Platform Access's internal contract;
//! * [`authentication::CredentialAuthenticator`]: presented credential → Platform Access
//!   → [`context::PlatformAuthContext`] (API client identity);
//! * [`projection`]: the read-only projection of the identity rows Calling's foreign keys
//!   reference (same ids), fed by authority chains and the identity change feed;
//! * [`principals::PrincipalResolver`]: communication principals resolved inside the
//!   authenticated context's tenant / application / environment (participant identity);
//! * [`handoff`]: the controlled Part III → Platform Access hand-off (export, receipt
//!   verification, verifier scrub, rollback restore).
//!
//! [`PlatformService`] keeps Calling-owned policy (limits) and read access to the
//! projection. API client identity and participant identity stay separate types.

pub mod access;
pub mod applications;
pub mod authentication;
pub mod context;
pub mod credentials;
pub mod environments;
pub mod error;
pub mod handoff;
pub mod limits;
pub mod operator;
pub mod principals;
pub mod projection;
pub mod scopes;
pub mod service_accounts;
pub mod tenants;

use sea_orm::DatabaseConnection;

pub use error::{PlatformError, PlatformResult};

use crate::{domain::SharedClock, platform::limits::LimitDefaults};

/// Calling-owned platform policy (limits) and read access to the identity projection.
#[derive(Clone)]
pub struct PlatformService {
    pub(crate) db: DatabaseConnection,
    pub(crate) clock: SharedClock,
    pub(crate) limit_defaults: LimitDefaults,
}

impl PlatformService {
    pub fn new(db: DatabaseConnection, clock: SharedClock, limit_defaults: LimitDefaults) -> Self {
        Self {
            db,
            clock,
            limit_defaults,
        }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }
}

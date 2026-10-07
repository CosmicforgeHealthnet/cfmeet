//! SeaORM entities for the Calling database. These are owned by this service only and
//! are never shared with (or imported from) any other deployable service.

pub mod api_credential_scopes;
pub mod api_credentials;
pub mod application_limits;
pub mod application_scopes;
pub mod applications;
pub mod credential_limits;
pub mod environments;
pub mod principals;
pub mod service_accounts;
pub mod tenants;

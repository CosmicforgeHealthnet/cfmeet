//! SeaORM entities for the Platform Access database. Owned by this service only and never
//! shared with (or imported from) any other deployable service.

pub mod api_credential_scopes;
pub mod api_credentials;
pub mod application_scopes;
pub mod applications;
pub mod environments;
pub mod principals;
pub mod service_accounts;
pub mod tenants;

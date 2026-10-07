//! SeaORM entities for the Calling database. These are owned by this service only and
//! are never shared with (or imported from) any other deployable service.

pub mod api_credential_scopes;
pub mod api_credentials;
pub mod application_limits;
pub mod application_scopes;
pub mod applications;
pub mod call_attempts;
pub mod call_authorizations;
pub mod call_events;
pub mod call_legs;
pub mod call_participants;
pub mod call_quality_records;
pub mod call_routes;
pub mod call_session_intervals;
pub mod call_sessions;
pub mod call_usage_records;
pub mod calling_outbox_events;
pub mod calls;
pub mod credential_limits;
pub mod environments;
pub mod principals;
pub mod service_accounts;
pub mod tenants;

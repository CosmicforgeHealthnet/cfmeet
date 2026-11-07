//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_service_auth::ServiceTokenVerifier;
use sea_orm::DatabaseConnection;

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    /// Receiver-authoritative verifier of `cfmeet.organizations`.
    pub verifier: Arc<ServiceTokenVerifier>,
}

impl AppState {
    pub fn new(db: DatabaseConnection, verifier: Arc<ServiceTokenVerifier>) -> Self {
        Self { db, verifier }
    }
}

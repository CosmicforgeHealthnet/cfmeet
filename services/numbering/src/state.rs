//! Shared application state.

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{service_auth::ServiceAuthRuntime, WorkerSettings},
    domain::SharedClock,
    identity::CustomerAuthority,
    numbers::NumberDomain,
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub clock: SharedClock,
    pub numbers: NumberDomain,
    /// Customer credential verification (Platform Access).
    pub authority: Arc<dyn CustomerAuthority>,
    /// Internal service authentication.
    pub service_auth: ServiceAuthRuntime,
    pub workers: WorkerSettings,
}

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        authority: Arc<dyn CustomerAuthority>,
        service_auth: ServiceAuthRuntime,
        workers: WorkerSettings,
    ) -> Self {
        Self {
            numbers: NumberDomain::new(db.clone(), clock.clone()),
            db,
            clock,
            authority,
            service_auth,
            workers,
        }
    }
}

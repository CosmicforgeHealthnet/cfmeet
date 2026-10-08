//! Route health / capacity input (§22). Calling learns normalized route availability
//! from Telephony's internal API (`GET /internal/v1/routes/availability`) through this
//! port — never by reading Telephony data. Without a configured Telephony, no public
//! route exists (an empty, KNOWN set), which denies paid classes and leaves APP_RTC alone.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::engine::{CandidateRoute, Evidence, RouteCapability};

/// What the router asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailabilityQuery {
    pub country: Option<String>,
    pub capabilities: Vec<RouteCapability>,
}

#[async_trait]
pub trait RouteAvailabilitySource: Send + Sync {
    /// Normalized candidates; `Unknown` when Telephony could not be asked.
    async fn availability(&self, query: &AvailabilityQuery) -> Evidence<Vec<CandidateRoute>>;
}

/// No Telephony configured: there is no public route.
pub struct NoPublicRoutes;

#[async_trait]
impl RouteAvailabilitySource for NoPublicRoutes {
    async fn availability(&self, _: &AvailabilityQuery) -> Evidence<Vec<CandidateRoute>> {
        Evidence::Known(Vec::new())
    }
}

/// Fixed answers (tests and operator-pinned deployments).
#[derive(Clone)]
pub struct StaticAvailability {
    routes: Arc<Mutex<Evidence<Vec<CandidateRoute>>>>,
    queries: Arc<Mutex<Vec<AvailabilityQuery>>>,
}

impl StaticAvailability {
    pub fn new(routes: Evidence<Vec<CandidateRoute>>) -> Self {
        Self {
            routes: Arc::new(Mutex::new(routes)),
            queries: Arc::default(),
        }
    }

    pub fn set(&self, routes: Evidence<Vec<CandidateRoute>>) {
        *self.routes.lock().expect("availability lock") = routes;
    }

    pub fn queries(&self) -> Vec<AvailabilityQuery> {
        self.queries.lock().expect("availability lock").clone()
    }
}

#[async_trait]
impl RouteAvailabilitySource for StaticAvailability {
    async fn availability(&self, query: &AvailabilityQuery) -> Evidence<Vec<CandidateRoute>> {
        self.queries
            .lock()
            .expect("availability lock")
            .push(query.clone());
        self.routes.lock().expect("availability lock").clone()
    }
}

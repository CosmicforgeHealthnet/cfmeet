pub mod api;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod error;
pub mod events;
pub mod invites;
pub mod model;
pub mod policy;
pub mod profiles;
pub mod relationships;
pub mod store;

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
#[derive(Clone)]
pub struct State {
    pub store: store::Store,
    pub config: Arc<config::Config>,
    pub metrics: Arc<Metrics>,
}
pub struct Metrics {
    pub started: Instant,
    pub requests: AtomicU64,
    pub denied: AtomicU64,
    pub events: AtomicU64,
}
impl Default for Metrics {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            requests: AtomicU64::new(0),
            denied: AtomicU64::new(0),
            events: AtomicU64::new(0),
        }
    }
}
impl Metrics {
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"schemaVersion":1,"uptimeSeconds":self.started.elapsed().as_secs(),"requests":self.requests.load(Ordering::Relaxed),"denied":self.denied.load(Ordering::Relaxed),"eventsDelivered":self.events.load(Ordering::Relaxed)})
    }
}

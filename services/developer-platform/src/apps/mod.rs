//! CF Meet Apps — the Developer Console part of ADR 0064 / 0065 ("Integrate").
//!
//! * [`registration`]: the app registered on a project (manifest versions, Platform
//!   Access client profile sync, client secret rotation, submission, grants, test
//!   accounts) — `DEVELOPER_PLATFORM_APPS_ENABLED`.
//! * [`review`]: the operator review workflow (DRAFT → SUBMITTED → IN_REVIEW → APPROVED |
//!   REJECTED → PUBLISHED, SUSPENDED), named reviewers, no self-review.
//! * [`oauth`]: the anonymous RFC 6749 endpoints `/v1/oauth/*`, relayed to Platform
//!   Access (the token authority) — `DEVELOPER_PLATFORM_OAUTH_ENABLED`.
//! * [`directory`]: the anonymous app directory — `DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED`.
//! * [`installations`]: the projection of Platform Access `APP_GRANT` changes and the app
//!   lifecycle events — `DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED` (the install routing
//!   pass itself lives in [`crate::webhooks::ingest`]).
//!
//! The Developer Platform verifies no client secret and no OAuth token: Platform Access
//! does, live, for every request.

pub mod directory;
pub mod installations;
pub mod manifest;
pub mod oauth;
pub mod port;
pub mod registration;
pub mod review;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::config::OAuthEndpoints;

/// Runtime settings of the apps features.
#[derive(Debug, Clone, Default)]
pub struct AppSettings {
    /// `DEVELOPER_PLATFORM_APP_REVIEWERS` (empty: no review decision is possible).
    pub reviewers: Vec<String>,
    /// What `/v1/oauth/metadata` advertises.
    pub endpoints: OAuthEndpoints,
    /// The per-process ceiling of the anonymous routes.
    pub limiter: Arc<AnonymousLimiter>,
}

/// A fixed-window request ceiling per key for the anonymous routes (defence in depth
/// behind the API Gateway's own limits): per client id for the token / revocation
/// endpoints, per route otherwise. Nothing secret is used as a key.
#[derive(Debug)]
pub struct AnonymousLimiter {
    per_minute: u32,
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Default for AnonymousLimiter {
    fn default() -> Self {
        Self::new(1_200)
    }
}

impl AnonymousLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one request of a client `key`; `false`: over the ceiling for this minute.
    pub fn allow(&self, key: &str) -> bool {
        self.allow_with(key, self.per_minute)
    }

    /// Counts one request of a whole anonymous route (50 times the client ceiling).
    pub fn allow_route(&self, route: &str) -> bool {
        self.allow_with(route, self.per_minute.saturating_mul(50))
    }

    fn allow_with(&self, key: &str, ceiling: u32) -> bool {
        let now = Instant::now();
        let Ok(mut w) = self.windows.lock() else {
            return true;
        };
        if w.len() > 10_000 {
            w.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        }
        let entry = w.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1 <= ceiling
    }
}

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

use crate::config::OAuthEndpoints;

/// Runtime settings of the apps features.
#[derive(Debug, Clone, Default)]
pub struct AppSettings {
    /// `DEVELOPER_PLATFORM_APP_REVIEWERS` (empty: no review decision is possible).
    pub reviewers: Vec<String>,
    /// What `/v1/oauth/metadata` advertises.
    pub endpoints: OAuthEndpoints,
}

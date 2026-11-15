//! Live push adapters (§68): Apple Push Notification service (token-based), Firebase Cloud
//! Messaging (HTTP v1) and Web Push (RFC 8030, message encryption RFC 8291, VAPID
//! RFC 8292).
//!
//! The adapters, their unit tests and their RFC 8291 vectors moved **verbatim** to the
//! shared crate `packages/push-transport` (Notification Service roadmap N4). Every item is
//! re-exported here under its previous path, so Messaging's behaviour, configuration and
//! tests are unchanged: metadata-only payloads, the Web Push SSRF host allowlist (at
//! registration and at send time, no redirects), constant-time signing and truthful
//! provider outcomes. From Phase 4.1 the unified Notification Service uses the crate and
//! this module is retired with the Messaging dispatcher.

pub use cf_push_transport::adapters::*;

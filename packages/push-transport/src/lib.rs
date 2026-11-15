//! CF Meet push transport (Notification Service roadmap N4, Document 6).
//!
//! The APNs, FCM and Web Push adapters, their provider boundary and their configuration,
//! moved **verbatim** out of Messaging together with their unit tests. Nothing about
//! their behaviour changed: payloads stay metadata-only ([`adapters::METADATA_KEYS`]),
//! Web Push endpoints stay restricted to the host allowlist (at registration and at send
//! time, no redirects), signing stays constant-time (`p256`, `ring`), and the RFC 8291
//! test vectors move with the code.
//!
//! Messaging depends on this crate and re-exports every item under its previous path
//! (`messaging::realtime::push_adapters::*`, `messaging::realtime::push::{PushOutcome,
//! PushProvider, LocalTestPush}`, `messaging::config::{PushAdapterConfig, ...}`) until
//! Phase 4.1; afterwards only the unified Notification Service uses it.

pub mod adapters;
pub mod config;
pub mod provider;

pub use config::{ApnsConfig, FcmConfig, PushAdapterConfig, WebPushConfig};
pub use provider::{LocalTestPush, PushOutcome, PushProvider};

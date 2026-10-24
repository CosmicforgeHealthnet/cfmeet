//! CF Meet Messaging — the Unified Messaging backend foundation (Part V Workstreams C–D).
//!
//! * [`identity`]: customers are authenticated by Platform Access (fresh for mutations,
//!   bounded feed-invalidated cache for reads); the acting principal is resolved by
//!   Platform Access inside the credential's own environment.
//! * [`devices`]: Messaging devices and their PUBLIC MLS material (registration with
//!   proof of possession, KeyPackages, revocation, directory).
//! * [`conversations`]: the Conversation aggregate — context classes, persisted security
//!   profiles, policy snapshots, lifecycle, sequencing foundation.
//! * [`transparency`]: the append-only RFC 9162 key-transparency log (unwitnessed).
//! * [`domain`]: security profiles and the encrypted / managed content boundary.
//! * [`events`]: transactional outbox.
//! * [`http`]: health / readiness, the served OpenAPI, the customer API.
//!
//! Messaging holds no private key, group secret or plaintext E2EE content, owns no other
//! service's data and never reads another service's database.

pub mod attachments;
pub mod audit;
pub mod backups;
pub mod compliance;
pub mod config;
pub mod conversations;
pub mod db;
pub mod devices;
pub mod domain;
pub mod engagement;
pub mod error;
pub mod events;
pub mod http;
pub mod identity;
pub mod integrations;
pub mod limits;
pub mod membership;
pub mod messages;
pub mod metrics;
pub mod mls;
pub mod moderation;
pub mod operations;
pub mod realtime;
pub mod safety;
pub mod search;
pub mod sms_bridge;
pub mod state;
pub mod transparency;
pub mod usage;
pub mod usage_export;
pub mod webhook_relay;
pub mod workers;

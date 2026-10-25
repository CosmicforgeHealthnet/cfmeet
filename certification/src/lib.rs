//! CF Meet Part VI cross-service certification harness.
//!
//! [`stack::Stack`] starts the REAL services as child processes — Platform Access, the
//! Developer Platform (+ worker), the Meeting Service, Calling, Messaging, Numbering, the
//! SMS Gateway, Privacy Relay and the API Gateway — each with:
//!
//! * its own PostgreSQL database, migrated by the service's own migration binary;
//! * its own freshly generated Ed25519 service signing key ([`keys`]), and a trust bundle
//!   derived AUTOMATICALLY from the internal permission registry of
//!   `packages/service-auth` (every permission granted by its single receiver to every
//!   started grantee; operator-only permissions to the harness's operator-console key);
//! * the shared Redis (gateway quota counters, Meeting sessions / OTPs);
//! * deterministic adapters only (synthetic numbering provider, deterministic SMS
//!   provider, no media provider, no carrier, no paid access).
//!
//! [`provision`] drives the services exactly as their operators and customers do: the
//! operator console (signed operator calls to Platform Access and the Developer Platform),
//! the CF Meet account API of the Meeting Service, and the public API Gateway.
//! [`receiver`] is a customer's webhook endpoint (scripted answers, recorded requests,
//! signature verification with the reference verifier of `packages/webhooks`).

pub mod db;
pub mod keys;
pub mod process;
pub mod provision;
pub mod receiver;
pub mod redis;
pub mod stack;
pub mod ws;

pub use stack::{Stack, StackConfig, StackOptions};

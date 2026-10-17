//! CF Meet SMS Gateway — the authoritative SMS TRANSPORT service (Part IV Workstreams M–O).
//!
//! * [`sms`]: transports (one message, inbound or outbound) and their state machine,
//!   content and number protection at rest, GSM-7 / UCS-2 segmentation, transport
//!   threads, safety controls and opt-out, events.
//! * [`providers`]: the provider boundary (submission, status query, authenticated
//!   callbacks), the deterministic test adapter and the reusable adapter contract.
//! * [`numbering`]: the port to Numbering — the gateway never replicates assignments,
//!   KYC or number lifecycle; it asks Numbering for every routing / sender decision.
//! * [`infra`]: transactional outbox and its publisher to the platform's webhook delivery.
//! * [`identity`]: customers are authenticated by Platform Access; services by
//!   `packages/service-auth`.
//! * [`http`]: health / readiness, the served OpenAPI, the customer API, provider
//!   callbacks.
//!
//! The SMS Gateway owns no call, no conversation model and no number authority, and never
//! reads another service's database.

pub mod config;
pub mod db;
pub mod domain;
pub mod http;
pub mod identity;
pub mod infra;
pub mod metrics;
pub mod numbering;
pub mod providers;
pub mod sms;
pub mod state;

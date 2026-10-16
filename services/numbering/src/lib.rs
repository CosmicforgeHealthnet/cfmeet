//! CF Meet Numbering — the canonical authority for public Number Resources (Part IV
//! Workstreams C–D).
//!
//! * [`numbers`]: the number domain — canonical resources and their lifecycle, provider
//!   bindings, capabilities, inventory observations, reservations, DCN assignments,
//!   ownership, routing profiles and history. Every state change goes through a domain
//!   transition (also enforced by the database).
//! * [`infra`]: reliability foundations — transactional outbox, leased jobs with bounded
//!   retry / dead-letter / uncertain-outcome parking, idempotency keys.
//! * [`identity`]: the trusted request context. Customers are authenticated by Platform
//!   Access (`platform-credential.authenticate`; owning principals are resolved with
//!   `platform-principal.resolve`); services by `packages/service-auth`.
//! * [`http`]: health / readiness, the customer read API and the internal API.
//!
//! Numbering owns no logical call, SIP / PSTN execution, relay-session semantics or SMS
//! transport envelope, and never reads another service's database.

pub mod acquisition;
pub mod config;
pub mod domain;
pub mod grants;
pub mod http;
pub mod identity;
pub mod infra;
pub mod metrics;
pub mod numbers;
pub mod providers;
pub mod state;

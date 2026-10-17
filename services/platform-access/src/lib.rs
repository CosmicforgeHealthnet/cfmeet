//! CF Meet **Platform Access** service (Part IV Workstream B).
//!
//! The single, independently owned authority for the shared platform identity hierarchy
//! — tenants, applications, SANDBOX / TEST / PRODUCTION environments, service accounts,
//! API credentials and their scopes, environment-scoped communication principals — and
//! for verifying customer API credentials. Calling (and later Numbering) consume it only
//! through authenticated internal contracts (`/internal/v1`, `packages/service-auth`) and
//! the identity change feed; no other service reads this database.
//!
//! Extracted from Calling's Part III trust foundation without changing identifiers,
//! verifier semantics (HMAC-SHA-256 under the same pepper and message label), scope
//! names, environment isolation, rotation / revocation behaviour or the first-party /
//! external classification (ADR 0032).

// `domain` first: its `text_enum!` / `typed_id!` macros are used by later modules.
#[macro_use]
pub mod domain;

pub mod config;
pub mod http;
pub mod identity;
pub mod metrics;
pub mod models;
pub mod state;

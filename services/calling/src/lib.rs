//! CF Meet **Calling** service.
//!
//! This crate currently contains the platform trust foundation the Calling domain is
//! built on (Part B §7–§10):
//!
//! * the platform consumer model — tenant, application, environment, service account;
//! * first-party and external consumers on one model;
//! * API credential security — keyed verifiers, scopes, rotation, revocation, limits;
//! * the separation between API client identity ([`platform::context::PlatformAuthContext`])
//!   and communication principal identity ([`platform::principals::ResolvedPrincipal`]).
//!
//! It also integrates internal service authentication (Part B §11) through the shared
//! `packages/service-auth` protocol ([`internal_auth`]).
//!
//! On that foundation [`calls`] implements the canonical Calling core (Part C §15–§19):
//! the provider-independent Call aggregate, its participants, sessions, routes, legs,
//! attempts, authorizations, immutable events, quality and usage records, the canonical
//! call state machine, the transactional outbox and the read-only call API.
//!
//! The service is independent of `services/meeting-service`: it shares no source, models
//! or database with it, and it never imports another deployable service.

// `domain` first: its `text_enum!` / `typed_id!` macros are used by later modules.
#[macro_use]
pub mod domain;

pub mod calls;
pub mod config;
pub mod endpoints;
pub mod error;
pub mod internal_auth;
pub mod media;
pub mod middleware;
pub mod models;
pub mod platform;
pub mod routes;
pub mod signaling;
pub mod state;
pub mod telemetry;

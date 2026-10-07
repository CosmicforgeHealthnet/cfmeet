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
//! The service is independent of `services/meeting-service`: it shares no source, models
//! or database with it.

pub mod config;
pub mod domain;
pub mod error;
pub mod middleware;
pub mod models;
pub mod platform;
pub mod routes;
pub mod state;
pub mod telemetry;

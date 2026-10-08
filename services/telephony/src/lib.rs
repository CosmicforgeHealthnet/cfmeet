//! CF Meet **Telephony** service (Part E §39–§51).
//!
//! Telephony owns public-network EXECUTION: Telephony legs (the execution of Calling's
//! canonical, provider-independent CallLegs), SIP dialogs, provider bindings, carrier /
//! provider events, durable telephony commands, bridges, provider health / capacity /
//! circuit breakers, unknown outcomes, reconciliation and provider-level usage / cost
//! evidence.
//!
//! It never decides whether a business user may call (Calling / Privacy Relay do; every
//! paid action carries their proof), never owns a logical Call, never receives a customer
//! credential and never touches another service's database. Raw telephone numbers exist
//! only in memory inside the execution boundary; provider identifiers never leave it.

pub mod abuse;
pub mod bridges;
pub mod clock;
pub mod commands;
pub mod config;
pub mod db;
pub mod domain;
pub mod edge_events;
pub mod error;
pub mod events;
pub mod inbound;
pub mod incidents;
pub mod internal;
pub mod legs;
pub mod metrics;
pub mod middleware;
pub mod ports;
pub mod providers;
pub mod reconciliation;
pub mod routes;
pub mod routing;
pub mod service;
pub mod sip;
pub mod state;
pub mod usage;
pub mod workers;

//! CF Meet **Privacy Relay** service (Part D §26–§30).
//!
//! The authoritative service for Communication Contexts and Privacy Relay Sessions: WHY
//! parties may communicate (an opaque consumer reference), WHO, in which DIRECTION, over
//! which CHANNEL, during which time window, under which relay strategy — and whether that
//! authority is still valid NOW. It is a generic platform capability: first-party and
//! external applications use the same model, and nothing here knows any consumer
//! product or business object.
//!
//! It owns contexts, context participants / channels / directions, relay sessions and
//! their authorization snapshots, authorization decisions, immutable audit evidence, the
//! transactional outbox, command idempotency, durable active-call enforcement jobs and
//! the Contact Vault (protected contacts, their access audit; §31+) and the temporary
//! relay-number pool (§34). It does NOT own
//! calls, call sessions, endpoints, RTC rooms, meetings or telephony legs.
//!
//! It shares no source and no database with any other service: Calling reaches it over
//! authenticated internal HTTP (`packages/service-auth`), and it reaches Calling's
//! enforcement API the same way.

pub mod audit;
pub mod clock;
pub mod commands;
pub mod config;
pub mod contact_vault;
pub mod context;
pub(crate) mod db;
pub mod enforcement;
pub mod error;
pub mod events;
pub mod internal;
pub mod middleware;
pub mod models;
pub mod relay;
pub mod relay_numbers;
pub mod retention;
pub mod routes;
pub mod scope;
pub mod service;
pub mod state;
pub mod vocabulary;
pub mod workers;

//! CF Meet Admin Bridge (ADR 0068, `docs/admin/cfmeet-admin-bridge.md`): the only
//! doorway from the CosmicForge central admin (HQ) into CF Meet.
//!
//! Phase A0 ships the contract only: the capability catalog that HQ pins
//! (`docs/contract/`), the wire rules shared with the future runtime (`wire`) and the
//! published error codes (`errors`). The runtime (security chain, audit, health,
//! discovery, datasets) is phase A1.

pub mod chain;
pub mod config;
pub mod contract;
pub mod errors;
pub mod metrics;
pub mod policy;
pub mod routes;
pub mod server;
pub mod store;
pub mod wire;

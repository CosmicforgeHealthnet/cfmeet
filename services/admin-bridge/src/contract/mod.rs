//! The HQ interface v1 contract: catalog (source of truth), schemas, datasets, event
//! adapters, rendering of `docs/contract/` and the breaking-change gate.

pub mod catalog;
pub mod compat;
pub mod datasets;
pub mod events;
pub mod model;
pub mod owner_commands;
pub mod render;
pub mod schemas;
pub mod schemas_calling;
pub mod schemas_commands;
pub mod schemas_communications;
pub mod schemas_identity;
pub mod schemas_meetings;
pub mod schemas_messaging;
pub mod schemas_partners;
pub mod schemas_verification;

//! The HQ interface v1 contract: catalog (source of truth), schemas, datasets, event
//! adapters, rendering of `docs/contract/` and the breaking-change gate.

pub mod catalog;
pub mod compat;
pub mod datasets;
pub mod events;
pub mod model;
pub mod render;
pub mod schemas;

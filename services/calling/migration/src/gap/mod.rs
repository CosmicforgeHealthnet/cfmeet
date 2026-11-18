//! The gap-closure migrations ([`crate::GapClosureMigrator`], table
//! `calling_gap_closure_seaql_migrations`): additive changes kept out of the main
//! history so the previous image can still start against a database that has them.

pub mod m20261203_000052_call_host_controls;
pub mod m20261204_000053_call_history_visibility;
pub mod m20261205_000054_direct_call_routing;

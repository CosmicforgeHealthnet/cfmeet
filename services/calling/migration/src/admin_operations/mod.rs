//! The admin-operations history ([`crate::AdminOperationsMigrator`], table
//! `calling_admin_operations_seaql_migrations`): ADR 0068 phase A4, the commands the HQ
//! admin bridge relays. Its own history for the same rollback-safety reason as the
//! call-kind-offer one: the previous image fails closed on any migration of the main,
//! gap-closure or call-kind-offer history it does not know, and never reads this table.

pub mod m20261210_000058_calling_admin_operations;
pub mod m20261211_000059_calling_admin_bridge_events;

//! The gap-closure migrations ([`crate::GapClosureMigrator`], table
//! `developer_platform_gap_closure_seaql_migrations`): additive changes kept out of the
//! main history so the previous image can still start against a database that has them.

pub mod m20261206_000001_retention_purge;
pub mod m20261207_000002_platform_build;
pub mod m20261208_000003_apps;

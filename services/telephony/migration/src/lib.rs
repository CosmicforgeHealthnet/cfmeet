//! Migration history owned by the Telephony service (`services/telephony/`).
//!
//! Independent of every other service: it never creates, alters or references another
//! service's tables (no calls, participants, sessions, contexts or protected contacts —
//! Calling and Privacy Relay ids are opaque UUIDs here, never foreign keys) and records
//! applied migrations in its own `telephony_seaql_migrations` table.

pub use sea_orm_migration::prelude::*;

mod m20261008_000001_providers_routes;
mod m20261008_000002_telephony_legs_bindings;
mod m20261008_000003_commands_events;
mod m20261008_000004_bridges_usage;
mod m20261008_000005_incidents_reconciliation;
mod m20261008_000006_ingress_security;

/// Name of the table recording which Telephony migrations have been applied.
pub const MIGRATION_TABLE: &str = "telephony_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261008_000001_providers_routes::Migration),
            Box::new(m20261008_000002_telephony_legs_bindings::Migration),
            Box::new(m20261008_000003_commands_events::Migration),
            Box::new(m20261008_000004_bridges_usage::Migration),
            Box::new(m20261008_000005_incidents_reconciliation::Migration),
            Box::new(m20261008_000006_ingress_security::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

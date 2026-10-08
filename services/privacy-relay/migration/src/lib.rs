//! Migration history owned by the Privacy Relay service (`services/privacy-relay/`).
//!
//! Independent of every other service: it never creates, alters or references another
//! service's tables, and records applied migrations in its own
//! `privacy_relay_seaql_migrations` table. Telephony tables (carrier legs, provider
//! callbacks) are deliberately absent: they belong to the future Telephony service.

pub use sea_orm_migration::prelude::*;

mod m20261008_000001_communication_contexts;
mod m20261008_000002_context_participants_directions_channels;
mod m20261008_000003_privacy_relay_sessions;
mod m20261008_000004_relay_audit_and_outbox;
mod m20261008_000005_commands_and_enforcement;
mod m20261008_000006_contact_vault;

/// Name of the table recording which Privacy Relay migrations have been applied.
pub const MIGRATION_TABLE: &str = "privacy_relay_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261008_000001_communication_contexts::Migration),
            Box::new(m20261008_000002_context_participants_directions_channels::Migration),
            Box::new(m20261008_000003_privacy_relay_sessions::Migration),
            Box::new(m20261008_000004_relay_audit_and_outbox::Migration),
            Box::new(m20261008_000005_commands_and_enforcement::Migration),
            Box::new(m20261008_000006_contact_vault::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

//! Migration history owned by the Messaging service (`services/messaging/`).
//!
//! Messaging (Part V) owns conversations, Messaging devices and their PUBLIC MLS material,
//! the key-transparency log, idempotency records and its outbox. Its history is recorded in
//! its own `messaging_seaql_migrations` table and is independent of every other service:
//! no table references another service's database (principals, tenants, applications and
//! environments are opaque Platform Access ids). No column may hold an E2EE private key, a
//! group / epoch secret or a plaintext E2EE message body (repository guard
//! `E2EE_SECRET_IN_SCHEMA`, migration tests).

pub use sea_orm_migration::prelude::*;

mod m20261101_000001_messaging_foundation;
mod m20261102_000002_conversations;

/// Name of the table recording which Messaging migrations have been applied.
pub const MIGRATION_TABLE: &str = "messaging_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261101_000001_messaging_foundation::Migration),
            Box::new(m20261102_000002_conversations::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

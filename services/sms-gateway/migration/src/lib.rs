//! Migration history owned by the SMS Gateway service (`services/sms-gateway/`).
//!
//! The SMS Gateway is the authoritative TRANSPORT service for SMS (Part IV Workstreams
//! M–O): message transports, submission attempts, provider message bindings, delivery /
//! inbound provider events, transport threads, usage records and opt-out blocks. Its
//! history is independent of every other service and is recorded in its own
//! `sms_gateway_seaql_migrations` table. No table references another service's database:
//! numbers are Numbering's opaque Number Resource ids, platform identities are opaque CF
//! Meet ids verified through Platform Access. Message content and remote numbers are
//! stored only sealed (AEAD); the plaintext never reaches a column, an event or a log.

pub use sea_orm_migration::prelude::*;

mod m20261025_000001_sms_foundation;
mod m20261027_000002_sms_outbound;
mod m20261028_000003_sms_bridge;

/// Name of the table recording which SMS Gateway migrations have been applied.
pub const MIGRATION_TABLE: &str = "sms_gateway_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261025_000001_sms_foundation::Migration),
            Box::new(m20261027_000002_sms_outbound::Migration),
            Box::new(m20261028_000003_sms_bridge::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

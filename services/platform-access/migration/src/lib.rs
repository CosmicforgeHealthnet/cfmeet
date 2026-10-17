//! Migration history owned by the Platform Access service (`services/platform-access/`).
//!
//! Platform Access is the single authority for the shared platform identity hierarchy
//! (Tenant → Application → Environment → Service Account → API Credential, application
//! scopes and environment-scoped communication principals). Its history is independent of
//! every other service and is recorded in its own `platform_access_seaql_migrations`
//! table.

pub use sea_orm_migration::prelude::*;

mod m20261016_000001_identity_hierarchy;
mod m20261016_000002_identity_change_feed;
mod m20261016_000003_access_operations;

/// Name of the table recording which Platform Access migrations have been applied.
pub const MIGRATION_TABLE: &str = "platform_access_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261016_000001_identity_hierarchy::Migration),
            Box::new(m20261016_000002_identity_change_feed::Migration),
            Box::new(m20261016_000003_access_operations::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

//! Migration history owned by the Calling service (`services/calling/`).
//!
//! This history is independent of the Meeting Service migrations: it never creates,
//! alters or references Meeting Service tables, and it records applied migrations in its
//! own `calling_seaql_migrations` table so it stays unambiguous even if an operator
//! points both services at the same PostgreSQL server.

pub use sea_orm_migration::prelude::*;

mod m20261007_000001_platform_consumers;
mod m20261007_000002_platform_credentials;
mod m20261007_000003_platform_principals;
mod m20261007_000004_platform_limits;

/// Name of the table recording which Calling migrations have been applied.
pub const MIGRATION_TABLE: &str = "calling_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261007_000001_platform_consumers::Migration),
            Box::new(m20261007_000002_platform_credentials::Migration),
            Box::new(m20261007_000003_platform_principals::Migration),
            Box::new(m20261007_000004_platform_limits::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

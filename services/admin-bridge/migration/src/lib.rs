//! Migration history owned by the admin bridge (`services/admin-bridge/`, ADR 0068).
//!
//! Independent of every other service; recorded in its own `admin_bridge_seaql_migrations`
//! table. Every table is `admin_`-prefixed. The bridge stores no business data, no
//! personal content and no secret: HMAC keys are enrolled as SHA-256 digests only.

pub use sea_orm_migration::prelude::*;

pub mod m20261119_000001_admin_bridge_foundation;
pub mod m20261121_000001_admin_operations;

pub use m20261119_000001_admin_bridge_foundation::AUDIT_LOCK_KEY;

/// Name of the table recording which admin bridge migrations have been applied.
pub const MIGRATION_TABLE: &str = "admin_bridge_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261119_000001_admin_bridge_foundation::Migration),
            Box::new(m20261121_000001_admin_operations::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

//! Migration history owned by the Numbering service (`services/numbering/`).
//!
//! Numbering is the canonical authority for public Number Resources, provider bindings,
//! capabilities, inventory observations, reservations, DCN assignments, ownership,
//! routing profiles and number lifecycle (Part IV Workstreams C–D). Its history is
//! independent of every other service and is recorded in its own
//! `numbering_seaql_migrations` table. No table here references another service's
//! database: platform identities (tenant, application, environment, service account,
//! principal) are carried as opaque CF Meet ids verified through Platform Access.

pub use sea_orm_migration::prelude::*;

mod m20261016_000001_numbering_foundation;
mod m20261016_000002_number_domain;
mod m20261017_000003_capability_catalog;
mod m20261018_000004_kyc_eligibility;
mod m20261019_000005_provider_commands;
mod m20261020_000006_inventory_reservations;
mod m20261021_000007_provisioning_activation;

/// Name of the table recording which Numbering migrations have been applied.
pub const MIGRATION_TABLE: &str = "numbering_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261016_000001_numbering_foundation::Migration),
            Box::new(m20261016_000002_number_domain::Migration),
            Box::new(m20261017_000003_capability_catalog::Migration),
            Box::new(m20261018_000004_kyc_eligibility::Migration),
            Box::new(m20261019_000005_provider_commands::Migration),
            Box::new(m20261020_000006_inventory_reservations::Migration),
            Box::new(m20261021_000007_provisioning_activation::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

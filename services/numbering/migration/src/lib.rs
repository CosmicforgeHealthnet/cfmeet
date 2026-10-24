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
mod m20261022_000008_ownership_assignment;
mod m20261023_000009_routing_grants;
mod m20261024_000010_lifecycle_stewardship;

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
            Box::new(m20261022_000008_ownership_assignment::Migration),
            Box::new(m20261023_000009_routing_grants::Migration),
            Box::new(m20261024_000010_lifecycle_stewardship::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

/// The migrations of [`Migrator`] not yet recorded in [`MIGRATION_TABLE`] — READ-ONLY.
///
/// `MigratorTrait::get_pending_migrations` first runs `CREATE TABLE IF NOT EXISTS` on the
/// migration table, which the least-privilege runtime role
/// (`infra/deployment/postgres/provision-service-database.sh`: no DDL) is refused
/// (sqlstate 42501) even when the table exists — the service could neither start nor
/// report ready under its own role. This only `SELECT`s.
pub async fn pending_migrations<C>(db: &C) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!("SELECT to_regclass('{MIGRATION_TABLE}') IS NOT NULL AS present"),
        ))
        .await?
        .map(|r| r.try_get::<bool>("", "present"))
        .transpose()?
        .unwrap_or(false);
    if !present {
        return Ok(known);
    }
    let applied = db
        .query_all(Statement::from_string(
            backend,
            format!("SELECT version FROM {MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    // Same fail-closed rule as `get_pending_migrations`: a migration applied to the
    // database but unknown to this binary (an older binary on a newer schema) is an error.
    let mut unknown: Vec<&String> = applied.iter().filter(|v| !known.contains(v)).collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            unknown
                .iter()
                .map(|v| format!("Migration file of version '{v}' is missing, this migration has been applied but its file is missing"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

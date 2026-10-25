//! Migration history owned by the Privacy Relay service (`services/privacy-relay/`).
//!
//! Independent of every other service: it never creates, alters or references another
//! service's tables, and records applied migrations in its own
//! `privacy_relay_seaql_migrations` table. Telephony tables (carrier legs, provider
//! callbacks) are deliberately absent: they belong to the future Telephony service. The
//! temporary relay-number inventory is relay infrastructure, not a numbering product.

pub use sea_orm_migration::prelude::*;

mod m20261008_000001_communication_contexts;
mod m20261008_000002_context_participants_directions_channels;
mod m20261008_000003_privacy_relay_sessions;
mod m20261008_000004_relay_audit_and_outbox;
mod m20261008_000005_commands_and_enforcement;
mod m20261008_000006_contact_vault;
mod m20261008_000007_contact_fingerprints;
mod m20261008_000008_contact_verification_and_resolution;
mod m20261008_000009_relay_number_inventory;
mod m20261008_000010_relay_number_assignments;
mod m20261008_000011_relay_inbound_resolutions;
mod m20261010_000012_relay_outbox_publication;
mod m20261010_000013_retention_and_contact_erasure;
mod m20261015_000014_incidents_and_enforcement_dead_letter;
mod m20261024_000015_numbering_relay_leases;
mod m20261026_000016_messaging_authority_jobs;
mod m20261101_000017_public_event_destinations;

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
            Box::new(m20261008_000007_contact_fingerprints::Migration),
            Box::new(m20261008_000008_contact_verification_and_resolution::Migration),
            Box::new(m20261008_000009_relay_number_inventory::Migration),
            Box::new(m20261008_000010_relay_number_assignments::Migration),
            Box::new(m20261008_000011_relay_inbound_resolutions::Migration),
            Box::new(m20261010_000012_relay_outbox_publication::Migration),
            Box::new(m20261010_000013_retention_and_contact_erasure::Migration),
            Box::new(m20261015_000014_incidents_and_enforcement_dead_letter::Migration),
            Box::new(m20261024_000015_numbering_relay_leases::Migration),
            Box::new(m20261026_000016_messaging_authority_jobs::Migration),
            Box::new(m20261101_000017_public_event_destinations::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

/// The migrations of [`Migrator`] not yet recorded in [`MIGRATION_TABLE`] — READ-ONLY.
///
/// `MigratorTrait::get_pending_migrations` first (re)creates the migration table with DDL,
/// which the least-privilege runtime role
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

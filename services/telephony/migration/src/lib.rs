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
mod m20261009_000007_enterprise_sip;
mod m20261009_000008_media_capture;
mod m20261009_000009_recording_capture;
mod m20261010_000010_callback_ingress_incidents;
mod m20261010_000011_operator_audit;
mod m20261010_000012_destination_classes;
mod m20261010_000013_leg_relay_assignment;
mod m20261010_000014_retention;
mod m20261015_000015_outbox_dead_letter;
mod m20261024_000016_dcn_voice;
mod m20261027_000017_synthetic_interconnect;

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
            Box::new(m20261009_000007_enterprise_sip::Migration),
            Box::new(m20261009_000008_media_capture::Migration),
            Box::new(m20261009_000009_recording_capture::Migration),
            Box::new(m20261010_000010_callback_ingress_incidents::Migration),
            Box::new(m20261010_000011_operator_audit::Migration),
            Box::new(m20261010_000012_destination_classes::Migration),
            Box::new(m20261010_000013_leg_relay_assignment::Migration),
            Box::new(m20261010_000014_retention::Migration),
            Box::new(m20261015_000015_outbox_dead_letter::Migration),
            Box::new(m20261024_000016_dcn_voice::Migration),
            Box::new(m20261027_000017_synthetic_interconnect::Migration),
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

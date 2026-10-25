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
mod m20261101_000004_public_event_destinations;

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
            Box::new(m20261101_000004_public_event_destinations::Migration),
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

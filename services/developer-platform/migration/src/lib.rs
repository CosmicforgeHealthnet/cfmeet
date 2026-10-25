//! Migration history owned by the Developer Platform (`services/developer-platform/`).
//!
//! The Developer Platform is the Part VI developer / enterprise control plane. Its history
//! is independent of every other service and is recorded in its own
//! `developer_platform_seaql_migrations` table. Every table is `devplat_`-prefixed.
//!
//! Authority rule: Platform Access is the ONLY authority for tenants, applications,
//! environments, service accounts, API credentials, scopes and principals. This schema
//! stores their canonical Platform Access ids as opaque references and nothing else about
//! them — no name, slug, status, classification, scope, credential, verifier or secret —
//! and no communications content and no E2EE material. The only secret it holds is its
//! OWN: customer webhook signing secrets (Section 53), stored sealed under a deployment
//! key and never returned after creation / rotation.

pub use sea_orm_migration::prelude::*;

mod m20261101_000001_developer_platform_foundation;
mod m20261102_000002_control_plane;
mod m20261103_000003_api_usage;
mod m20261104_000004_webhooks;
mod m20261105_000005_webhook_migration;
mod m20261110_000010_api_operation_usage;
mod m20261120_000011_credentials_production_usage;
mod m20261201_000012_event_rejections;

/// Name of the table recording which Developer Platform migrations have been applied.
pub const MIGRATION_TABLE: &str = "developer_platform_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261101_000001_developer_platform_foundation::Migration),
            Box::new(m20261102_000002_control_plane::Migration),
            Box::new(m20261103_000003_api_usage::Migration),
            Box::new(m20261104_000004_webhooks::Migration),
            Box::new(m20261105_000005_webhook_migration::Migration),
            Box::new(m20261110_000010_api_operation_usage::Migration),
            Box::new(m20261120_000011_credentials_production_usage::Migration),
            Box::new(m20261201_000012_event_rejections::Migration),
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

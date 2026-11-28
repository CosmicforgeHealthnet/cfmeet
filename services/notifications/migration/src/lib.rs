//! Migration history owned by the Notification Service (`services/notifications/`).
//!
//! The Notification Service is the single notification delivery authority of CF Meet
//! (docs/audits/notification-service, roadmap N2). Its history is independent of every
//! other service and is recorded in its own `notifications_seaql_migrations` table. Every
//! table is `notif_`-prefixed.
//!
//! Authority rule: recipients are opaque references (`cfmeet-user:<uuid>`, …); addresses,
//! subjects, bodies and push handles are sealed at rest; codes and tokens exist only in
//! sealed bodies and are erased at the final state.

pub use sea_orm_migration::prelude::*;

mod m20261116_000001_notifications_foundation;
mod m20261117_000001_preference_copy;
mod m20261118_000001_account_notices;
mod m20261119_000001_partner_push;
mod m20261120_000001_admin_operations;
mod m20261121_000001_organization_exports;
mod m20261220_000001_messaging_status_notices;

/// Name of the table recording which Notifications migrations have been applied.
pub const MIGRATION_TABLE: &str = "notifications_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261116_000001_notifications_foundation::Migration),
            Box::new(m20261117_000001_preference_copy::Migration),
            // Owner decision A (N12 follow-up): the ACCOUNT_NOTICES / EMAIL routing row.
            Box::new(m20261118_000001_account_notices::Migration),
            // ADR 0063 (phase B5): the partner push provider vault, partner registrations
            // and the platform-principal:<env>:<principal> recipient (additive).
            Box::new(m20261119_000001_partner_push::Migration),
            // ADR 0068 (admin bridge A4): the bridge command ledger and the suppression
            // row version (additive).
            Box::new(m20261120_000001_admin_operations::Migration),
            // ADR 0068 A4: the ORGANIZATIONS / EMAIL routing row (organization export notice).
            Box::new(m20261121_000001_organization_exports::Migration),
            // ADR 0069: the MESSAGING_STATUS / IN_APP and PUSH routing rows (Messaging status
            // mention and share notices) and the OPEN_STATUS inbox action.
            Box::new(m20261220_000001_messaging_status_notices::Migration),
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
/// (`infra/deployment/postgres/provision-service-database.sh`: no DDL) is refused. This
/// only `SELECT`s.
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
    // Fail closed: a migration applied to the database but unknown to this binary (an
    // older binary on a newer schema) is an error.
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

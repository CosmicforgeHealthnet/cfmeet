//! Profile and notifications history (Blueprint v1.0 Sections 9 and 11): `up` applies it;
//! other subcommands use the standard CLI against the same separate history table.
#[async_std::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
    sea_orm_migration::cli::run_cli(migration::ProfileNotificationsMigrator).await;
}

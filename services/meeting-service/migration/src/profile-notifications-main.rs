//! Profile and notifications history (Blueprint v1.0 Sections 9 and 11): `up` applies it;
//! other subcommands use the standard CLI against the same separate history table.
#[async_std::main]
async fn main() {
    sea_orm_migration::cli::run_cli(migration::ProfileNotificationsMigrator).await;
}

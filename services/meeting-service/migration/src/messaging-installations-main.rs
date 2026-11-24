#[async_std::main]
async fn main() {
    cf_migration_guard::enforce_from_env();
    sea_orm_migration::cli::run_cli(migration::MessagingInstallationsMigrator).await;
}

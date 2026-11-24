#[tokio::main]
async fn main() {
    cf_migration_guard::enforce_from_env();
    sea_orm_migration::cli::run_cli(messaging_migration::HistoryTransferV2Migrator).await;
}

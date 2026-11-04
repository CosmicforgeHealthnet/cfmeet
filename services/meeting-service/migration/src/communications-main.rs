#[async_std::main]
async fn main() {
    sea_orm_migration::cli::run_cli(migration::CommunicationsMigrator).await;
}

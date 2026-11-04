#[tokio::main]
async fn main() {
    sea_orm_migration::cli::run_cli(messaging_migration::CommunicationsMigrator).await;
}

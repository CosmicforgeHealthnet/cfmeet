#[tokio::main]
async fn main() {
    sea_orm_migration::cli::run_cli(communications_migration::Migrator).await;
}

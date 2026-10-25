use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // Same .env as the Developer Platform (services/developer-platform/.env), then the
    // current directory.
    dotenvy::from_filename("../.env").ok();
    if std::env::var("DATABASE_URL").is_err() {
        dotenvy::dotenv().ok();
    }
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("ERROR: DATABASE_URL is not set (see services/developer-platform/.env.example).");
        std::process::exit(1);
    }
    cli::run_cli(developer_platform_migration::Migrator).await;
}

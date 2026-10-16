use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // Same .env as the Platform Access service (services/platform-access/.env), then the
    // current directory.
    dotenvy::from_filename("../.env").ok();
    if std::env::var("DATABASE_URL").is_err() {
        dotenvy::dotenv().ok();
    }
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("ERROR: DATABASE_URL is not set (see services/platform-access/.env.example).");
        std::process::exit(1);
    }
    cli::run_cli(platform_access_migration::Migrator).await;
}

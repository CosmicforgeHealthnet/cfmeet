use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // Same .env as the Messaging service (services/messaging/.env), then the current
    // directory.
    dotenvy::from_filename("../.env").ok();
    if std::env::var("DATABASE_URL").is_err() {
        dotenvy::dotenv().ok();
    }
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("ERROR: DATABASE_URL is not set (see services/messaging/.env.example).");
        std::process::exit(1);
    }
    cli::run_cli(messaging_migration::Migrator).await;
}

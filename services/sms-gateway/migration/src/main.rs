use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // Same .env as the SMS Gateway service (services/sms-gateway/.env), then the current
    // directory.
    dotenvy::from_filename("../.env").ok();
    if std::env::var("DATABASE_URL").is_err() {
        dotenvy::dotenv().ok();
    }
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("ERROR: DATABASE_URL is not set (see services/sms-gateway/.env.example).");
        std::process::exit(1);
    }
    cli::run_cli(sms_gateway_migration::Migrator).await;
}

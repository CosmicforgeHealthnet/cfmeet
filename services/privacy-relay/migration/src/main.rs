use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
    // Same .env as the Privacy Relay service (services/privacy-relay/.env), then the
    // current directory.
    dotenvy::from_filename("../.env").ok();
    if std::env::var("DATABASE_URL").is_err() {
        dotenvy::dotenv().ok();
    }
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("ERROR: DATABASE_URL is not set (see services/privacy-relay/.env.example).");
        std::process::exit(1);
    }
    cli::run_cli(privacy_relay_migration::Migrator).await;
}

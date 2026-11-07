use sea_orm_migration::prelude::*;

#[async_std::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
    // Load .env from backend root directory (parent of migration folder)
    // This ensures migration uses the same .env as the main application
    dotenvy::from_filename("../.env").ok();

    // Fallback: try current directory if parent doesn't have .env
    if std::env::var("DATABASE_URL").is_err() {
        dotenvy::dotenv().ok();
    }

    // Verify DATABASE_URL is set
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("ERROR: DATABASE_URL environment variable is not set!");
        eprintln!("Please ensure backend/.env contains DATABASE_URL");
        std::process::exit(1);
    }

    cli::run_cli(migration::Migrator).await;
}

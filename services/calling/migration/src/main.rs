use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
    // Same .env as the Calling service (services/calling/.env), then the current directory.
    dotenvy::from_filename("../.env").ok();
    if std::env::var("DATABASE_URL").is_err() {
        dotenvy::dotenv().ok();
    }
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("ERROR: DATABASE_URL is not set (see services/calling/.env.example).");
        std::process::exit(1);
    }
    // `up` applies the main history, then the separate gap-closure history; every other
    // subcommand concerns the main history only (`reset` first rolls the gap-closure
    // history back; `fresh` drops every table, the gap-closure one included).
    let command = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_default();
    let up = command == "up";
    if command == "reset" {
        // The gap-closure history depends on the main one: it is rolled back first.
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
        let db = sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .unwrap_or_else(|e| {
                eprintln!("ERROR: cannot connect for the gap-closure migrations: {e}");
                std::process::exit(1);
            });
        if let Err(e) = calling_migration::GapClosureMigrator::down(&db, None).await {
            eprintln!("ERROR: gap-closure rollback failed: {e}");
            std::process::exit(1);
        }
    }
    cli::run_cli(calling_migration::Migrator).await;
    if up {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
        let db = sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .unwrap_or_else(|e| {
                eprintln!("ERROR: cannot connect for the gap-closure migrations: {e}");
                std::process::exit(1);
            });
        if let Err(e) = calling_migration::GapClosureMigrator::up(&db, None).await {
            eprintln!("ERROR: gap-closure migrations failed: {e}");
            std::process::exit(1);
        }
        println!("Gap-closure migrations applied.");
    }
}

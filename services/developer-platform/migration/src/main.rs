use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
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
    // `up` applies the main history, then the separate gap-closure history; `reset` first
    // rolls the gap-closure history back; every other subcommand concerns the main history.
    let command = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_default();
    let connect = || async {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
        sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .unwrap_or_else(|e| {
                eprintln!("ERROR: cannot connect for the gap-closure migrations: {e}");
                std::process::exit(1);
            })
    };
    if command == "reset" {
        if let Err(e) =
            developer_platform_migration::GapClosureMigrator::down(&connect().await, None).await
        {
            eprintln!("ERROR: gap-closure rollback failed: {e}");
            std::process::exit(1);
        }
    }
    cli::run_cli(developer_platform_migration::Migrator).await;
    if command == "up" {
        if let Err(e) =
            developer_platform_migration::GapClosureMigrator::up(&connect().await, None).await
        {
            eprintln!("ERROR: gap-closure migrations failed: {e}");
            std::process::exit(1);
        }
        println!("Gap-closure migrations applied.");
    }
}

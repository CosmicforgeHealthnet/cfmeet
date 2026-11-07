use sea_orm_migration::sea_orm::Database;

/// `up` applies every history (foundation, Sections 9-23, Section 14). Other subcommands
/// (`status`, `fresh` and so on) keep the standard CLI for the foundation history.
#[tokio::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "up" {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
        let db = Database::connect(url)
            .await
            .expect("Communications migration database connection failed");
        communications_migration::up_all(&db)
            .await
            .expect("Communications migrations failed");
        println!("Communications migrations applied");
        return;
    }
    sea_orm_migration::cli::run_cli(communications_migration::Migrator).await;
}

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
    // `up` applies the main history, then the notification-publications one (roadmap N7,
    // its own history for rollback safety). Other subcommands (`status`, `fresh`, `up -n`
    // and so on) keep the standard CLI for the main history.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "up" {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is set");
        let db = sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .expect("Calling migration database connection failed");
        calling_migration::up_all(&db)
            .await
            .expect("Calling migrations failed");
        println!("Calling migrations applied (main and notification-publications histories)");
        return;
    }
    // The destructive subcommands roll back the main history, whose tables the
    // notification-publications history references: roll that one back first (the SEC-015
    // guard above already refused these outside a non-production APP_ENV).
    if matches!(
        args.first().map(String::as_str),
        Some("down" | "fresh" | "refresh" | "reset")
    ) {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is set");
        let db = sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .expect("Calling migration database connection failed");
        calling_migration::NotificationPublicationsMigrator::reset(&db)
            .await
            .expect("Calling notification-publications rollback failed");
    }
    cli::run_cli(calling_migration::Migrator).await;
}

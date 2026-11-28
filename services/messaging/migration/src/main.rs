use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
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
    // `up` (or no subcommand) also applies the gap-closure history, after the main one.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || (args.len() == 1 && args[0] == "up") {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is set");
        let db = sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .expect("Messaging migration database connection failed");
        messaging_migration::Migrator::up(&db, None)
            .await
            .expect("Messaging migrations failed");
        messaging_migration::GapClosureMigrator::up(&db, None)
            .await
            .expect("Messaging gap-closure migrations failed");
        messaging_migration::MlsJoinMigrator::up(&db, None)
            .await
            .expect("Messaging MLS self-join migrations failed");
        messaging_migration::MemberNotificationsMigrator::up(&db, None)
            .await
            .expect("Messaging member-notification migrations failed");
        messaging_migration::ChatControlsMigrator::up(&db, None)
            .await
            .expect("Messaging chat-controls migrations failed");
        messaging_migration::AdminOperationsMigrator::up(&db, None)
            .await
            .expect("Messaging admin-operations migrations failed");
        messaging_migration::ChatOrganizationMigrator::up(&db, None)
            .await
            .expect("Messaging chat-organization migrations failed");
        messaging_migration::SocialMigrator::up(&db, None)
            .await
            .expect("Messaging status / channels migrations failed");
        println!("Messaging migrations applied");
        return;
    }
    // The gap-closure history depends on the main one: the destructive subcommands roll
    // it back first (the SEC-015 guard above already refused them outside a non-production
    // APP_ENV).
    if matches!(
        args.first().map(String::as_str),
        Some("down" | "fresh" | "refresh" | "reset")
    ) {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is set");
        let db = sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .expect("Messaging migration database connection failed");
        messaging_migration::SocialMigrator::reset(&db)
            .await
            .expect("Messaging status / channels rollback failed");
        messaging_migration::ChatOrganizationMigrator::reset(&db)
            .await
            .expect("Messaging chat-organization rollback failed");
        messaging_migration::AdminOperationsMigrator::reset(&db)
            .await
            .expect("Messaging admin-operations rollback failed");
        messaging_migration::ChatControlsMigrator::reset(&db)
            .await
            .expect("Messaging chat-controls rollback failed");
        messaging_migration::MemberNotificationsMigrator::reset(&db)
            .await
            .expect("Messaging member-notification rollback failed");
        messaging_migration::MlsJoinMigrator::reset(&db)
            .await
            .expect("Messaging MLS self-join rollback failed");
        messaging_migration::GapClosureMigrator::reset(&db)
            .await
            .expect("Messaging gap-closure rollback failed");
    }
    cli::run_cli(messaging_migration::Migrator).await;
}

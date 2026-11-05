/// `up` applies the Account Communications history, then the separate Sections 9-23
/// facade history. Other subcommands keep the standard CLI for the original history.
#[async_std::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "up" {
        use sea_orm_migration::MigratorTrait;
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
        let db = sea_orm_migration::sea_orm::Database::connect(url)
            .await
            .expect("Account Communications migration database connection failed");
        migration::CommunicationsMigrator::up(&db, None)
            .await
            .expect("Account Communications migrations failed");
        migration::CommunicationsFacadeMigrator::up(&db, None)
            .await
            .expect("Account Communications facade migrations failed");
        println!("Account Communications migrations applied");
        return;
    }
    sea_orm_migration::cli::run_cli(migration::CommunicationsMigrator).await;
}

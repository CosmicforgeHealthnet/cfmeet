use sea_orm_migration::sea_orm::Database;

/// `up` applies both histories (foundation, then Sections 9-23). Other subcommands
/// (`status`, `fresh` and so on) keep the standard CLI for the foundation history.
#[tokio::main]
async fn main() {
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

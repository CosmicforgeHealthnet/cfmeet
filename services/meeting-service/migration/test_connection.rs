use sea_orm_migration::sea_orm::Database;

#[async_std::main]
async fn main() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");
    
    println!("Connecting to database...");
    println!("URL: {}", database_url);
    
    match Database::connect(&database_url).await {
        Ok(db) => {
            println!("✅ Successfully connected to database!");
            drop(db);
        }
        Err(e) => {
            println!("❌ Failed to connect: {:?}", e);
        }
    }
}

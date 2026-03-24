use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Execute raw SQL to add 'guest' to the enum type
        // This is Postgres specific syntax
        manager
            .get_connection()
            .execute_unprepared("ALTER TYPE user_role ADD VALUE IF NOT EXISTS 'guest'")
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Enum values cannot be removed in Postgres easily
        // We can leave it or try to recreate the type (dangerous)
        // For now, we will do nothing in down migration as adding a value is generally backward compatible
        Ok(())
    }
}

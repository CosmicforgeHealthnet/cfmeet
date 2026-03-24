use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add 'media_toggle' to the event_type enum
        manager
            .get_connection()
            .execute_unprepared("ALTER TYPE event_type ADD VALUE IF NOT EXISTS 'media_toggle';")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Note: PostgreSQL doesn't support removing enum values directly
        // This would require recreating the enum type
        // For simplicity, we'll leave the value in place
        Ok(())
    }
}

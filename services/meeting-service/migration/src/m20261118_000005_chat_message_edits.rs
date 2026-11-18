//! Gap audit M17: meeting chat messages kept in the Meeting Service (rows not yet moved
//! to Messaging) can be edited by their sender; `edited_at` marks an edited message.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE conference_messages ADD COLUMN IF NOT EXISTS edited_at TIMESTAMPTZ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conference_messages DROP COLUMN IF EXISTS edited_at")
            .await?;
        Ok(())
    }
}

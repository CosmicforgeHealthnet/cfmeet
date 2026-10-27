//! Part V Appendix K (G-31): read receipts configurable per user and per conversation
//! (on top of the environment policy). Message requests reuse the INVITED member state.
//! Also widens `messaging_presence_settings.visibility`, too short for one of its own
//! allowed values.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- Defect fix: the column was VARCHAR(16) while its CHECK allows 'CONVERSATION_MEMBERS'
-- (20 characters), so choosing that visibility explicitly always failed.
ALTER TABLE messaging_presence_settings ALTER COLUMN visibility TYPE VARCHAR(24);
ALTER TABLE messaging_presence_settings ADD COLUMN share_read_receipts BOOLEAN NOT NULL DEFAULT TRUE;
-- NULL: follow the policy; FALSE: no read receipts in this conversation.
ALTER TABLE messaging_conversations ADD COLUMN read_receipts BOOLEAN;
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_conversations DROP COLUMN IF EXISTS read_receipts;
ALTER TABLE messaging_presence_settings DROP COLUMN IF EXISTS share_read_receipts;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}

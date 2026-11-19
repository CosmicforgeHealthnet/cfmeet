//! CF Meet Apps (ADR 0064, integrate contract §7). Additive only.
//!
//! * `messaging_messages.via_application_id`: the CF Meet App whose OAuth credential sent a
//!   (MANAGED) message, so clients can label "sent by App X". `NULL` on every existing row
//!   and on every message not sent through an app.
//!
//! Bot events (`messaging.bot.mentioned` / `messaging.bot.message-received`) need no
//! schema: they are ordinary outbox events (the type CHECK admits them) queued in the
//! existing relay table by the service itself.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_messages ADD COLUMN IF NOT EXISTS via_application_id UUID;
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_messages DROP COLUMN IF EXISTS via_application_id;
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

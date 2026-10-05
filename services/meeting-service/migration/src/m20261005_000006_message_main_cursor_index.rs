//! Phase 2 Part M §42 — query-plan certification finding.
//!
//! Main-room chat history (`messaging.rs`: `conference_id = $1 AND breakout_room_id IS
//! NULL AND recipient_participant_id IS NULL ORDER BY created_at DESC, id DESC LIMIT n`)
//! could not use `idx_messages_room_cursor` for its ordering (PostgreSQL does not treat
//! `IS NULL` as an equality for ORDER BY), so it sequentially scanned
//! `conference_messages` at representative volume. This partial index serves the
//! newest-first page and the (created_at, id) cursor directly. Additive only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE INDEX IF NOT EXISTS idx_messages_main_cursor ON conference_messages
    (conference_id, created_at DESC, id DESC)
    WHERE breakout_room_id IS NULL AND recipient_participant_id IS NULL;
"#;

const DOWN: &str = "DROP INDEX IF EXISTS idx_messages_main_cursor;";

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

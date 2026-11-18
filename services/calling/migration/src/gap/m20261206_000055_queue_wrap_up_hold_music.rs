//! Queue wrap-up time and hold music (gap closure C15, additive).
//!
//! * `enterprise_call_queues.wrap_up_seconds` (0-600, default 0 = no wrap-up): an agent
//!   whose previous call answered from this queue ended less than this many seconds ago is
//!   not offered another of the queue's calls yet.
//! * `enterprise_call_queues.hold_music` (NULL = none): a built-in hold-music choice from
//!   a controlled catalogue (the media plane / client maps each to its own audio; no audio
//!   or URL is stored in the Calling database).
//!
//! Rollback note: both columns have defaults the previous image never needs to write
//! (it inserts queues without them), so it keeps working against a database that has
//! them. DOWN drops the columns (configuration only, no evidence).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE enterprise_call_queues
    ADD COLUMN wrap_up_seconds INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN hold_music TEXT,
    ADD CONSTRAINT ck_enterprise_call_queues_wrap_up CHECK (wrap_up_seconds BETWEEN 0 AND 600),
    ADD CONSTRAINT ck_enterprise_call_queues_hold_music
        CHECK (hold_music IS NULL OR hold_music IN ('DEFAULT', 'CALM', 'BRIGHT'));
-- Wrap-up lookups: an agent's recent answered entries of a queue.
CREATE INDEX idx_enterprise_queue_entries_assigned
    ON enterprise_queue_entries (queue_id, assigned_principal_id, exited_at)
    WHERE state = 'ANSWERED';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_enterprise_queue_entries_assigned;
ALTER TABLE enterprise_call_queues
    DROP CONSTRAINT IF EXISTS ck_enterprise_call_queues_hold_music,
    DROP CONSTRAINT IF EXISTS ck_enterprise_call_queues_wrap_up,
    DROP COLUMN IF EXISTS hold_music,
    DROP COLUMN IF EXISTS wrap_up_seconds;
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

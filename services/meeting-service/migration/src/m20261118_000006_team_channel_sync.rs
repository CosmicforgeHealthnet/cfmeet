//! Team chats follow their team: the team chats the Meeting Service set up in Messaging,
//! who set each up (the organization manager its background resync acts as), and whether
//! a team event made it due for a resync. Organizations delivers `team.member_added`,
//! `team.member_removed` and `team.archived`; the worker re-runs the same channel sync.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS organization_team_channels (
    organization_id UUID NOT NULL,
    team_id         UUID NOT NULL,
    managed_by      UUID NOT NULL,
    title           TEXT NULL CHECK (title IS NULL OR char_length(title) <= 200),
    state           VARCHAR(16) NOT NULL DEFAULT 'ACTIVE'
                        CHECK (state IN ('ACTIVE', 'ARCHIVED', 'NEEDS_ADMIN')),
    sync_due        BOOLEAN NOT NULL DEFAULT false,
    archive_due     BOOLEAN NOT NULL DEFAULT false,
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error      VARCHAR(64) NULL,
    last_synced_at  TIMESTAMPTZ NULL,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, team_id)
);
CREATE INDEX IF NOT EXISTS organization_team_channels_due
    ON organization_team_channels (next_attempt_at) WHERE sync_due;
"#;

const DOWN: &str = "DROP TABLE IF EXISTS organization_team_channels;";

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

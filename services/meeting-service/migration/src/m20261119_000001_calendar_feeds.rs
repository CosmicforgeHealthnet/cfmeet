//! Audit MF2: private, revocable calendar feed URLs (webcal / ICS subscriptions listing a
//! person's meetings). One active feed per account; only the SHA-256 of the feed token is
//! stored. Additive only (a new table), in its own history (`meeting_audit_closure_migrations`)
//! so the previous image, which never reads this table, keeps starting after a rollback.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS calendar_feed_tokens (
                    id UUID PRIMARY KEY,
                    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                    token_hash TEXT NOT NULL UNIQUE CHECK (char_length(token_hash) = 64),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                    last_used_at TIMESTAMPTZ,
                    revoked_at TIMESTAMPTZ
                );
                CREATE UNIQUE INDEX IF NOT EXISTS uq_calendar_feed_tokens_active
                    ON calendar_feed_tokens (user_id) WHERE revoked_at IS NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS calendar_feed_tokens")
            .await?;
        Ok(())
    }
}

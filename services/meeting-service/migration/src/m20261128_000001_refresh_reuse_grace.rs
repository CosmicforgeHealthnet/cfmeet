//! Refresh-token reuse grace window (ADR 0070).
//!
//! Appended to the `account_security_migrations` history. Additive only: a nullable
//! column, so the previously deployed image keeps starting (its readiness check is
//! "applied ⊇ known") and keeps working (it never reads or writes the column; a session it
//! rotates simply has a stale `previous_refresh_id`, which the new image only honours
//! within the grace window after `last_refreshed_at`). Nobody is signed out.
//!
//! * `account_sessions.previous_refresh_id` — the refresh id superseded by the session's
//!   last rotation. Presented within the grace window it is a retry or race of that
//!   rotation, not theft. Like `current_refresh_id`, an identifier inside a signed token,
//!   useless without the signing key.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE account_sessions ADD COLUMN IF NOT EXISTS previous_refresh_id UUID NULL;
"#;

const DOWN: &str = r#"
ALTER TABLE account_sessions DROP COLUMN IF EXISTS previous_refresh_id;
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

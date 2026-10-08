//! Privacy-safe call evidence (Part D §36): the protected calls of one Privacy Relay
//! Session, newest first, without a full scan. Calling stays the only owner of call
//! records; Privacy Relay only proves the session belongs to the caller.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE INDEX idx_calls_privacy_relay_session
    ON calls (tenant_id, application_id, environment_id, privacy_relay_ref, created_at DESC, id DESC)
    WHERE privacy_relay_ref IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_calls_privacy_relay_session;
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

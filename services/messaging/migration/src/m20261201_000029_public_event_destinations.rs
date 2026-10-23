//! Part VI §53 — direct publication of public events to the Developer Platform.
//!
//! During the webhook cutover Messaging may relay each public event to Calling (today's
//! customer webhook delivery), to the Developer Platform, or to both (shadow comparison).
//! Each receiver's outcome is recorded on the relay row — `ACCEPTED` or `REJECTED`, `NULL`
//! while it has not settled — so a retry re-sends only to a receiver that has not accepted
//! the event. Codes only: the row still holds no copy of any payload.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_webhook_relay
    ADD COLUMN calling_outcome VARCHAR(16),
    ADD COLUMN developer_platform_outcome VARCHAR(16),
    ADD CONSTRAINT ck_messaging_webhook_relay_calling_outcome
        CHECK (calling_outcome IS NULL OR calling_outcome IN ('ACCEPTED', 'REJECTED')),
    ADD CONSTRAINT ck_messaging_webhook_relay_developer_platform_outcome
        CHECK (developer_platform_outcome IS NULL OR developer_platform_outcome IN ('ACCEPTED', 'REJECTED'));
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_webhook_relay
    DROP CONSTRAINT IF EXISTS ck_messaging_webhook_relay_developer_platform_outcome,
    DROP CONSTRAINT IF EXISTS ck_messaging_webhook_relay_calling_outcome,
    DROP COLUMN IF EXISTS developer_platform_outcome,
    DROP COLUMN IF EXISTS calling_outcome;
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

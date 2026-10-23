//! Part VI §53 — direct publication of public events to the Developer Platform.
//!
//! During the webhook cutover Privacy Relay may publish each committed envelope to Calling
//! (today's customer webhook delivery), to the Developer Platform, or to both (shadow
//! comparison). Each receiver's outcome is recorded on the outbox row — `ACCEPTED` or
//! `REJECTED`, `NULL` while it has not settled — so a retry re-sends only to a receiver
//! that has not accepted the event. Envelopes stay immutable; the columns are codes only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE privacy_relay_outbox
    ADD COLUMN calling_outcome VARCHAR(16),
    ADD COLUMN developer_platform_outcome VARCHAR(16),
    ADD CONSTRAINT ck_relay_outbox_calling_outcome
        CHECK (calling_outcome IS NULL OR calling_outcome IN ('ACCEPTED', 'REJECTED')),
    ADD CONSTRAINT ck_relay_outbox_developer_platform_outcome
        CHECK (developer_platform_outcome IS NULL OR developer_platform_outcome IN ('ACCEPTED', 'REJECTED'));
"#;

const DOWN: &str = r#"
ALTER TABLE privacy_relay_outbox
    DROP CONSTRAINT IF EXISTS ck_relay_outbox_developer_platform_outcome,
    DROP CONSTRAINT IF EXISTS ck_relay_outbox_calling_outcome,
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

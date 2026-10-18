//! SMS message events as customer webhooks (Part IV Workstream N): the SMS Gateway
//! (`cfmeet.sms-gateway`) becomes a third producer of public webhook events (`sms.*`),
//! next to Calling and Privacy Relay. The projected event body is still checked by the
//! existing constraints (normalized type, id, size); only the producer allow-list grows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE webhook_events
    DROP CONSTRAINT ck_webhook_events_producer,
    ADD CONSTRAINT ck_webhook_events_producer
        CHECK (producer IN ('cfmeet.calling', 'cfmeet.privacy-relay', 'cfmeet.sms-gateway'));
"#;

// Rows of the SMS Gateway cannot satisfy the old allow-list and webhook events are
// immutable, so the old constraint is restored NOT VALID (new rows are checked again).
const DOWN: &str = r#"
ALTER TABLE webhook_events
    DROP CONSTRAINT ck_webhook_events_producer,
    ADD CONSTRAINT ck_webhook_events_producer
        CHECK (producer IN ('cfmeet.calling', 'cfmeet.privacy-relay')) NOT VALID;
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

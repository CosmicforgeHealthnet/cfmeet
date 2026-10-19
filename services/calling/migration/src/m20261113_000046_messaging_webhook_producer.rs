//! Messaging events as customer webhooks (Part V I §§116–118): Messaging
//! (`cfmeet.messaging`) becomes a fourth producer of public webhook events
//! (`conversation.*`, `message.*`) through the EXISTING webhook system (subscriptions,
//! signing, delivery, retries, replay are unchanged). The projected event body is still
//! checked by the existing constraints; only the producer allow-list grows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE webhook_events
    DROP CONSTRAINT ck_webhook_events_producer,
    ADD CONSTRAINT ck_webhook_events_producer
        CHECK (producer IN ('cfmeet.calling', 'cfmeet.privacy-relay', 'cfmeet.sms-gateway', 'cfmeet.messaging'));
"#;

// Webhook events are immutable, so rows of Messaging stay and the previous allow-list is
// restored NOT VALID (new rows are checked again).
const DOWN: &str = r#"
ALTER TABLE webhook_events
    DROP CONSTRAINT ck_webhook_events_producer,
    ADD CONSTRAINT ck_webhook_events_producer
        CHECK (producer IN ('cfmeet.calling', 'cfmeet.privacy-relay', 'cfmeet.sms-gateway')) NOT VALID;
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

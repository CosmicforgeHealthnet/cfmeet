//! Gap audit M14: a meeting invitation email carries its calendar invite (an RFC 5545
//! `.ics` attachment). The body is kept with the queued email, like the HTML and text
//! bodies, and cleared with them once the email is sent or expires.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE email_deliveries ADD COLUMN IF NOT EXISTS ics_body TEXT \
                 CHECK (ics_body IS NULL OR octet_length(ics_body) <= 65536)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE email_deliveries DROP COLUMN IF EXISTS ics_body")
            .await?;
        Ok(())
    }
}

//! Owner-delivered exports (ADR 0068, admin bridge final round; additive; appended to the
//! `messaging_admin_operations_seaql_migrations` history, which no released image reads).
//!
//! `messaging_exports` gains the delivery of an export HQ requests for a data subject
//! (`messaging.export.request`): the export is built by the existing leased worker, but it
//! is delivered ONLY to the verified recipient named in the request, never to the tenant:
//!
//! * `delivery_channel` — `CONSOLE_REVEAL_ONCE` (the recipient claims it once, signed in to
//!   their own CF Meet console); NULL for the tenant's own exports (unchanged);
//! * `recipient_principal_id` — the data subject (a principal id, never an address);
//! * `delivery_ref` — the reference HQ is answered with (no token, no URL);
//! * `delivered_at` — when the single claim happened (a second claim is refused).
//!
//! Rollback: the previous image never writes these columns and its inserts leave them NULL
//! (the constraint holds). It does not know them either: drain or let owner-delivered
//! exports expire (7 days) before rolling back, otherwise its tenant download grant would
//! no longer exclude them.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_exports
    ADD COLUMN delivery_channel        VARCHAR(32),
    ADD COLUMN recipient_principal_id  UUID,
    ADD COLUMN delivery_ref            UUID,
    ADD COLUMN delivered_at            TIMESTAMPTZ,
    ADD CONSTRAINT ck_messaging_exports_delivery CHECK (
        (delivery_channel IS NULL AND recipient_principal_id IS NULL AND delivery_ref IS NULL AND delivered_at IS NULL)
        OR (delivery_channel = 'CONSOLE_REVEAL_ONCE' AND recipient_principal_id IS NOT NULL AND delivery_ref IS NOT NULL
            AND (delivered_at IS NULL OR status IN ('READY', 'EXPIRED'))));
CREATE INDEX ix_messaging_exports_recipient ON messaging_exports (environment_id, recipient_principal_id, created_at)
    WHERE recipient_principal_id IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS ix_messaging_exports_recipient;
ALTER TABLE messaging_exports DROP CONSTRAINT IF EXISTS ck_messaging_exports_delivery;
ALTER TABLE messaging_exports DROP COLUMN IF EXISTS delivered_at;
ALTER TABLE messaging_exports DROP COLUMN IF EXISTS delivery_ref;
ALTER TABLE messaging_exports DROP COLUMN IF EXISTS recipient_principal_id;
ALTER TABLE messaging_exports DROP COLUMN IF EXISTS delivery_channel;
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

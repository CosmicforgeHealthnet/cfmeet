//! C7 — inviting participants into an ongoing call (`POST /v1/calls/{id}/participants`).
//!
//! No new table or column: an invited principal is an ordinary RECIPIENT participant with
//! ordinary ring offers (CALL_PARTICIPANT_ADDED / CALL_INVITATION_ISSUED evidence). Only the
//! idempotency ledger gains the ADD_PARTICIPANTS operation (replays of the same request).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE call_commands
    DROP CONSTRAINT ck_call_commands_operation,
    ADD CONSTRAINT ck_call_commands_operation CHECK (operation IN (
        'CREATE_CALL', 'ACCEPT', 'DECLINE', 'CANCEL', 'END', 'HOLD', 'RESUME', 'TRANSFER',
        'REMOVE_SESSION', 'UPGRADE', 'START_SCREEN_SHARE', 'STOP_SCREEN_SHARE',
        'ADD_PARTICIPANTS'));
"#;

const DOWN: &str = r#"
ALTER TABLE call_commands
    DROP CONSTRAINT ck_call_commands_operation,
    ADD CONSTRAINT ck_call_commands_operation CHECK (operation IN (
        'CREATE_CALL', 'ACCEPT', 'DECLINE', 'CANCEL', 'END', 'HOLD', 'RESUME', 'TRANSFER',
        'REMOVE_SESSION', 'UPGRADE', 'START_SCREEN_SHARE', 'STOP_SCREEN_SHARE'));
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

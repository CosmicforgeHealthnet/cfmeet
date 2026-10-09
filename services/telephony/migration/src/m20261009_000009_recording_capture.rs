//! Call recording capture (Part F §61; ADR 0024).
//!
//! * `telephony_captures` also holds RECORDING captures (a call recording's audio; the
//!   leg's bridge when it is bridged, else its channel), bounded to 1440 s (one WAV
//!   delivery of at most 24 MiB).
//! * `telephony_commands` gains CAPTURE_STOP (stop a running capture early; never blocked
//!   by a kill switch).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE telephony_captures
    DROP CONSTRAINT ck_telephony_captures_purpose,
    ADD CONSTRAINT ck_telephony_captures_purpose CHECK (purpose IN ('VOICEMAIL', 'RECORDING')),
    DROP CONSTRAINT ck_telephony_captures_max,
    ADD CONSTRAINT ck_telephony_captures_max CHECK (
        max_duration_seconds BETWEEN 1 AND 1440
        AND (purpose <> 'VOICEMAIL' OR max_duration_seconds <= 600));

ALTER TABLE telephony_commands
    DROP CONSTRAINT ck_telephony_commands_kind,
    ADD CONSTRAINT ck_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'SEND_DTMF', 'QUERY_STATUS',
        'ANSWER', 'PROMPT', 'CAPTURE', 'CAPTURE_STOP'));
"#;

const DOWN: &str = r#"
ALTER TABLE telephony_commands
    DROP CONSTRAINT ck_telephony_commands_kind,
    ADD CONSTRAINT ck_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'SEND_DTMF', 'QUERY_STATUS',
        'ANSWER', 'PROMPT', 'CAPTURE'));
ALTER TABLE telephony_captures
    DROP CONSTRAINT ck_telephony_captures_max,
    ADD CONSTRAINT ck_telephony_captures_max CHECK (max_duration_seconds BETWEEN 1 AND 600),
    DROP CONSTRAINT ck_telephony_captures_purpose,
    ADD CONSTRAINT ck_telephony_captures_purpose CHECK (purpose IN ('VOICEMAIL'));
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

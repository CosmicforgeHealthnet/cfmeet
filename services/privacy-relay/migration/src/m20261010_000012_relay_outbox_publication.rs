//! Part G §67: Privacy Relay's transactional outbox gains publisher state (ADR 0028).
//!
//! A multi-worker publisher claims PENDING rows with `FOR UPDATE SKIP LOCKED` and a lease
//! (`lease_until`: a crashed worker's claim expires and the row is reclaimed), records the
//! last bounded failure code, and dead-letters after bounded retries. DEAD_LETTER rows can
//! be replayed (back to PENDING). Envelopes stay immutable; nothing is deleted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE privacy_relay_outbox
    ADD COLUMN lease_until     TIMESTAMPTZ,
    ADD COLUMN last_error_code VARCHAR(64),
    DROP CONSTRAINT ck_relay_outbox_status,
    ADD CONSTRAINT ck_relay_outbox_status CHECK (status IN ('PENDING', 'PUBLISHED', 'DEAD_LETTER')),
    ADD CONSTRAINT ck_relay_outbox_error_code
        CHECK (last_error_code IS NULL OR last_error_code ~ '^[A-Z][A-Z0-9_]{1,63}$');
CREATE INDEX idx_relay_outbox_dead_letter ON privacy_relay_outbox (updated_at) WHERE status = 'DEAD_LETTER';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_relay_outbox_dead_letter;
UPDATE privacy_relay_outbox SET status = 'PENDING' WHERE status = 'DEAD_LETTER';
ALTER TABLE privacy_relay_outbox
    DROP CONSTRAINT ck_relay_outbox_error_code,
    DROP CONSTRAINT ck_relay_outbox_status,
    ADD CONSTRAINT ck_relay_outbox_status CHECK (status IN ('PENDING', 'PUBLISHED')),
    DROP COLUMN last_error_code,
    DROP COLUMN lease_until;
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

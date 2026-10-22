//! Part V §38 / §49 / §90 / Appendix K.
//!
//! * `messaging_conversations.retention_seconds` — a conversation-specific retention that
//!   is only ever STRICTER than its context's policy and only ever tightened (disappearing
//!   messages: the server expires the ciphertext / sealed text, clients delete locally).
//! * `messaging_conversations.expiry_grace_seconds` — a transactional conversation past
//!   `expires_at` refuses sends but stays readable (and extendable) for the grace period
//!   before it becomes EXPIRED.
//! * `messaging_message_pins.expires_at` — optional pin expiry.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_conversations
    ADD COLUMN retention_seconds BIGINT,
    ADD COLUMN expiry_grace_seconds BIGINT,
    ADD CONSTRAINT ck_messaging_conversations_retention CHECK (retention_seconds IS NULL OR retention_seconds BETWEEN 60 AND 3153600000),
    ADD CONSTRAINT ck_messaging_conversations_grace CHECK (expiry_grace_seconds IS NULL OR expiry_grace_seconds BETWEEN 0 AND 2592000);
CREATE INDEX ix_messaging_conversations_retention ON messaging_conversations (retention_seconds) WHERE retention_seconds IS NOT NULL;

-- Retention only tightens: once set it can be lowered, never raised or removed.
CREATE FUNCTION messaging_conversation_retention_tightens() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.retention_seconds IS NOT NULL
       AND (NEW.retention_seconds IS NULL OR NEW.retention_seconds > OLD.retention_seconds) THEN
        RAISE EXCEPTION 'messaging: a conversation retention only tightens' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_conversation_retention_tightens BEFORE UPDATE OF retention_seconds ON messaging_conversations
    FOR EACH ROW EXECUTE FUNCTION messaging_conversation_retention_tightens();

ALTER TABLE messaging_message_pins ADD COLUMN expires_at TIMESTAMPTZ;
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_message_pins DROP COLUMN IF EXISTS expires_at;
DROP TRIGGER IF EXISTS trg_messaging_conversation_retention_tightens ON messaging_conversations;
DROP FUNCTION IF EXISTS messaging_conversation_retention_tightens();
DROP INDEX IF EXISTS ix_messaging_conversations_retention;
ALTER TABLE messaging_conversations
    DROP CONSTRAINT IF EXISTS ck_messaging_conversations_grace,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversations_retention,
    DROP COLUMN IF EXISTS expiry_grace_seconds,
    DROP COLUMN IF EXISTS retention_seconds;
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

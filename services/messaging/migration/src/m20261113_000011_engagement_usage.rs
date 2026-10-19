//! Part V I §113 / §129 — engagement metadata and usage metering.
//!
//! * `messaging_messages.reply_to_message_id` / `thread_root_id` — replies and threads
//!   (ids only; for PRIVATE_E2EE the relation is metadata the sender chose to disclose,
//!   the quoted content stays inside the ciphertext).
//! * `messaging_message_reactions` — MANAGED reactions (PRIVATE_E2EE reactions are
//!   encrypted application messages, never server-readable rows).
//! * `messaging_message_pins` — pinned messages (ids only; any profile).
//! * `messaging_usage_records` — append-only usage FACTS of an application environment
//!   (accepted messages, stored attachment bytes, bridged SMS segments), idempotent per
//!   key. There is deliberately no price or cost column: Messaging meters, it never bills
//!   (SMS cost evidence stays with the SMS Gateway).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_messages
    ADD COLUMN reply_to_message_id UUID,
    ADD COLUMN thread_root_id UUID;
CREATE INDEX ix_messaging_messages_thread ON messaging_messages (conversation_id, thread_root_id, sequence)
    WHERE thread_root_id IS NOT NULL;

CREATE TABLE messaging_message_reactions (
    message_id       UUID         NOT NULL,
    conversation_id  UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    reaction         VARCHAR(64)  NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_message_reactions PRIMARY KEY (message_id, principal_id, reaction),
    CONSTRAINT fk_messaging_message_reactions_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_message_reactions_value CHECK (octet_length(reaction) BETWEEN 1 AND 64)
);
CREATE INDEX ix_messaging_message_reactions_message ON messaging_message_reactions (message_id);

CREATE TABLE messaging_message_pins (
    conversation_id       UUID         NOT NULL,
    message_id            UUID         NOT NULL,
    pinned_by_principal_id UUID        NOT NULL,
    pinned_at             TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_message_pins PRIMARY KEY (conversation_id, message_id),
    CONSTRAINT fk_messaging_message_pins_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id)
);

CREATE TABLE messaging_usage_records (
    id                UUID         NOT NULL,
    tenant_id         UUID         NOT NULL,
    application_id    UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    usage_type        VARCHAR(40)  NOT NULL,
    quantity          BIGINT       NOT NULL,
    unit              VARCHAR(16)  NOT NULL,
    subject_id        UUID         NOT NULL,
    security_profile  VARCHAR(16),
    idempotency_key   VARCHAR(128) NOT NULL,
    occurred_at       TIMESTAMPTZ  NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_usage_records PRIMARY KEY (id),
    CONSTRAINT uq_messaging_usage_records_key UNIQUE (environment_id, idempotency_key),
    CONSTRAINT ck_messaging_usage_records_type CHECK (
        (usage_type = 'MESSAGE_ACCEPTED' AND unit = 'MESSAGES') OR
        (usage_type = 'ATTACHMENT_STORED' AND unit = 'BYTES') OR
        (usage_type = 'SMS_BRIDGED' AND unit = 'SEGMENTS')),
    CONSTRAINT ck_messaging_usage_records_quantity CHECK (quantity >= 0),
    CONSTRAINT ck_messaging_usage_records_profile CHECK (security_profile IS NULL OR security_profile IN ('PRIVATE_E2EE', 'MANAGED'))
);
CREATE INDEX ix_messaging_usage_records_scope ON messaging_usage_records (environment_id, occurred_at, id);
CREATE FUNCTION messaging_usage_records_immutable() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'messaging: usage records are append-only facts' USING ERRCODE = 'P0001';
END $$;
CREATE TRIGGER trg_messaging_usage_records_immutable BEFORE UPDATE OR DELETE ON messaging_usage_records
    FOR EACH ROW EXECUTE FUNCTION messaging_usage_records_immutable();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_usage_records;
DROP FUNCTION IF EXISTS messaging_usage_records_immutable();
DROP TABLE IF EXISTS messaging_message_pins;
DROP TABLE IF EXISTS messaging_message_reactions;
DROP INDEX IF EXISTS ix_messaging_messages_thread;
ALTER TABLE messaging_messages DROP COLUMN IF EXISTS thread_root_id, DROP COLUMN IF EXISTS reply_to_message_id;
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

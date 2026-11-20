//! Chat controls (additive; its own `messaging_chat_controls_seaql_migrations` history).
//!
//! * `messaging_conversation_inbox_preferences` (F1): a member's own archive and pin of a
//!   conversation in its chat list. Per principal, never shown to anyone else; at most
//!   three pinned conversations per principal and environment (enforced by the service).
//! * `messaging_policy_invalidations` (B2): one row per principal whose personal
//!   relationship policy changed (block, contact, privacy, account status — the
//!   Communications events Messaging consumes). Its `seq` only grows; a cached "allowed"
//!   send decision is used only while neither principal's `seq` moved since the check,
//!   so every Messaging instance drops it as soon as the event is applied.
//! * Disappearing messages (B4): a conversation's retention can now be turned off or
//!   lengthened by its admins (the "only tightens" trigger is dropped), and each message
//!   records the retention in force when it was sent (`messaging_messages.retention_seconds`,
//!   stamped by a BEFORE INSERT trigger, so the previous image's inserts are stamped too):
//!   a change applies to messages sent after it. Messages already in a conversation with
//!   a retention are stamped with it here.
//!
//! Rollback: the previous image never reads these tables or the new column, ignores the
//! missing trigger (its own code still refuses a looser retention) and never reads this
//! history table, so it keeps booting on this schema.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_conversation_inbox_preferences (
    conversation_id   UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    archived_at       TIMESTAMPTZ,
    pinned_at         TIMESTAMPTZ,
    updated_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_conversation_inbox_preferences PRIMARY KEY (conversation_id, principal_id),
    CONSTRAINT fk_messaging_conversation_inbox_preferences_conversation FOREIGN KEY (conversation_id)
        REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_conversation_inbox_preferences_exclusive CHECK (archived_at IS NULL OR pinned_at IS NULL)
);
CREATE INDEX ix_messaging_conversation_inbox_preferences_pinned
    ON messaging_conversation_inbox_preferences (environment_id, principal_id) WHERE pinned_at IS NOT NULL;

CREATE SEQUENCE messaging_policy_invalidation_seq;
CREATE TABLE messaging_policy_invalidations (
    tenant_id         UUID         NOT NULL,
    application_id    UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    seq               BIGINT       NOT NULL DEFAULT nextval('messaging_policy_invalidation_seq'),
    updated_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_messaging_policy_invalidations PRIMARY KEY (tenant_id, application_id, environment_id, principal_id)
);

DROP TRIGGER IF EXISTS trg_messaging_conversation_retention_tightens ON messaging_conversations;

ALTER TABLE messaging_messages
    ADD COLUMN retention_seconds BIGINT,
    ADD CONSTRAINT ck_messaging_messages_retention CHECK (retention_seconds IS NULL OR retention_seconds BETWEEN 60 AND 3153600000);
CREATE FUNCTION messaging_messages_stamp_retention() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.retention_seconds IS NULL THEN
        SELECT c.retention_seconds INTO NEW.retention_seconds FROM messaging_conversations c WHERE c.id = NEW.conversation_id;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_messages_stamp_retention BEFORE INSERT ON messaging_messages
    FOR EACH ROW EXECUTE FUNCTION messaging_messages_stamp_retention();
UPDATE messaging_messages m SET retention_seconds = c.retention_seconds
  FROM messaging_conversations c
 WHERE c.id = m.conversation_id AND c.retention_seconds IS NOT NULL AND m.lifecycle = 'ACTIVE';
CREATE INDEX ix_messaging_messages_disappearing ON messaging_messages (created_at)
    WHERE retention_seconds IS NOT NULL AND lifecycle = 'ACTIVE';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS ix_messaging_messages_disappearing;
DROP TRIGGER IF EXISTS trg_messaging_messages_stamp_retention ON messaging_messages;
DROP FUNCTION IF EXISTS messaging_messages_stamp_retention();
ALTER TABLE messaging_messages
    DROP CONSTRAINT IF EXISTS ck_messaging_messages_retention,
    DROP COLUMN IF EXISTS retention_seconds;
DROP TRIGGER IF EXISTS trg_messaging_conversation_retention_tightens ON messaging_conversations;
CREATE TRIGGER trg_messaging_conversation_retention_tightens BEFORE UPDATE OF retention_seconds ON messaging_conversations
    FOR EACH ROW EXECUTE FUNCTION messaging_conversation_retention_tightens();
DROP TABLE IF EXISTS messaging_policy_invalidations;
DROP SEQUENCE IF EXISTS messaging_policy_invalidation_seq;
DROP TABLE IF EXISTS messaging_conversation_inbox_preferences;
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

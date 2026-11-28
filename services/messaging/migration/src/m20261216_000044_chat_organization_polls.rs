//! Chat organization, MANAGED polls and group descriptions (additive; its own
//! `messaging_chat_organization_seaql_migrations` history).
//!
//! * `messaging_conversation_inbox_preferences.favourited_at`: a member's own "Favourites"
//!   (WhatsApp). Per principal, never visible to anyone else; independent of archive / pin.
//! * `messaging_chat_lists` / `messaging_chat_list_items`: a member's own named chat lists
//!   ("Family", "Work"). Per principal; a conversation may be in several lists; an item is
//!   only a reference (removing it never touches the conversation or its membership).
//! * Polls in MANAGED conversations: a poll is a MANAGED message with the presentation
//!   `POLL` (its sealed text is the question). `messaging_polls` holds the options (sealed
//!   like message text, bound to the message) and the close state;
//!   `messaging_poll_votes` holds each member's choices (ids and option indexes only).
//!   When the message's content is purged its poll goes with it (trigger).
//! * `messaging_conversations.description`: a MANAGED group's description (at most 512
//!   characters; a PRIVATE_E2EE conversation's metadata stays inside its MLS group).
//!
//! Rollback: the previous image never reads the new tables or columns and never reads this
//! history table. It renders a poll message as its question (presentation `POLL` is shown
//! as-is), and its preference upserts leave `favourited_at` untouched.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_conversation_inbox_preferences ADD COLUMN favourited_at TIMESTAMPTZ;
CREATE INDEX ix_messaging_conversation_inbox_preferences_favourites
    ON messaging_conversation_inbox_preferences (environment_id, principal_id) WHERE favourited_at IS NOT NULL;

CREATE TABLE messaging_chat_lists (
    id                UUID          NOT NULL,
    environment_id    UUID          NOT NULL,
    principal_id      UUID          NOT NULL,
    name              VARCHAR(40)   NOT NULL,
    position          INTEGER       NOT NULL,
    created_at        TIMESTAMPTZ   NOT NULL,
    updated_at        TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_chat_lists PRIMARY KEY (id),
    CONSTRAINT ck_messaging_chat_lists_name CHECK (char_length(btrim(name)) BETWEEN 1 AND 40),
    CONSTRAINT ck_messaging_chat_lists_position CHECK (position >= 0)
);
CREATE UNIQUE INDEX uq_messaging_chat_lists_name
    ON messaging_chat_lists (environment_id, principal_id, lower(name));
CREATE INDEX ix_messaging_chat_lists_owner
    ON messaging_chat_lists (environment_id, principal_id, position);

CREATE TABLE messaging_chat_list_items (
    list_id           UUID          NOT NULL,
    conversation_id   UUID          NOT NULL,
    added_at          TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_chat_list_items PRIMARY KEY (list_id, conversation_id),
    CONSTRAINT fk_messaging_chat_list_items_list FOREIGN KEY (list_id)
        REFERENCES messaging_chat_lists (id) ON DELETE CASCADE,
    CONSTRAINT fk_messaging_chat_list_items_conversation FOREIGN KEY (conversation_id)
        REFERENCES messaging_conversations (id)
);
CREATE INDEX ix_messaging_chat_list_items_conversation ON messaging_chat_list_items (conversation_id);

ALTER TABLE messaging_messages DROP CONSTRAINT ck_messaging_messages_presentation;
ALTER TABLE messaging_messages ADD CONSTRAINT ck_messaging_messages_presentation CHECK (managed_presentation IS NULL
    OR (security_profile = 'MANAGED' AND managed_presentation IN ('STANDARD', 'ANNOUNCEMENT', 'POLL')));

CREATE TABLE messaging_polls (
    message_id              UUID          NOT NULL,
    conversation_id         UUID          NOT NULL,
    allows_multiple         BOOLEAN       NOT NULL,
    option_count            SMALLINT      NOT NULL,
    options_sealed          BYTEA         NOT NULL,
    options_nonce           BYTEA         NOT NULL,
    options_key_id          VARCHAR(64)   NOT NULL,
    closed_at               TIMESTAMPTZ,
    closed_by_principal_id  UUID,
    created_at              TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_polls PRIMARY KEY (message_id),
    CONSTRAINT fk_messaging_polls_message FOREIGN KEY (message_id)
        REFERENCES messaging_messages (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_polls_options CHECK (option_count BETWEEN 2 AND 12),
    CONSTRAINT ck_messaging_polls_closed CHECK ((closed_at IS NULL) = (closed_by_principal_id IS NULL))
);
CREATE INDEX ix_messaging_polls_conversation ON messaging_polls (conversation_id);

CREATE TABLE messaging_poll_votes (
    message_id        UUID          NOT NULL,
    principal_id      UUID          NOT NULL,
    option_index      SMALLINT      NOT NULL,
    voted_at          TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_poll_votes PRIMARY KEY (message_id, principal_id, option_index),
    CONSTRAINT fk_messaging_poll_votes_poll FOREIGN KEY (message_id)
        REFERENCES messaging_polls (message_id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_poll_votes_option CHECK (option_index BETWEEN 0 AND 11)
);

CREATE FUNCTION messaging_polls_purge() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM messaging_polls WHERE message_id = NEW.id;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_polls_purge AFTER UPDATE OF managed_sealed ON messaging_messages
    FOR EACH ROW WHEN (OLD.managed_sealed IS NOT NULL AND NEW.managed_sealed IS NULL)
    EXECUTE FUNCTION messaging_polls_purge();

ALTER TABLE messaging_conversations
    ADD COLUMN description VARCHAR(512),
    ADD CONSTRAINT ck_messaging_conversations_description CHECK (description IS NULL
        OR (security_profile = 'MANAGED' AND char_length(description) BETWEEN 1 AND 512));
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_conversations DROP CONSTRAINT IF EXISTS ck_messaging_conversations_description;
ALTER TABLE messaging_conversations DROP COLUMN IF EXISTS description;
DROP TRIGGER IF EXISTS trg_messaging_polls_purge ON messaging_messages;
DROP FUNCTION IF EXISTS messaging_polls_purge();
DROP TABLE IF EXISTS messaging_poll_votes;
DROP TABLE IF EXISTS messaging_polls;
UPDATE messaging_messages SET managed_presentation = 'STANDARD' WHERE managed_presentation = 'POLL';
ALTER TABLE messaging_messages DROP CONSTRAINT IF EXISTS ck_messaging_messages_presentation;
ALTER TABLE messaging_messages ADD CONSTRAINT ck_messaging_messages_presentation CHECK (managed_presentation IS NULL
    OR (security_profile = 'MANAGED' AND managed_presentation IN ('STANDARD', 'ANNOUNCEMENT')));
DROP TABLE IF EXISTS messaging_chat_list_items;
DROP TABLE IF EXISTS messaging_chat_lists;
DROP INDEX IF EXISTS ix_messaging_conversation_inbox_preferences_favourites;
ALTER TABLE messaging_conversation_inbox_preferences DROP COLUMN IF EXISTS favourited_at;
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

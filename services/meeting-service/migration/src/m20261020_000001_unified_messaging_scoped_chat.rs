//! Part V remediation — breakout and private meeting chat move to Unified Messaging.
//!
//! Extends the main-room migration (`m20261019_000001_unified_messaging_integration`)
//! to the two remaining chat scopes, under the SAME migration state machine:
//!
//! | table / change | purpose |
//! |---|---|
//! | `conference_chat_scope_links` | breakout room / participant pair → its Messaging MEETING conversation (ids only) |
//! | `conference_chat_scope_member_sync` | the membership version last applied per (conversation, participant) |
//! | `conference_chat_message_links.conversation_id` | which conversation a linked message lives in (NULL: the main room) |
//! | `communication_principal_links` | also the environment's `SYSTEM` → `BOT_OR_SERVICE` principal (owner of private chats) |
//! | `trg_conference_messages_cutover` | once Messaging is primary, NO legacy chat row of ANY scope is accepted |
//!
//! Private chats are owned by the environment's non-human system principal so that only
//! the two parties are members (no moderator, no meeting owner can read them through
//! Messaging), and either party can lose access (Messaging never removes an OWNER).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE communication_principal_links DROP CONSTRAINT communication_principal_links_subject_kind_check;
ALTER TABLE communication_principal_links DROP CONSTRAINT communication_principal_links_principal_kind_check;
ALTER TABLE communication_principal_links DROP CONSTRAINT ck_communication_principal_kind;
ALTER TABLE communication_principal_links ADD CONSTRAINT communication_principal_links_subject_kind_check
    CHECK (subject_kind IN ('USER','PARTICIPANT','SYSTEM'));
ALTER TABLE communication_principal_links ADD CONSTRAINT communication_principal_links_principal_kind_check
    CHECK (principal_kind IN ('NATIVE_CF_USER','TEMPORARY_EXTERNAL_PARTICIPANT','BOT_OR_SERVICE'));
ALTER TABLE communication_principal_links ADD CONSTRAINT ck_communication_principal_kind CHECK (
    (subject_kind = 'USER' AND principal_kind = 'NATIVE_CF_USER') OR
    (subject_kind = 'PARTICIPANT' AND principal_kind = 'TEMPORARY_EXTERNAL_PARTICIPANT') OR
    (subject_kind = 'SYSTEM' AND principal_kind = 'BOT_OR_SERVICE'));

CREATE TABLE conference_chat_scope_links (
    conversation_id          UUID         NOT NULL PRIMARY KEY,
    conference_id            UUID         NOT NULL REFERENCES conference_sessions(id) ON DELETE CASCADE,
    scope_kind               VARCHAR(16)  NOT NULL CHECK (scope_kind IN ('BREAKOUT','PRIVATE')),
    breakout_room_id         UUID,
    participant_low          UUID,
    participant_high         UUID,
    environment_id           UUID         NOT NULL,
    owner_principal_id       UUID         NOT NULL,
    lifecycle_synced_state   VARCHAR(16),
    created_at               TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT ck_conference_chat_scope_shape CHECK (
        (scope_kind = 'BREAKOUT' AND breakout_room_id IS NOT NULL AND participant_low IS NULL AND participant_high IS NULL) OR
        (scope_kind = 'PRIVATE' AND breakout_room_id IS NULL AND participant_low IS NOT NULL AND participant_high IS NOT NULL
             AND participant_low < participant_high))
);
CREATE UNIQUE INDEX uq_conference_chat_scope_breakout ON conference_chat_scope_links (conference_id, breakout_room_id)
    WHERE scope_kind = 'BREAKOUT';
CREATE UNIQUE INDEX uq_conference_chat_scope_private ON conference_chat_scope_links (conference_id, participant_low, participant_high)
    WHERE scope_kind = 'PRIVATE';

CREATE TABLE conference_chat_scope_member_sync (
    conversation_id  UUID         NOT NULL REFERENCES conference_chat_scope_links(conversation_id) ON DELETE CASCADE,
    participant_id   UUID         NOT NULL REFERENCES conference_participants(id) ON DELETE CASCADE,
    principal_id     UUID         NOT NULL,
    synced_version   BIGINT       NOT NULL,
    synced_state     VARCHAR(16)  NOT NULL CHECK (synced_state IN ('ACTIVE','REMOVED','BANNED','LEFT')),
    synced_role      VARCHAR(16),
    -- A host who entered a breakout room they are not assigned to (breakout.enter_any).
    visitor          BOOLEAN      NOT NULL DEFAULT false,
    synced_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    PRIMARY KEY (conversation_id, participant_id)
);

ALTER TABLE conference_chat_message_links ADD COLUMN conversation_id UUID;
CREATE INDEX idx_conference_chat_message_links_conversation ON conference_chat_message_links (conversation_id, messaging_sequence)
    WHERE conversation_id IS NOT NULL;

-- After cutover the legacy store accepts no NEW authoritative chat row of ANY scope.
CREATE OR REPLACE FUNCTION conference_messages_cutover_guard() RETURNS trigger AS $$
DECLARE s VARCHAR(24);
BEGIN
    IF NEW.origin = 'MEETING' THEN
        SELECT state INTO s FROM conference_chat_migration WHERE id = 1;
        IF s IN ('MESSAGING_PRIMARY','LEGACY_RETIRED') THEN
            RAISE EXCEPTION 'CHAT_MIGRATED: Messaging is the authority for meeting chat (state %)', s
                USING ERRCODE = 'P0001';
        END IF;
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;
"#;

const DOWN: &str = r#"
CREATE OR REPLACE FUNCTION conference_messages_cutover_guard() RETURNS trigger AS $$
DECLARE s VARCHAR(24);
BEGIN
    IF NEW.breakout_room_id IS NULL AND NEW.recipient_participant_id IS NULL AND NEW.origin = 'MEETING' THEN
        SELECT state INTO s FROM conference_chat_migration WHERE id = 1;
        IF s IN ('MESSAGING_PRIMARY','LEGACY_RETIRED') THEN
            RAISE EXCEPTION 'CHAT_MIGRATED: Messaging is the authority for main-room chat (state %)', s
                USING ERRCODE = 'P0001';
        END IF;
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP INDEX IF EXISTS idx_conference_chat_message_links_conversation;
DELETE FROM conference_chat_message_links WHERE conversation_id IS NOT NULL;
ALTER TABLE conference_chat_message_links DROP COLUMN IF EXISTS conversation_id;
DROP TABLE IF EXISTS conference_chat_scope_member_sync;
DROP TABLE IF EXISTS conference_chat_scope_links;
DELETE FROM communication_principal_links WHERE subject_kind = 'SYSTEM';
ALTER TABLE communication_principal_links DROP CONSTRAINT ck_communication_principal_kind;
ALTER TABLE communication_principal_links DROP CONSTRAINT communication_principal_links_principal_kind_check;
ALTER TABLE communication_principal_links DROP CONSTRAINT communication_principal_links_subject_kind_check;
ALTER TABLE communication_principal_links ADD CONSTRAINT communication_principal_links_subject_kind_check
    CHECK (subject_kind IN ('USER','PARTICIPANT'));
ALTER TABLE communication_principal_links ADD CONSTRAINT communication_principal_links_principal_kind_check
    CHECK (principal_kind IN ('NATIVE_CF_USER','TEMPORARY_EXTERNAL_PARTICIPANT'));
ALTER TABLE communication_principal_links ADD CONSTRAINT ck_communication_principal_kind CHECK (
    (subject_kind = 'USER' AND principal_kind = 'NATIVE_CF_USER') OR
    (subject_kind = 'PARTICIPANT' AND principal_kind = 'TEMPORARY_EXTERNAL_PARTICIPANT'));
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

//! Part V H §§97–101 / §104 — Meeting chat migration to Unified Messaging.
//!
//! Meeting keeps meeting admission and policy; after cutover Messaging is the ONLY
//! authority for main-room chat messages and history. These tables hold Meeting's own
//! integration state, never a second copy of Messaging's data:
//!
//! | table | purpose |
//! |---|---|
//! | `conference_chat_migration` | the single migration state (LEGACY_PRIMARY → SHADOW_VALIDATION → MESSAGING_PRIMARY → LEGACY_RETIRED) |
//! | `conference_chat_migration_transitions` | append-only, audited transitions (actor, approver, parity evidence) |
//! | `communication_principal_links` | CF Meet account / guest participant → Platform Access principal (ids only) |
//! | `conference_chat_links` | conference → its Messaging MEETING conversation (ids only) |
//! | `conference_chat_member_sync` | the participant version last applied to the conversation membership |
//! | `conference_chat_message_links` | legacy message id ↔ canonical Messaging message id (ids only, no body) |
//! | `conference_chat_parity_runs` / `_mismatches` | shadow-validation evidence |
//! | `communication_session_revocations` | durable "log out of messaging" jobs (retried until done) |
//!
//! The database itself refuses NEW authoritative main-room chat writes once Messaging is
//! primary (`trg_conference_messages_cutover`, `trg_chat_messages_cutover`) and refuses
//! to leave LEGACY_RETIRED. Breakout and private chat stay in `conference_messages`
//! (documented PARTIAL scope of this increment).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE conference_chat_migration (
    id          SMALLINT     NOT NULL PRIMARY KEY CHECK (id = 1),
    state       VARCHAR(24)  NOT NULL CHECK (state IN ('LEGACY_PRIMARY','SHADOW_VALIDATION','MESSAGING_PRIMARY','LEGACY_RETIRED')),
    version     BIGINT       NOT NULL DEFAULT 1,
    changed_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    changed_by  VARCHAR(160) NOT NULL DEFAULT 'migration',
    reason      VARCHAR(500)
);
INSERT INTO conference_chat_migration (id, state) VALUES (1, 'LEGACY_PRIMARY');

CREATE OR REPLACE FUNCTION conference_chat_migration_guard() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'the chat migration state cannot be deleted';
    END IF;
    IF OLD.state = 'LEGACY_RETIRED' AND NEW.state <> 'LEGACY_RETIRED' THEN
        RAISE EXCEPTION 'LEGACY_RETIRED is final: the legacy chat store cannot become authoritative again';
    END IF;
    IF NEW.version <= OLD.version THEN
        RAISE EXCEPTION 'the chat migration version only moves forward';
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;
CREATE TRIGGER trg_conference_chat_migration_guard BEFORE UPDATE OR DELETE ON conference_chat_migration
    FOR EACH ROW EXECUTE FUNCTION conference_chat_migration_guard();

CREATE TABLE conference_chat_migration_transitions (
    id             BIGSERIAL    PRIMARY KEY,
    from_state     VARCHAR(24)  NOT NULL,
    to_state       VARCHAR(24)  NOT NULL,
    version        BIGINT       NOT NULL UNIQUE,
    actor          VARCHAR(160) NOT NULL,
    approved_by    VARCHAR(160),
    reason         VARCHAR(500) NOT NULL,
    parity_run_id  UUID,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT now()
);
CREATE OR REPLACE FUNCTION conference_chat_transitions_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'chat migration transitions are append-only';
END $$ LANGUAGE plpgsql;
CREATE TRIGGER trg_conference_chat_transitions_append_only BEFORE UPDATE OR DELETE ON conference_chat_migration_transitions
    FOR EACH ROW EXECUTE FUNCTION conference_chat_transitions_append_only();

CREATE TABLE communication_principal_links (
    subject_kind    VARCHAR(16)  NOT NULL CHECK (subject_kind IN ('USER','PARTICIPANT')),
    subject_id      UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    principal_id    UUID         NOT NULL,
    principal_kind  VARCHAR(40)  NOT NULL CHECK (principal_kind IN ('NATIVE_CF_USER','TEMPORARY_EXTERNAL_PARTICIPANT')),
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    PRIMARY KEY (subject_kind, subject_id, environment_id),
    CONSTRAINT uq_communication_principal UNIQUE (environment_id, principal_id),
    CONSTRAINT ck_communication_principal_kind CHECK (
        (subject_kind = 'USER' AND principal_kind = 'NATIVE_CF_USER') OR
        (subject_kind = 'PARTICIPANT' AND principal_kind = 'TEMPORARY_EXTERNAL_PARTICIPANT'))
);

CREATE TABLE conference_chat_links (
    conference_id              UUID         NOT NULL PRIMARY KEY REFERENCES conference_sessions(id) ON DELETE CASCADE,
    conversation_id            UUID         NOT NULL UNIQUE,
    environment_id             UUID         NOT NULL,
    owner_principal_id         UUID         NOT NULL,
    lifecycle_synced_version   BIGINT       NOT NULL DEFAULT 0,
    lifecycle_synced_state     VARCHAR(16),
    backfilled_at              TIMESTAMPTZ,
    created_at                 TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at                 TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE TABLE conference_chat_member_sync (
    participant_id   UUID         NOT NULL PRIMARY KEY REFERENCES conference_participants(id) ON DELETE CASCADE,
    conference_id    UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    synced_version   BIGINT       NOT NULL,
    synced_state     VARCHAR(16)  NOT NULL CHECK (synced_state IN ('ACTIVE','REMOVED','BANNED','LEFT')),
    synced_role      VARCHAR(16),
    synced_at        TIMESTAMPTZ  NOT NULL DEFAULT now()
);
CREATE INDEX idx_conference_chat_member_sync_conference ON conference_chat_member_sync (conference_id, principal_id);
-- Membership sync source versions: taken under the conference link lock AFTER reading
-- the participant state, so a delayed older update is always fenced out by Messaging.
CREATE SEQUENCE conference_chat_sync_version_seq;

CREATE TABLE conference_chat_message_links (
    messaging_message_id  UUID         NOT NULL PRIMARY KEY,
    conference_id         UUID         NOT NULL,
    legacy_message_id     UUID         UNIQUE,
    messaging_sequence    BIGINT       NOT NULL,
    origin                VARCHAR(24)  NOT NULL CHECK (origin IN ('LEGACY_IMPORT','MESSAGING_NATIVE','ROLLBACK_COPY')),
    -- A legacy deletion that has been applied to the canonical message (shadow mode).
    deletion_mirrored_at  TIMESTAMPTZ,
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT now()
);
CREATE INDEX idx_conference_chat_message_links_seq ON conference_chat_message_links (conference_id, messaging_sequence);

CREATE TABLE conference_chat_parity_runs (
    id                   UUID         NOT NULL PRIMARY KEY,
    started_at           TIMESTAMPTZ  NOT NULL,
    finished_at          TIMESTAMPTZ,
    conferences_checked  INTEGER      NOT NULL DEFAULT 0,
    messages_checked     INTEGER      NOT NULL DEFAULT 0,
    mismatches           INTEGER      NOT NULL DEFAULT 0,
    status               VARCHAR(16)  NOT NULL CHECK (status IN ('RUNNING','PASSED','FAILED','ERROR')),
    detail               JSONB        NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE conference_chat_mismatches (
    id                    BIGSERIAL    PRIMARY KEY,
    run_id                UUID         NOT NULL REFERENCES conference_chat_parity_runs(id) ON DELETE CASCADE,
    conference_id         UUID         NOT NULL,
    legacy_message_id     UUID,
    messaging_message_id  UUID,
    kind                  VARCHAR(32)  NOT NULL CHECK (kind IN ('MISSING_IN_MESSAGING','CONTENT_DIFFERS','DELETION_DIFFERS','UNEXPECTED_IN_MESSAGING','CONVERSATION_MISSING','MESSAGING_UNAVAILABLE')),
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE TABLE communication_session_revocations (
    id               UUID         NOT NULL PRIMARY KEY,
    principal_id     UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    reason           VARCHAR(32)  NOT NULL CHECK (reason IN ('LOGOUT','ACCOUNT_DISABLED','PASSWORD_CHANGED')),
    status           VARCHAR(16)  NOT NULL DEFAULT 'PENDING' CHECK (status IN ('PENDING','DONE')),
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    last_error       VARCHAR(64),
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    done_at          TIMESTAMPTZ
);
CREATE INDEX idx_communication_session_revocations_due ON communication_session_revocations (next_attempt_at) WHERE status = 'PENDING';

-- Rows written back during a rollback are labelled (they came from Messaging).
ALTER TABLE conference_messages ADD COLUMN origin VARCHAR(24) NOT NULL DEFAULT 'MEETING'
    CHECK (origin IN ('MEETING','MESSAGING_ROLLBACK'));

-- After cutover the legacy store accepts no NEW authoritative main-room message.
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
CREATE TRIGGER trg_conference_messages_cutover BEFORE INSERT ON conference_messages
    FOR EACH ROW EXECUTE FUNCTION conference_messages_cutover_guard();

CREATE OR REPLACE FUNCTION chat_messages_cutover_guard() RETURNS trigger AS $$
DECLARE s VARCHAR(24);
BEGIN
    SELECT state INTO s FROM conference_chat_migration WHERE id = 1;
    IF s IN ('MESSAGING_PRIMARY','LEGACY_RETIRED') THEN
        RAISE EXCEPTION 'CHAT_MIGRATED: legacy v1 chat accepts no new messages (state %)', s
            USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;
CREATE TRIGGER trg_chat_messages_cutover BEFORE INSERT ON chat_messages
    FOR EACH ROW EXECUTE FUNCTION chat_messages_cutover_guard();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_chat_messages_cutover ON chat_messages;
DROP FUNCTION IF EXISTS chat_messages_cutover_guard();
DROP TRIGGER IF EXISTS trg_conference_messages_cutover ON conference_messages;
DROP FUNCTION IF EXISTS conference_messages_cutover_guard();
ALTER TABLE conference_messages DROP COLUMN IF EXISTS origin;
DROP TABLE IF EXISTS communication_session_revocations;
DROP TABLE IF EXISTS conference_chat_mismatches;
DROP TABLE IF EXISTS conference_chat_parity_runs;
DROP TABLE IF EXISTS conference_chat_message_links;
DROP TABLE IF EXISTS conference_chat_member_sync;
DROP SEQUENCE IF EXISTS conference_chat_sync_version_seq;
DROP TABLE IF EXISTS conference_chat_links;
DROP TABLE IF EXISTS communication_principal_links;
DROP TABLE IF EXISTS conference_chat_migration_transitions;
DROP FUNCTION IF EXISTS conference_chat_transitions_append_only();
DROP TABLE IF EXISTS conference_chat_migration;
DROP FUNCTION IF EXISTS conference_chat_migration_guard();
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

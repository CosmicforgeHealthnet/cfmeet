//! Membership, the message store, the MLS Delivery Service state and the per-conversation
//! durable event cursor (Part V Workstreams E / F prerequisites of Parts E–G).
//!
//! * `messaging_conversation_members` gains a lifecycle (ACTIVE → REMOVED / LEFT, and back
//!   on re-admission), roles OWNER / ADMIN / MEMBER, a send restriction (mute) and the
//!   history window a member may see (`visible_from_sequence` / `visible_from_event`:
//!   new members receive only what is committed after they joined, AD-07).
//! * `messaging_conversation_sequences.next_event_sequence` — the per-conversation durable
//!   EVENT cursor (every replayable event), beside the message sequence.
//! * `messaging_outbox_events` carries `(conversation_id, conversation_event_sequence)`:
//!   the outbox IS the durable, replayable conversation event log (one log, no second
//!   authoritative stream). Unique per conversation.
//! * `messaging_mls_groups` — the Delivery Service's view of each PRIVATE_E2EE group: the
//!   current epoch number (PUBLIC framing metadata, RFC 9420 §6.3.1 — never an epoch
//!   secret) and whether a removal still awaits its rekeying commit.
//! * `messaging_messages` — the authoritative message store. PRIVATE_E2EE rows carry the
//!   opaque MLS ciphertext, its digest and the server acceptance-record digest only;
//!   MANAGED rows carry server-sealed (envelope-encrypted) text and a search vector. A
//!   CHECK makes any other combination impossible; purged rows hold no ciphertext.
//! * `messaging_mls_welcomes` — Welcome messages addressed to one device each (opaque).
//! * `messaging_ack_cursors` / `messaging_read_cursors` — application
//!   acknowledgements per device and read positions per principal (distinct concepts).
//! * `messaging_message_hidden` — "delete for me" markers (sender-side local deletion
//!   shared across the principal's devices).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_role;
ALTER TABLE messaging_conversation_members
    ADD CONSTRAINT ck_messaging_conversation_members_role CHECK (role IN ('OWNER', 'ADMIN', 'MEMBER')),
    ADD COLUMN state VARCHAR(16) NOT NULL DEFAULT 'ACTIVE',
    ADD COLUMN send_restricted_until TIMESTAMPTZ,
    ADD COLUMN visible_from_sequence BIGINT NOT NULL DEFAULT 1,
    ADD COLUMN visible_from_event BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN removed_at TIMESTAMPTZ,
    ADD COLUMN removed_reason VARCHAR(48),
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1,
    ADD COLUMN updated_at TIMESTAMPTZ;
UPDATE messaging_conversation_members SET updated_at = joined_at;
ALTER TABLE messaging_conversation_members
    ALTER COLUMN updated_at SET NOT NULL,
    ADD CONSTRAINT ck_messaging_conversation_members_state CHECK (state IN ('ACTIVE', 'REMOVED', 'LEFT')),
    ADD CONSTRAINT ck_messaging_conversation_members_removed CHECK ((state = 'ACTIVE') = (removed_at IS NULL)),
    ADD CONSTRAINT ck_messaging_conversation_members_reason CHECK (removed_reason IS NULL
        OR removed_reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    ADD CONSTRAINT ck_messaging_conversation_members_window CHECK (visible_from_sequence >= 1 AND visible_from_event >= 0);
CREATE INDEX ix_messaging_conversation_members_active ON messaging_conversation_members (conversation_id)
    WHERE state = 'ACTIVE';

ALTER TABLE messaging_conversation_sequences
    ADD COLUMN next_event_sequence BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_messaging_conversation_sequences_event CHECK (next_event_sequence >= 1);
CREATE OR REPLACE FUNCTION messaging_sequences_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' OR NEW.next_sequence < OLD.next_sequence
       OR NEW.next_event_sequence < OLD.next_event_sequence THEN
        RAISE EXCEPTION 'messaging: message sequences only advance' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;

ALTER TABLE messaging_outbox_events
    ADD COLUMN conversation_id UUID,
    ADD COLUMN conversation_event_sequence BIGINT,
    ADD CONSTRAINT ck_messaging_outbox_events_cursor CHECK ((conversation_id IS NULL) = (conversation_event_sequence IS NULL)
        AND (conversation_event_sequence IS NULL OR conversation_event_sequence >= 1));
CREATE UNIQUE INDEX uq_messaging_outbox_events_cursor ON messaging_outbox_events (conversation_id, conversation_event_sequence)
    WHERE conversation_id IS NOT NULL;

CREATE TABLE messaging_mls_groups (
    conversation_id   UUID         NOT NULL,
    mls_epoch         BIGINT       NOT NULL DEFAULT 0,
    rekey_required    BOOLEAN      NOT NULL DEFAULT FALSE,
    rekey_reason      VARCHAR(48),
    commits_accepted  BIGINT       NOT NULL DEFAULT 0,
    updated_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_mls_groups PRIMARY KEY (conversation_id),
    CONSTRAINT fk_messaging_mls_groups_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_mls_groups_epoch CHECK (mls_epoch >= 0 AND mls_epoch <= 9007199254740992),
    CONSTRAINT ck_messaging_mls_groups_rekey CHECK (rekey_required = (rekey_reason IS NOT NULL))
);
CREATE FUNCTION messaging_mls_groups_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: MLS group state is never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.mls_epoch <> OLD.mls_epoch AND NEW.mls_epoch <> OLD.mls_epoch + 1 THEN
        RAISE EXCEPTION 'messaging: an MLS epoch advances by exactly one commit' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_mls_groups_guard BEFORE UPDATE OR DELETE ON messaging_mls_groups
    FOR EACH ROW EXECUTE FUNCTION messaging_mls_groups_guard();
INSERT INTO messaging_mls_groups (conversation_id, updated_at)
    SELECT id, created_at FROM messaging_conversations WHERE security_profile = 'PRIVATE_E2EE';

CREATE TABLE messaging_messages (
    id                    UUID         NOT NULL,
    conversation_id       UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    sequence              BIGINT       NOT NULL,
    message_kind          VARCHAR(16)  NOT NULL,
    security_profile      VARCHAR(16)  NOT NULL,
    sender_principal_id   UUID         NOT NULL,
    sender_device_id      UUID,
    client_message_id     UUID         NOT NULL,
    target_message_id     UUID,
    mls_epoch             BIGINT,
    mls_ciphertext        BYTEA,
    ciphertext_sha256     CHAR(64),
    acceptance_digest     CHAR(64),
    managed_sealed        BYTEA,
    managed_seal_nonce    BYTEA,
    managed_seal_key_id   VARCHAR(32),
    managed_search        TSVECTOR,
    attachment_count      INTEGER      NOT NULL DEFAULT 0,
    lifecycle             VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    lifecycle_reason      VARCHAR(48),
    lifecycle_changed_at  TIMESTAMPTZ,
    edit_count            INTEGER      NOT NULL DEFAULT 0,
    edited_at             TIMESTAMPTZ,
    purge_after           TIMESTAMPTZ,
    purged_at             TIMESTAMPTZ,
    created_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_messages PRIMARY KEY (id),
    CONSTRAINT fk_messaging_messages_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT uq_messaging_messages_sequence UNIQUE (conversation_id, sequence),
    CONSTRAINT uq_messaging_messages_client UNIQUE (conversation_id, sender_principal_id, client_message_id),
    CONSTRAINT ck_messaging_messages_sequence CHECK (sequence >= 1),
    CONSTRAINT ck_messaging_messages_kind CHECK (message_kind IN ('APPLICATION', 'MLS_COMMIT', 'EDIT')),
    CONSTRAINT ck_messaging_messages_lifecycle CHECK (lifecycle IN ('ACTIVE', 'DELETED', 'MODERATED', 'EXPIRED', 'PURGED')),
    CONSTRAINT ck_messaging_messages_reason CHECK (lifecycle_reason IS NULL OR lifecycle_reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_messages_digests CHECK ((ciphertext_sha256 IS NULL OR ciphertext_sha256 ~ '^[0-9a-f]{64}$')
        AND (acceptance_digest IS NULL OR acceptance_digest ~ '^[0-9a-f]{64}$')),
    -- The persisted profile decides what a row may hold: never both, never the wrong one.
    CONSTRAINT ck_messaging_messages_profile CHECK (
        (security_profile = 'PRIVATE_E2EE' AND sender_device_id IS NOT NULL AND mls_epoch IS NOT NULL
            AND ciphertext_sha256 IS NOT NULL AND managed_sealed IS NULL AND managed_seal_nonce IS NULL
            AND managed_seal_key_id IS NULL AND managed_search IS NULL)
        OR (security_profile = 'MANAGED' AND message_kind <> 'MLS_COMMIT' AND mls_epoch IS NULL
            AND mls_ciphertext IS NULL AND ciphertext_sha256 IS NULL AND acceptance_digest IS NULL)),
    CONSTRAINT ck_messaging_messages_edit CHECK ((message_kind = 'EDIT') = (target_message_id IS NOT NULL)),
    CONSTRAINT ck_messaging_messages_size CHECK (mls_ciphertext IS NULL OR octet_length(mls_ciphertext) BETWEEN 1 AND 1048576),
    -- Purged rows hold nothing that was content.
    CONSTRAINT ck_messaging_messages_purged CHECK (lifecycle <> 'PURGED'
        OR (mls_ciphertext IS NULL AND managed_sealed IS NULL AND managed_search IS NULL AND purged_at IS NOT NULL)),
    CONSTRAINT ck_messaging_messages_live CHECK (lifecycle = 'PURGED' OR purged_at IS NULL)
);
CREATE INDEX ix_messaging_messages_purge ON messaging_messages (purge_after)
    WHERE purge_after IS NOT NULL AND lifecycle <> 'PURGED';
CREATE INDEX ix_messaging_messages_retention ON messaging_messages (environment_id, created_at)
    WHERE lifecycle = 'ACTIVE';
CREATE INDEX ix_messaging_messages_sender ON messaging_messages (environment_id, sender_principal_id, created_at);
CREATE INDEX ix_messaging_messages_search ON messaging_messages USING GIN (managed_search)
    WHERE managed_search IS NOT NULL;
-- Identity, sequence and profile never change; a purge is terminal.
CREATE FUNCTION messaging_messages_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: messages are purged, never deleted physically' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.conversation_id <> OLD.conversation_id OR NEW.sequence <> OLD.sequence
       OR NEW.security_profile <> OLD.security_profile OR NEW.sender_principal_id <> OLD.sender_principal_id
       OR NEW.sender_device_id IS DISTINCT FROM OLD.sender_device_id OR NEW.message_kind <> OLD.message_kind
       OR NEW.client_message_id <> OLD.client_message_id OR NEW.created_at <> OLD.created_at
       OR NEW.tenant_id <> OLD.tenant_id OR NEW.environment_id <> OLD.environment_id THEN
        RAISE EXCEPTION 'messaging: message identity is immutable' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.security_profile = 'PRIVATE_E2EE' AND NEW.mls_ciphertext IS NOT NULL
       AND NEW.mls_ciphertext IS DISTINCT FROM OLD.mls_ciphertext THEN
        RAISE EXCEPTION 'messaging: E2EE ciphertext is never rewritten' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.lifecycle = 'PURGED' AND (NEW.lifecycle <> 'PURGED' OR NEW.mls_ciphertext IS NOT NULL
       OR NEW.managed_sealed IS NOT NULL) THEN
        RAISE EXCEPTION 'messaging: a purge is irreversible' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_messages_guard BEFORE UPDATE OR DELETE ON messaging_messages
    FOR EACH ROW EXECUTE FUNCTION messaging_messages_guard();

CREATE TABLE messaging_mls_welcomes (
    id                UUID         NOT NULL,
    conversation_id   UUID         NOT NULL,
    device_id         UUID         NOT NULL,
    commit_message_id UUID         NOT NULL,
    mls_epoch         BIGINT       NOT NULL,
    welcome           BYTEA        NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL,
    consumed_at       TIMESTAMPTZ,
    CONSTRAINT pk_messaging_mls_welcomes PRIMARY KEY (id),
    CONSTRAINT fk_messaging_mls_welcomes_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT fk_messaging_mls_welcomes_device FOREIGN KEY (device_id) REFERENCES messaging_devices (id),
    CONSTRAINT fk_messaging_mls_welcomes_commit FOREIGN KEY (commit_message_id) REFERENCES messaging_messages (id),
    CONSTRAINT uq_messaging_mls_welcomes_device UNIQUE (commit_message_id, device_id),
    CONSTRAINT ck_messaging_mls_welcomes_size CHECK (octet_length(welcome) BETWEEN 1 AND 1048576)
);
CREATE INDEX ix_messaging_mls_welcomes_pending ON messaging_mls_welcomes (device_id, created_at) WHERE consumed_at IS NULL;

-- Devices whose leaves a commit must still remove (member removed / device revoked).
-- While any remain, the group accepts commits only (no application message).
CREATE TABLE messaging_mls_pending_removals (
    conversation_id   UUID         NOT NULL,
    device_id         UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    reason            VARCHAR(48)  NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_mls_pending_removals PRIMARY KEY (conversation_id, device_id),
    CONSTRAINT fk_messaging_mls_pending_removals_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_mls_pending_removals_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,47}$')
);

CREATE TABLE messaging_ack_cursors (
    conversation_id   UUID         NOT NULL,
    device_id         UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    acked_event_sequence BIGINT    NOT NULL,
    updated_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_ack_cursors PRIMARY KEY (conversation_id, device_id),
    CONSTRAINT fk_messaging_ack_cursors_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_ack_cursors_value CHECK (acked_event_sequence >= 0)
);

CREATE TABLE messaging_read_cursors (
    conversation_id   UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    read_sequence     BIGINT       NOT NULL,
    updated_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_read_cursors PRIMARY KEY (conversation_id, principal_id),
    CONSTRAINT fk_messaging_read_cursors_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_read_cursors_value CHECK (read_sequence >= 0)
);

CREATE TABLE messaging_message_hidden (
    message_id        UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    hidden_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_message_hidden PRIMARY KEY (message_id, principal_id),
    CONSTRAINT fk_messaging_message_hidden_message FOREIGN KEY (message_id) REFERENCES messaging_messages (id)
);

-- Messaging blocks (§87): direct-message blocks between principals of one environment.
CREATE TABLE messaging_blocks (
    environment_id        UUID         NOT NULL,
    blocker_principal_id  UUID         NOT NULL,
    blocked_principal_id  UUID         NOT NULL,
    created_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_blocks PRIMARY KEY (environment_id, blocker_principal_id, blocked_principal_id),
    CONSTRAINT ck_messaging_blocks_distinct CHECK (blocker_principal_id <> blocked_principal_id)
);
CREATE INDEX ix_messaging_blocks_blocked ON messaging_blocks (environment_id, blocked_principal_id);

-- Temporary sending restrictions (§88): automatic (abuse thresholds) or by a moderator.
CREATE TABLE messaging_principal_restrictions (
    id                    UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    principal_id          UUID         NOT NULL,
    reason                VARCHAR(48)  NOT NULL,
    source                VARCHAR(16)  NOT NULL,
    starts_at             TIMESTAMPTZ  NOT NULL,
    ends_at               TIMESTAMPTZ  NOT NULL,
    created_by_credential_id UUID,
    lifted_at             TIMESTAMPTZ,
    CONSTRAINT pk_messaging_principal_restrictions PRIMARY KEY (id),
    CONSTRAINT ck_messaging_principal_restrictions_source CHECK (source IN ('AUTOMATIC', 'MODERATOR')),
    CONSTRAINT ck_messaging_principal_restrictions_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_principal_restrictions_window CHECK (ends_at > starts_at)
);
CREATE INDEX ix_messaging_principal_restrictions_active ON messaging_principal_restrictions (environment_id, principal_id, ends_at)
    WHERE lifted_at IS NULL;

-- Abuse signals (metadata only: never content of an E2EE conversation).
CREATE TABLE messaging_abuse_signals (
    id                UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    signal            VARCHAR(48)  NOT NULL,
    occurred_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_abuse_signals PRIMARY KEY (id),
    CONSTRAINT ck_messaging_abuse_signals_signal CHECK (signal ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE INDEX ix_messaging_abuse_signals_principal ON messaging_abuse_signals (environment_id, principal_id, occurred_at);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_abuse_signals;
DROP TABLE IF EXISTS messaging_principal_restrictions;
DROP TABLE IF EXISTS messaging_blocks;
DROP TABLE IF EXISTS messaging_message_hidden;
DROP TABLE IF EXISTS messaging_read_cursors;
DROP TABLE IF EXISTS messaging_ack_cursors;
DROP TABLE IF EXISTS messaging_mls_pending_removals;
DROP TABLE IF EXISTS messaging_mls_welcomes;
DROP TABLE IF EXISTS messaging_messages;
DROP FUNCTION IF EXISTS messaging_messages_guard();
DROP TABLE IF EXISTS messaging_mls_groups;
DROP FUNCTION IF EXISTS messaging_mls_groups_guard();
DROP INDEX IF EXISTS uq_messaging_outbox_events_cursor;
ALTER TABLE messaging_outbox_events DROP CONSTRAINT IF EXISTS ck_messaging_outbox_events_cursor,
    DROP COLUMN IF EXISTS conversation_event_sequence, DROP COLUMN IF EXISTS conversation_id;
CREATE OR REPLACE FUNCTION messaging_sequences_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' OR NEW.next_sequence < OLD.next_sequence THEN
        RAISE EXCEPTION 'messaging: message sequences only advance' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
ALTER TABLE messaging_conversation_sequences DROP CONSTRAINT IF EXISTS ck_messaging_conversation_sequences_event,
    DROP COLUMN IF EXISTS next_event_sequence;
DROP INDEX IF EXISTS ix_messaging_conversation_members_active;
DELETE FROM messaging_conversation_members WHERE role = 'ADMIN';
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_window,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_reason,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_removed,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_state,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_role,
    DROP COLUMN IF EXISTS updated_at, DROP COLUMN IF EXISTS version, DROP COLUMN IF EXISTS removed_reason,
    DROP COLUMN IF EXISTS removed_at, DROP COLUMN IF EXISTS visible_from_event,
    DROP COLUMN IF EXISTS visible_from_sequence, DROP COLUMN IF EXISTS send_restricted_until,
    DROP COLUMN IF EXISTS state;
ALTER TABLE messaging_conversation_members
    ADD CONSTRAINT ck_messaging_conversation_members_role CHECK (role IN ('OWNER', 'MEMBER'));
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

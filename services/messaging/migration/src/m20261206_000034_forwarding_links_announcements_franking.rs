//! Messaging gap closure S11 / S12 / S13 / S2 (additive; gap-closure history).
//!
//! * S13 `messaging_conversations.announcement_only`: only OWNER / ADMIN members may send
//!   (send, edit and forward check it); others read and react as policy allows.
//! * S11 forwarding provenance on `messaging_messages` (MANAGED only): the source message
//!   and conversation of a server-side forward, and the original sender ONLY when the
//!   environment's policy shows it (`messages.forwardShowsOriginalSender`). Set at insert,
//!   never changed afterwards.
//! * S12 `messaging_invite_links`: expiring, use-capped join links of a GROUP / CHANNEL
//!   conversation. Only the SHA-256 of the link token is stored (the token is shown once);
//!   members joining through a link record `authority_source = 'INVITE_LINK'`.
//! * S2 `messaging_message_frankings`: the sender's franking commitment of a PRIVATE_E2EE
//!   message (HMAC-SHA256 under a per-message key carried inside the ciphertext), stored
//!   immutable with the message. A report that reveals the plaintext and the key is
//!   verified against it when moderators view the report.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_conversations
    ADD COLUMN IF NOT EXISTS announcement_only BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE messaging_messages
    ADD COLUMN IF NOT EXISTS forwarded_from_message_id UUID,
    ADD COLUMN IF NOT EXISTS forwarded_from_conversation_id UUID,
    ADD COLUMN IF NOT EXISTS forwarded_sender_principal_id UUID;
ALTER TABLE messaging_messages
    ADD CONSTRAINT ck_messaging_messages_forwarded CHECK (
        (forwarded_from_message_id IS NULL AND forwarded_from_conversation_id IS NULL
            AND forwarded_sender_principal_id IS NULL)
        OR (forwarded_from_message_id IS NOT NULL AND forwarded_from_conversation_id IS NOT NULL
            AND security_profile = 'MANAGED'));
CREATE FUNCTION messaging_messages_forward_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.forwarded_from_message_id IS DISTINCT FROM OLD.forwarded_from_message_id
       OR NEW.forwarded_from_conversation_id IS DISTINCT FROM OLD.forwarded_from_conversation_id
       OR NEW.forwarded_sender_principal_id IS DISTINCT FROM OLD.forwarded_sender_principal_id THEN
        RAISE EXCEPTION 'messaging: forwarding provenance is immutable' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_messages_forward_guard BEFORE UPDATE ON messaging_messages
    FOR EACH ROW EXECUTE FUNCTION messaging_messages_forward_guard();

ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_authority,
    ADD CONSTRAINT ck_messaging_conversation_members_authority
        CHECK (authority_source IN ('API', 'INVITATION', 'MEETING', 'INVITE_LINK'));

CREATE TABLE messaging_invite_links (
    id                     UUID         NOT NULL,
    tenant_id              UUID         NOT NULL,
    application_id         UUID         NOT NULL,
    environment_id         UUID         NOT NULL,
    conversation_id        UUID         NOT NULL,
    token_sha256           CHAR(64)     NOT NULL,
    role                   VARCHAR(16)  NOT NULL,
    max_uses               INTEGER,
    use_count              INTEGER      NOT NULL DEFAULT 0,
    expires_at             TIMESTAMPTZ  NOT NULL,
    created_by_principal_id UUID,
    created_at             TIMESTAMPTZ  NOT NULL,
    revoked_at             TIMESTAMPTZ,
    revoked_by_principal_id UUID,
    CONSTRAINT pk_messaging_invite_links PRIMARY KEY (id),
    CONSTRAINT uq_messaging_invite_links_token UNIQUE (token_sha256),
    CONSTRAINT fk_messaging_invite_links_conversation FOREIGN KEY (conversation_id)
        REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_invite_links_token CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_invite_links_role CHECK (role IN ('MEMBER', 'READ_ONLY')),
    CONSTRAINT ck_messaging_invite_links_uses CHECK (
        (max_uses IS NULL OR max_uses BETWEEN 1 AND 10000) AND use_count >= 0
        AND (max_uses IS NULL OR use_count <= max_uses)),
    CONSTRAINT ck_messaging_invite_links_expiry CHECK (expires_at > created_at),
    CONSTRAINT ck_messaging_invite_links_revoked CHECK (revoked_at IS NOT NULL OR revoked_by_principal_id IS NULL)
);
CREATE INDEX ix_messaging_invite_links_conversation
    ON messaging_invite_links (conversation_id, created_at DESC);
CREATE FUNCTION messaging_invite_links_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: invite links are revoked, never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.conversation_id <> OLD.conversation_id OR NEW.token_sha256 <> OLD.token_sha256
       OR NEW.role <> OLD.role OR NEW.max_uses IS DISTINCT FROM OLD.max_uses OR NEW.expires_at <> OLD.expires_at
       OR NEW.created_at <> OLD.created_at OR NEW.environment_id <> OLD.environment_id THEN
        RAISE EXCEPTION 'messaging: an invite link is immutable' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.use_count < OLD.use_count OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at) THEN
        RAISE EXCEPTION 'messaging: invite link uses only grow and a revocation is final' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_invite_links_guard BEFORE UPDATE OR DELETE ON messaging_invite_links
    FOR EACH ROW EXECUTE FUNCTION messaging_invite_links_guard();

CREATE TABLE messaging_message_frankings (
    message_id       UUID         NOT NULL,
    conversation_id  UUID         NOT NULL,
    commitment       CHAR(64)     NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_message_frankings PRIMARY KEY (message_id),
    CONSTRAINT fk_messaging_message_frankings_message FOREIGN KEY (message_id) REFERENCES messaging_messages (id),
    CONSTRAINT ck_messaging_message_frankings_commitment CHECK (commitment ~ '^[0-9a-f]{64}$')
);
CREATE FUNCTION messaging_message_frankings_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'messaging: a franking commitment is immutable' USING ERRCODE = 'P0001';
END $$;
CREATE TRIGGER trg_messaging_message_frankings_guard BEFORE UPDATE ON messaging_message_frankings
    FOR EACH ROW EXECUTE FUNCTION messaging_message_frankings_guard();

"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_message_frankings;
DROP FUNCTION IF EXISTS messaging_message_frankings_guard();
DROP TABLE IF EXISTS messaging_invite_links;
DROP FUNCTION IF EXISTS messaging_invite_links_guard();
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_authority,
    ADD CONSTRAINT ck_messaging_conversation_members_authority
        CHECK (authority_source IN ('API', 'INVITATION', 'MEETING')) NOT VALID;
DROP TRIGGER IF EXISTS trg_messaging_messages_forward_guard ON messaging_messages;
DROP FUNCTION IF EXISTS messaging_messages_forward_guard();
ALTER TABLE messaging_messages DROP CONSTRAINT IF EXISTS ck_messaging_messages_forwarded;
ALTER TABLE messaging_messages
    DROP COLUMN IF EXISTS forwarded_sender_principal_id,
    DROP COLUMN IF EXISTS forwarded_from_conversation_id,
    DROP COLUMN IF EXISTS forwarded_from_message_id;
ALTER TABLE messaging_conversations DROP COLUMN IF EXISTS announcement_only;
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

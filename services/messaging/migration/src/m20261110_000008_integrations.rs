//! Part V H §§97–101 — meeting chat integration and migration.
//!
//! * `messaging_messages` gains SECURITY PROVENANCE: `NATIVE` (created through Messaging)
//!   or `LEGACY_MANAGED` (imported from a legacy chat store). Legacy content is MANAGED
//!   only — a CHECK makes a legacy PRIVATE_E2EE message impossible, so historical
//!   plaintext can never be presented as end-to-end encrypted. Imports are idempotent per
//!   (conversation, legacy source, legacy message id) and keep the original time.
//!   `managed_presentation` distinguishes MANAGED announcements from ordinary messages.
//! * `messaging_sync_fences` — monotonic per-key source versions of a peer authority
//!   (meeting membership, lifecycle, communication-context status): an update at or
//!   below the stored version is stale and never re-applied, so a delayed or replayed
//!   update cannot restore withdrawn access.
//! * `messaging_conversation_bans` — principals banned from a conversation: they cannot
//!   be (re)added until the ban is lifted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_messages
    ADD COLUMN security_provenance VARCHAR(16) NOT NULL DEFAULT 'NATIVE',
    ADD COLUMN legacy_source VARCHAR(48),
    ADD COLUMN legacy_message_id VARCHAR(128),
    ADD COLUMN original_created_at TIMESTAMPTZ,
    ADD COLUMN managed_presentation VARCHAR(16),
    ADD CONSTRAINT ck_messaging_messages_provenance CHECK (
        (security_provenance = 'NATIVE' AND legacy_source IS NULL AND legacy_message_id IS NULL AND original_created_at IS NULL)
        OR (security_provenance = 'LEGACY_MANAGED' AND security_profile = 'MANAGED' AND message_kind = 'APPLICATION'
            AND legacy_source IS NOT NULL AND legacy_message_id IS NOT NULL AND original_created_at IS NOT NULL)),
    ADD CONSTRAINT ck_messaging_messages_legacy_ref CHECK (
        (legacy_source IS NULL OR legacy_source ~ '^[a-z][a-z0-9.-]{1,47}$')
        AND (legacy_message_id IS NULL OR legacy_message_id ~ '^[A-Za-z0-9._:-]{1,128}$')),
    ADD CONSTRAINT ck_messaging_messages_presentation CHECK (managed_presentation IS NULL
        OR (security_profile = 'MANAGED' AND managed_presentation IN ('STANDARD', 'ANNOUNCEMENT')));
CREATE UNIQUE INDEX uq_messaging_messages_legacy ON messaging_messages (conversation_id, legacy_source, legacy_message_id)
    WHERE legacy_message_id IS NOT NULL;

CREATE OR REPLACE FUNCTION messaging_messages_provenance_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.security_provenance <> OLD.security_provenance
       OR NEW.legacy_source IS DISTINCT FROM OLD.legacy_source
       OR NEW.legacy_message_id IS DISTINCT FROM OLD.legacy_message_id
       OR NEW.original_created_at IS DISTINCT FROM OLD.original_created_at THEN
        RAISE EXCEPTION 'messaging: message provenance is immutable' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_messages_provenance_guard BEFORE UPDATE ON messaging_messages
    FOR EACH ROW EXECUTE FUNCTION messaging_messages_provenance_guard();

CREATE TABLE messaging_sync_fences (
    conversation_id   UUID          NOT NULL,
    fence_key         VARCHAR(96)   NOT NULL,
    source            VARCHAR(48)   NOT NULL,
    source_version    BIGINT        NOT NULL,
    applied_at        TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_sync_fences PRIMARY KEY (conversation_id, fence_key),
    CONSTRAINT fk_messaging_sync_fences_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_sync_fences_version CHECK (source_version >= 0),
    CONSTRAINT ck_messaging_sync_fences_source CHECK (source ~ '^[a-z][a-z0-9.-]{1,47}$'),
    CONSTRAINT ck_messaging_sync_fences_key CHECK (fence_key ~ '^[a-z][a-z0-9:._-]{1,95}$')
);
CREATE FUNCTION messaging_sync_fences_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: sync fences are never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.source_version <= OLD.source_version OR NEW.source <> OLD.source THEN
        RAISE EXCEPTION 'messaging: a sync fence only moves forward' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_sync_fences_guard BEFORE UPDATE OR DELETE ON messaging_sync_fences
    FOR EACH ROW EXECUTE FUNCTION messaging_sync_fences_guard();

CREATE TABLE messaging_conversation_bans (
    conversation_id   UUID          NOT NULL,
    principal_id      UUID          NOT NULL,
    environment_id    UUID          NOT NULL,
    reason            VARCHAR(48)   NOT NULL,
    created_at        TIMESTAMPTZ   NOT NULL,
    lifted_at         TIMESTAMPTZ,
    CONSTRAINT pk_messaging_conversation_bans PRIMARY KEY (conversation_id, principal_id),
    CONSTRAINT fk_messaging_conversation_bans_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_conversation_bans_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_conversation_bans;
DROP TABLE IF EXISTS messaging_sync_fences;
DROP FUNCTION IF EXISTS messaging_sync_fences_guard();
DROP TRIGGER IF EXISTS trg_messaging_messages_provenance_guard ON messaging_messages;
DROP FUNCTION IF EXISTS messaging_messages_provenance_guard();
DROP INDEX IF EXISTS uq_messaging_messages_legacy;
ALTER TABLE messaging_messages
    DROP CONSTRAINT IF EXISTS ck_messaging_messages_presentation,
    DROP CONSTRAINT IF EXISTS ck_messaging_messages_legacy_ref,
    DROP CONSTRAINT IF EXISTS ck_messaging_messages_provenance,
    DROP COLUMN IF EXISTS managed_presentation,
    DROP COLUMN IF EXISTS original_created_at,
    DROP COLUMN IF EXISTS legacy_message_id,
    DROP COLUMN IF EXISTS legacy_source,
    DROP COLUMN IF EXISTS security_provenance;
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

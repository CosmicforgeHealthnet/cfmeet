//! Attachments (Part V Part F, §69–§80).
//!
//! * `messaging_attachments` — authorized metadata and the server-generated object key of
//!   one uploaded object (the bytes live in protected object storage, never here). Bound
//!   to tenant / application / environment / conversation / uploader / security profile.
//!   Lifecycle: INTENT_CREATED → VERIFYING → READY | QUARANTINED | REJECTED, or EXPIRED,
//!   and DELETED once the object is gone. PRIVATE_E2EE rows can hold no server-readable
//!   presentation metadata (no file name, media metadata or real media type) and are never
//!   reported as malware-scanned (`NOT_APPLICABLE_ENCRYPTED`); a CHECK enforces both.
//! * `messaging_storage_usage` / `messaging_upload_days` — atomic quota accounting
//!   (committed and reserved bytes per environment; daily upload volume).
//! * `messaging_object_deletions` — the deletion queue: every object to remove (expired
//!   intents, rejected uploads, purges, orphans) stays here until deleted or DEAD
//!   (visible), never silently dropped.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_attachments (
    id                    UUID          NOT NULL,
    tenant_id             UUID          NOT NULL,
    application_id        UUID          NOT NULL,
    environment_id        UUID          NOT NULL,
    conversation_id       UUID          NOT NULL,
    uploader_principal_id UUID          NOT NULL,
    uploader_device_id    UUID,
    security_profile      VARCHAR(16)   NOT NULL,
    kind                  VARCHAR(16)   NOT NULL,
    state                 VARCHAR(16)   NOT NULL DEFAULT 'INTENT_CREATED',
    state_reason          VARCHAR(48),
    object_key            VARCHAR(512)  NOT NULL,
    declared_size         BIGINT        NOT NULL,
    verified_size         BIGINT,
    declared_sha256       CHAR(64)      NOT NULL,
    media_type            VARCHAR(127)  NOT NULL,
    safe_file_name        VARCHAR(255),
    media_metadata        JSONB,
    crypto_format         VARCHAR(64),
    scan_status           VARCHAR(32)   NOT NULL,
    scan_engine           VARCHAR(64),
    scan_finding          VARCHAR(128),
    scan_attempts         INTEGER       NOT NULL DEFAULT 0,
    next_scan_at          TIMESTAMPTZ,
    reserved_bytes        BIGINT        NOT NULL DEFAULT 0,
    committed_bytes       BIGINT        NOT NULL DEFAULT 0,
    message_id            UUID,
    intent_expires_at     TIMESTAMPTZ   NOT NULL,
    created_at            TIMESTAMPTZ   NOT NULL,
    finalized_at          TIMESTAMPTZ,
    state_changed_at      TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_attachments PRIMARY KEY (id),
    CONSTRAINT uq_messaging_attachments_object UNIQUE (object_key),
    CONSTRAINT fk_messaging_attachments_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT fk_messaging_attachments_message FOREIGN KEY (message_id) REFERENCES messaging_messages (id),
    CONSTRAINT ck_messaging_attachments_kind CHECK (kind IN ('IMAGE', 'DOCUMENT', 'FILE', 'VOICE_NOTE', 'VIDEO')),
    CONSTRAINT ck_messaging_attachments_state CHECK (state IN
        ('INTENT_CREATED', 'VERIFYING', 'READY', 'QUARANTINED', 'REJECTED', 'EXPIRED', 'DELETED')),
    CONSTRAINT ck_messaging_attachments_reason CHECK (state_reason IS NULL OR state_reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_attachments_size CHECK (declared_size >= 1 AND (verified_size IS NULL OR verified_size = declared_size)),
    CONSTRAINT ck_messaging_attachments_sha CHECK (declared_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_attachments_bytes CHECK (reserved_bytes >= 0 AND committed_bytes >= 0),
    CONSTRAINT ck_messaging_attachments_key CHECK (object_key ~ '^att/[0-9a-f]{32}/[0-9a-f]{32}/[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_attachments_scan CHECK (scan_status IN
        ('NOT_REQUIRED', 'PENDING', 'CLEAN', 'INFECTED', 'FAILED', 'NOT_APPLICABLE_ENCRYPTED')),
    CONSTRAINT ck_messaging_attachments_name CHECK (safe_file_name IS NULL
        OR (length(safe_file_name) BETWEEN 1 AND 255 AND safe_file_name !~ '[/\\[:cntrl:]]')),
    CONSTRAINT ck_messaging_attachments_metadata CHECK (media_metadata IS NULL
        OR (jsonb_typeof(media_metadata) = 'object' AND octet_length(media_metadata::text) <= 4096)),
    -- PRIVATE_E2EE: ciphertext only; nothing server-readable about the content; never
    -- "scanned"; a versioned client encryption format. MANAGED: no client crypto format.
    CONSTRAINT ck_messaging_attachments_profile CHECK (
        (security_profile = 'PRIVATE_E2EE' AND uploader_device_id IS NOT NULL AND crypto_format IS NOT NULL
            AND media_type = 'application/octet-stream' AND safe_file_name IS NULL AND media_metadata IS NULL
            AND scan_status = 'NOT_APPLICABLE_ENCRYPTED')
        OR (security_profile = 'MANAGED' AND crypto_format IS NULL AND scan_status <> 'NOT_APPLICABLE_ENCRYPTED')),
    -- Only a READY attachment can be referenced by a message.
    CONSTRAINT ck_messaging_attachments_linked CHECK (message_id IS NULL OR state IN ('READY', 'QUARANTINED', 'DELETED'))
);
CREATE INDEX ix_messaging_attachments_conversation ON messaging_attachments (conversation_id, created_at);
CREATE INDEX ix_messaging_attachments_message ON messaging_attachments (message_id) WHERE message_id IS NOT NULL;
CREATE INDEX ix_messaging_attachments_intents ON messaging_attachments (intent_expires_at) WHERE state = 'INTENT_CREATED';
CREATE INDEX ix_messaging_attachments_unlinked ON messaging_attachments (state_changed_at) WHERE state = 'READY' AND message_id IS NULL;
CREATE INDEX ix_messaging_attachments_scans ON messaging_attachments (next_scan_at) WHERE state = 'VERIFYING';
CREATE INDEX ix_messaging_attachments_uploader ON messaging_attachments (environment_id, uploader_principal_id, state);
CREATE FUNCTION messaging_attachments_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: attachment records are never deleted (state DELETED)' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.object_key <> OLD.object_key OR NEW.conversation_id <> OLD.conversation_id
       OR NEW.security_profile <> OLD.security_profile OR NEW.uploader_principal_id <> OLD.uploader_principal_id
       OR NEW.declared_sha256 <> OLD.declared_sha256 OR NEW.declared_size <> OLD.declared_size
       OR NEW.environment_id <> OLD.environment_id THEN
        RAISE EXCEPTION 'messaging: attachment identity is immutable' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.message_id IS NOT NULL AND NEW.message_id IS DISTINCT FROM OLD.message_id THEN
        RAISE EXCEPTION 'messaging: an attachment belongs to one message for ever' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.state IN ('REJECTED', 'EXPIRED', 'DELETED') AND NEW.state <> OLD.state AND NEW.state <> 'DELETED' THEN
        RAISE EXCEPTION 'messaging: attachment state % is terminal', OLD.state USING ERRCODE = 'P0003';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_attachments_guard BEFORE UPDATE OR DELETE ON messaging_attachments
    FOR EACH ROW EXECUTE FUNCTION messaging_attachments_guard();

CREATE TABLE messaging_storage_usage (
    environment_id   UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    committed_bytes  BIGINT       NOT NULL DEFAULT 0,
    reserved_bytes   BIGINT       NOT NULL DEFAULT 0,
    updated_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_storage_usage PRIMARY KEY (environment_id),
    CONSTRAINT ck_messaging_storage_usage_bytes CHECK (committed_bytes >= 0 AND reserved_bytes >= 0)
);

CREATE TABLE messaging_upload_days (
    environment_id   UUID         NOT NULL,
    day              DATE         NOT NULL,
    uploaded_bytes   BIGINT       NOT NULL DEFAULT 0,
    CONSTRAINT pk_messaging_upload_days PRIMARY KEY (environment_id, day),
    CONSTRAINT ck_messaging_upload_days_bytes CHECK (uploaded_bytes >= 0)
);

CREATE TABLE messaging_object_deletions (
    id               UUID          NOT NULL,
    object_key       VARCHAR(512)  NOT NULL,
    environment_id   UUID          NOT NULL,
    attachment_id    UUID,
    reason           VARCHAR(48)   NOT NULL,
    status           VARCHAR(16)   NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER       NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ   NOT NULL,
    lease_token      UUID,
    lease_expires_at TIMESTAMPTZ,
    last_error       VARCHAR(200),
    created_at       TIMESTAMPTZ   NOT NULL,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_messaging_object_deletions PRIMARY KEY (id),
    CONSTRAINT ck_messaging_object_deletions_status CHECK (status IN ('PENDING', 'DONE', 'HELD', 'DEAD')),
    CONSTRAINT ck_messaging_object_deletions_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_object_deletions_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL)),
    CONSTRAINT ck_messaging_object_deletions_done CHECK ((status = 'DONE') = (completed_at IS NOT NULL))
);
CREATE UNIQUE INDEX uq_messaging_object_deletions_open ON messaging_object_deletions (object_key) WHERE status IN ('PENDING', 'HELD');
CREATE INDEX ix_messaging_object_deletions_due ON messaging_object_deletions (next_attempt_at) WHERE status = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_object_deletions;
DROP TABLE IF EXISTS messaging_upload_days;
DROP TABLE IF EXISTS messaging_storage_usage;
DROP TABLE IF EXISTS messaging_attachments;
DROP FUNCTION IF EXISTS messaging_attachments_guard();
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

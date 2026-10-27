//! Part V Appendix I.3 / H.4 (opt-in secure backups) and §134 (independent kill switches).
//!
//! * `messaging_backups` — opaque, client-encrypted backup objects of one principal
//!   (format `CFMEET_BACKUP_XCHACHA20POLY1305_STREAM_V1` of the shared crypto core). The
//!   server keeps only what the object's public header and the upload declare: size,
//!   SHA-256, the client's backup id and the root-key id — never key material or content,
//!   and it cannot decrypt the object.
//! * Kill switches: E2EE conversation creation, MANAGED messaging, new realtime
//!   connections, search, exports, moderation actions, backups and key-transparency proofs
//!   become independently switchable.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- Orders READY backups exactly (finalization times can tie).
CREATE SEQUENCE messaging_backups_ready_seq;
CREATE TABLE messaging_backups (
    id                UUID          NOT NULL,
    tenant_id         UUID          NOT NULL,
    application_id    UUID          NOT NULL,
    environment_id    UUID          NOT NULL,
    principal_id      UUID          NOT NULL,
    device_id         UUID          NOT NULL,
    format            VARCHAR(64)   NOT NULL,
    size              BIGINT        NOT NULL,
    sha256            CHAR(64)      NOT NULL,
    object_key        VARCHAR(512)  NOT NULL,
    status            VARCHAR(16)   NOT NULL,
    client_backup_id  UUID,
    key_id            CHAR(32),
    reject_reason     VARCHAR(48),
    created_at        TIMESTAMPTZ   NOT NULL,
    upload_expires_at TIMESTAMPTZ   NOT NULL,
    finalized_at      TIMESTAMPTZ,
    ready_seq         BIGINT,
    deleted_at        TIMESTAMPTZ,
    CONSTRAINT pk_messaging_backups PRIMARY KEY (id),
    CONSTRAINT uq_messaging_backups_object UNIQUE (object_key),
    CONSTRAINT ck_messaging_backups_format CHECK (format = 'CFMEET_BACKUP_XCHACHA20POLY1305_STREAM_V1'),
    CONSTRAINT ck_messaging_backups_status CHECK (status IN ('PENDING', 'READY', 'REJECTED', 'DELETED')),
    CONSTRAINT ck_messaging_backups_size CHECK (size > 0),
    CONSTRAINT ck_messaging_backups_sha CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_backups_key_id CHECK (key_id IS NULL OR key_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_backups_ready CHECK (status <> 'READY' OR (client_backup_id IS NOT NULL AND key_id IS NOT NULL
        AND finalized_at IS NOT NULL AND ready_seq IS NOT NULL)),
    CONSTRAINT ck_messaging_backups_reason CHECK (reject_reason IS NULL OR reject_reason ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE INDEX ix_messaging_backups_owner ON messaging_backups (environment_id, principal_id, status, ready_seq DESC);

ALTER TABLE messaging_kill_switches DROP CONSTRAINT ck_messaging_kill_switches_name;
ALTER TABLE messaging_kill_switches ADD CONSTRAINT ck_messaging_kill_switches_name CHECK (switch_name IN (
    'MESSAGE_SEND', 'CONVERSATION_CREATE', 'E2EE_ONBOARDING', 'ATTACHMENT_UPLOAD', 'SMS_BRIDGE', 'WEBHOOK_RELAY',
    'E2EE_CONVERSATION_CREATE', 'MANAGED_MESSAGING', 'REALTIME_CONNECT', 'SEARCH', 'EXPORTS',
    'MODERATION_ACTIONS', 'BACKUPS', 'KT_PROOFS'));
"#;

const DOWN: &str = r#"
DELETE FROM messaging_kill_switches WHERE switch_name IN ('E2EE_CONVERSATION_CREATE', 'MANAGED_MESSAGING',
    'REALTIME_CONNECT', 'SEARCH', 'EXPORTS', 'MODERATION_ACTIONS', 'BACKUPS', 'KT_PROOFS');
ALTER TABLE messaging_kill_switches DROP CONSTRAINT ck_messaging_kill_switches_name;
ALTER TABLE messaging_kill_switches ADD CONSTRAINT ck_messaging_kill_switches_name CHECK (switch_name IN (
    'MESSAGE_SEND', 'CONVERSATION_CREATE', 'E2EE_ONBOARDING', 'ATTACHMENT_UPLOAD', 'SMS_BRIDGE', 'WEBHOOK_RELAY'));
DROP TABLE IF EXISTS messaging_backups;
DROP SEQUENCE IF EXISTS messaging_backups_ready_seq;
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

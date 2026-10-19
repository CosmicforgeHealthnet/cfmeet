//! Part V remediation — authenticated device-to-device history transfer (ADR 0052 AD-07).
//!
//! `messaging_history_transfers`: one transfer from a principal's existing (source)
//! device to its new (target) device. Messaging stores the target's single-use PUBLIC
//! transfer key and request signature, and — between sealing and completion only — the
//! opaque sealed history (HPKE ciphertext signed by the source device). It never holds a
//! key that opens it: there is no server recovery key and no escrow. The sealed bytes are
//! removed on completion, cancellation, expiry or revocation of either device.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_history_transfers (
    id                   UUID         NOT NULL,
    tenant_id            UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    environment_id       UUID         NOT NULL,
    principal_id         UUID         NOT NULL,
    source_device_id     UUID         NOT NULL REFERENCES messaging_devices (id),
    target_device_id     UUID         NOT NULL REFERENCES messaging_devices (id),
    transfer_public_key  BYTEA        NOT NULL,
    request_signature    BYTEA        NOT NULL,
    status               VARCHAR(16)  NOT NULL,
    sealed_history       BYTEA,
    sealed_sha256        BYTEA,
    sealed_bytes         BIGINT,
    end_reason           VARCHAR(32),
    created_at           TIMESTAMPTZ  NOT NULL,
    expires_at           TIMESTAMPTZ  NOT NULL,
    sealed_at            TIMESTAMPTZ,
    ended_at             TIMESTAMPTZ,
    CONSTRAINT pk_messaging_history_transfers PRIMARY KEY (id),
    CONSTRAINT ck_messaging_history_transfers_devices CHECK (source_device_id <> target_device_id),
    CONSTRAINT ck_messaging_history_transfers_key CHECK (octet_length(transfer_public_key) = 32),
    CONSTRAINT ck_messaging_history_transfers_sig CHECK (octet_length(request_signature) = 64),
    CONSTRAINT ck_messaging_history_transfers_status CHECK (status IN ('REQUESTED', 'SEALED', 'COMPLETED', 'CANCELLED', 'EXPIRED')),
    -- The sealed bytes exist exactly while SEALED.
    CONSTRAINT ck_messaging_history_transfers_sealed CHECK ((status = 'SEALED') = (sealed_history IS NOT NULL)),
    CONSTRAINT ck_messaging_history_transfers_digest CHECK (sealed_sha256 IS NULL OR octet_length(sealed_sha256) = 32),
    CONSTRAINT ck_messaging_history_transfers_reason CHECK (end_reason IS NULL OR end_reason ~ '^[A-Z][A-Z_]{1,31}$'),
    CONSTRAINT ck_messaging_history_transfers_window CHECK (expires_at > created_at)
);
-- One open transfer per target device.
CREATE UNIQUE INDEX uq_messaging_history_transfers_open_target ON messaging_history_transfers (target_device_id)
    WHERE status IN ('REQUESTED', 'SEALED');
CREATE INDEX ix_messaging_history_transfers_source ON messaging_history_transfers (source_device_id, created_at DESC);
CREATE INDEX ix_messaging_history_transfers_expiry ON messaging_history_transfers (expires_at)
    WHERE status IN ('REQUESTED', 'SEALED');
"#;

const DOWN: &str = "DROP TABLE IF EXISTS messaging_history_transfers;";

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

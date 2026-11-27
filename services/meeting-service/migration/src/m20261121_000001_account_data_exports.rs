//! ADR 0068, phase A7 — `accounts.user.export`: the personal-data export HQ staff request
//! for an account owner and the Meeting Service delivers to that owner ONLY (design §6.1,
//! owner decision 2026-11-21: HQ never sees, stores or relays exported data). Additive only.
//!
//! * `account_data_exports`: one row per requested export. The document (the same one
//!   `GET /api/v2/me/data-export` answers) is stored only SEALED (`sealed`, an AEAD blob
//!   under the service key named by `key_id`) and only while it can still be downloaded;
//!   the single-use download token only as its SHA-256 (`token_digest`). The owner's
//!   address is never stored here: only its masked form (`recipient_masked`). States:
//!   REQUESTED (recorded, being built) → READY (sealed, link emailed) → DELIVERED (the
//!   owner downloaded it once) | EXPIRED (retention over, 7 days) | FAILED. Every state but
//!   REQUESTED / READY keeps no sealed copy and no token (`ck_account_data_exports_purged`).
//!   `version` moves forward with every change.
//!
//! In the admin-operations history (`meeting_admin_operations_migrations`), required at
//! startup only while `ADMIN_OPERATOR_API_ENABLED=true`.
//!
//! Rollback: DOWN drops the table (every sealed copy goes with it).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS account_data_exports (
    id                UUID          NOT NULL,
    user_id           UUID          NOT NULL,
    idempotency_key   UUID          NOT NULL,
    delivery_ref      UUID          NOT NULL,
    state             VARCHAR(16)   NOT NULL DEFAULT 'REQUESTED',
    delivery_channel  VARCHAR(32)   NOT NULL DEFAULT 'DOWNLOAD_LINK_EMAIL',
    recipient_masked  VARCHAR(160)  NOT NULL,
    requested_by      VARCHAR(160)  NOT NULL,
    sealed            BYTEA,
    key_id            VARCHAR(32),
    token_digest      CHAR(64),
    size_bytes        BIGINT,
    sha256            CHAR(64),
    email_delivery_id UUID,
    attempts          INTEGER       NOT NULL DEFAULT 0,
    lease_until       TIMESTAMPTZ,
    failure_code      VARCHAR(48),
    requested_at      TIMESTAMPTZ   NOT NULL DEFAULT now(),
    ready_at          TIMESTAMPTZ,
    delivered_at      TIMESTAMPTZ,
    expires_at        TIMESTAMPTZ   NOT NULL,
    purged_at         TIMESTAMPTZ,
    version           INTEGER       NOT NULL DEFAULT 1,
    CONSTRAINT pk_account_data_exports PRIMARY KEY (id),
    CONSTRAINT fk_account_data_exports_user FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT uq_account_data_exports_key UNIQUE (idempotency_key),
    CONSTRAINT uq_account_data_exports_delivery UNIQUE (delivery_ref),
    CONSTRAINT ck_account_data_exports_state CHECK
        (state IN ('REQUESTED', 'READY', 'DELIVERED', 'EXPIRED', 'FAILED')),
    CONSTRAINT ck_account_data_exports_channel CHECK (delivery_channel IN ('DOWNLOAD_LINK_EMAIL')),
    CONSTRAINT ck_account_data_exports_operator CHECK (requested_by ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_account_data_exports_digest CHECK (token_digest IS NULL OR token_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_account_data_exports_sha256 CHECK (sha256 IS NULL OR sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_account_data_exports_ready CHECK
        (state <> 'READY' OR (sealed IS NOT NULL AND key_id IS NOT NULL AND token_digest IS NOT NULL
                               AND size_bytes IS NOT NULL AND sha256 IS NOT NULL AND ready_at IS NOT NULL)),
    CONSTRAINT ck_account_data_exports_purged CHECK
        (state IN ('REQUESTED', 'READY') OR (sealed IS NULL AND token_digest IS NULL)),
    CONSTRAINT ck_account_data_exports_version CHECK (version >= 1)
);
CREATE INDEX IF NOT EXISTS ix_account_data_exports_user
    ON account_data_exports (user_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_account_data_exports_open
    ON account_data_exports (expires_at) WHERE state IN ('REQUESTED', 'READY');
CREATE UNIQUE INDEX IF NOT EXISTS ux_account_data_exports_token
    ON account_data_exports (token_digest) WHERE token_digest IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS account_data_exports;
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

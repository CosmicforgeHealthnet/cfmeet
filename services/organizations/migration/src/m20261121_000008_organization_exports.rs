//! ADR 0068, phase A4 — owner-delivered organization exports. Additive only.
//!
//! HQ (through the admin bridge, R4, two-person) may ask for an organization's export, but
//! HQ never receives it: the document carries invitation addresses. Organizations builds
//! the same document as the existing export, keeps ONLY an encrypted copy (XChaCha20-Poly1305,
//! key file `ORGANIZATIONS_EXPORT_KEYS_FILE`, additional data bound to the export and
//! organization) for at most 7 days, and delivers it to the organization's ACTIVE OWNERs:
//! each owner gets one single-use, expiring download link (by email, through the
//! Notification Service, which alone resolves the owner's verified address) that works
//! only after the owner signs in.
//!
//! * `organization_exports` — one row per export: state (`READY` → `DELIVERED` once an
//!   owner downloaded it, `EXPIRED` once past `expires_at` unused, `FAILED` when no owner
//!   could be notified), row version, size and SHA-256 of the clear document, the sealed
//!   copy (`ciphertext`, `nonce`, `key_id`) until it is purged (`purged_at`: at expiry, or
//!   once every link is settled). `REQUESTED` is reserved: the document is built in the
//!   command's own transaction.
//! * `organization_export_deliveries` — one row per owner: only the SHA-256 of the link's
//!   token (the token itself exists only in the sealed notice), the notice envelope while
//!   it is being relayed (removed, with its sealed part, once settled), the relay state
//!   and the single use (`claimed_at`).
//! * `organization_admin_operations` admits the subject kind `ORGANIZATION_EXPORT`.
//!
//! Rows are deleted 90 days after creation (the clear document never outlives 7 days).
//!
//! Rollback: DOWN drops both tables and restores the previous subject constraint (ledger
//! rows of exports are removed first: the ledger is append-only, so DOWN disables its
//! trigger for that one statement).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE organization_exports (
    id               UUID         NOT NULL,
    organization_id  UUID         NOT NULL,
    state            VARCHAR(16)  NOT NULL,
    version          BIGINT       NOT NULL DEFAULT 1,
    idempotency_key  UUID         NOT NULL,
    operator_ref     VARCHAR(160) NOT NULL,
    approver_ref     VARCHAR(160) NOT NULL,
    approval_ref     VARCHAR(128) NOT NULL,
    size_bytes       BIGINT       NOT NULL,
    sha256           CHAR(64)     NOT NULL,
    key_id           VARCHAR(64),
    nonce            BYTEA,
    ciphertext       BYTEA,
    recipients       INTEGER      NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    expires_at       TIMESTAMPTZ  NOT NULL,
    delivered_at     TIMESTAMPTZ,
    purged_at        TIMESTAMPTZ,
    CONSTRAINT pk_organization_exports PRIMARY KEY (id),
    CONSTRAINT fk_organization_exports_organization FOREIGN KEY (organization_id) REFERENCES organizations (id),
    CONSTRAINT uq_organization_exports_idempotency UNIQUE (idempotency_key),
    CONSTRAINT ck_organization_exports_state CHECK (state IN ('REQUESTED', 'READY', 'DELIVERED', 'EXPIRED', 'FAILED')),
    CONSTRAINT ck_organization_exports_version CHECK (version >= 1),
    CONSTRAINT ck_organization_exports_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_organization_exports_approver CHECK (approver_ref ~ '^hq:' AND approver_ref <> operator_ref),
    CONSTRAINT ck_organization_exports_size CHECK (size_bytes >= 0),
    CONSTRAINT ck_organization_exports_sha256 CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_organization_exports_recipients CHECK (recipients >= 1),
    CONSTRAINT ck_organization_exports_sealed CHECK (
        (ciphertext IS NULL) = (nonce IS NULL)
        AND (ciphertext IS NULL) = (key_id IS NULL)
        AND (ciphertext IS NULL) = (purged_at IS NOT NULL)),
    CONSTRAINT ck_organization_exports_purged CHECK (state NOT IN ('EXPIRED', 'FAILED') OR purged_at IS NOT NULL),
    CONSTRAINT ck_organization_exports_delivered CHECK ((state = 'DELIVERED') = (delivered_at IS NOT NULL))
);
CREATE INDEX ix_organization_exports_organization
    ON organization_exports (organization_id, created_at DESC, id DESC);
CREATE INDEX ix_organization_exports_sealed
    ON organization_exports (expires_at) WHERE ciphertext IS NOT NULL;

CREATE TABLE organization_export_deliveries (
    id                 UUID         NOT NULL,
    export_id          UUID         NOT NULL,
    organization_id    UUID         NOT NULL,
    recipient_user_id  UUID         NOT NULL,
    channel            VARCHAR(24)  NOT NULL,
    token_hash         CHAR(64)     NOT NULL,
    state              VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    notice_event_id    VARCHAR(64)  NOT NULL,
    notice             JSONB,
    attempts           INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at    TIMESTAMPTZ  NOT NULL,
    last_error         VARCHAR(64),
    created_at         TIMESTAMPTZ  NOT NULL,
    expires_at         TIMESTAMPTZ  NOT NULL,
    sent_at            TIMESTAMPTZ,
    claimed_at         TIMESTAMPTZ,
    settled_at         TIMESTAMPTZ,
    CONSTRAINT pk_organization_export_deliveries PRIMARY KEY (id),
    CONSTRAINT fk_organization_export_deliveries_export FOREIGN KEY (export_id) REFERENCES organization_exports (id) ON DELETE CASCADE,
    CONSTRAINT uq_organization_export_deliveries_recipient UNIQUE (export_id, recipient_user_id),
    CONSTRAINT uq_organization_export_deliveries_token UNIQUE (token_hash),
    CONSTRAINT uq_organization_export_deliveries_notice UNIQUE (notice_event_id),
    CONSTRAINT ck_organization_export_deliveries_channel CHECK (channel IN ('DOWNLOAD_LINK_EMAIL')),
    CONSTRAINT ck_organization_export_deliveries_token CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_organization_export_deliveries_state CHECK (state IN ('PENDING', 'SENT', 'CLAIMED', 'FAILED', 'EXPIRED')),
    CONSTRAINT ck_organization_export_deliveries_notice_id CHECK (notice_event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_organization_export_deliveries_notice CHECK (state = 'PENDING' OR notice IS NULL),
    CONSTRAINT ck_organization_export_deliveries_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_organization_export_deliveries_claimed CHECK ((state = 'CLAIMED') = (claimed_at IS NOT NULL)),
    CONSTRAINT ck_organization_export_deliveries_settled CHECK ((state IN ('PENDING', 'SENT')) = (settled_at IS NULL))
);
CREATE INDEX ix_organization_export_deliveries_due
    ON organization_export_deliveries (next_attempt_at) WHERE state = 'PENDING';
CREATE INDEX ix_organization_export_deliveries_export
    ON organization_export_deliveries (export_id, created_at, id);

ALTER TABLE organization_admin_operations
    DROP CONSTRAINT ck_organization_admin_operations_subject,
    ADD CONSTRAINT ck_organization_admin_operations_subject CHECK (subject_kind IN (
        'ORGANIZATION', 'MEMBERSHIP', 'TEAM', 'POLICY', 'PROVISIONING',
        'CALLING_BINDING', 'MESSAGING_BINDING', 'EVENT_DELIVERY', 'ORGANIZATION_EXPORT'));
"#;

const DOWN: &str = r#"
ALTER TABLE organization_admin_operations DISABLE TRIGGER tr_organization_admin_operations_append_only;
DELETE FROM organization_admin_operations WHERE subject_kind = 'ORGANIZATION_EXPORT';
ALTER TABLE organization_admin_operations ENABLE TRIGGER tr_organization_admin_operations_append_only;
ALTER TABLE organization_admin_operations
    DROP CONSTRAINT ck_organization_admin_operations_subject,
    ADD CONSTRAINT ck_organization_admin_operations_subject CHECK (subject_kind IN (
        'ORGANIZATION', 'MEMBERSHIP', 'TEAM', 'POLICY', 'PROVISIONING',
        'CALLING_BINDING', 'MESSAGING_BINDING', 'EVENT_DELIVERY'));
DROP TABLE IF EXISTS organization_export_deliveries;
DROP TABLE IF EXISTS organization_exports;
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

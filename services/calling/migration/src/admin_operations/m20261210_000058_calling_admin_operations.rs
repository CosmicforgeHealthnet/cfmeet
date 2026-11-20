//! ADR 0068, phase A4 — commands relayed by the HQ admin bridge. Additive only.
//!
//! * `calling_admin_operations` records every bridge command Calling APPLIED, in the same
//!   transaction as the change, keyed by HQ's `Idempotency-Key`: the capability, the
//!   subject (kind + opaque id) and its tenant, who acted (`hq:<staff>`), who approved it
//!   (two-person commands) and the resulting state and version (NULL for an unversioned
//!   subject). The bridge settles an unknown outcome from this record
//!   (`GET /internal/v1/operator/operations/{key}`); a key that is not here was never
//!   applied. Append-only (UPDATE, DELETE and TRUNCATE are refused by trigger). Never a
//!   telephone number, SIP URI, media URL, object key, key material or free text.
//! * `platform_tenant_limits.version` / `platform_application_limits.version` (default 1,
//!   +1 on every change): the row version a bridge limit change is locked to. The
//!   previous image never writes the column (its upserts leave it as it is), so a
//!   rollback keeps working.
//!
//! Rollback: DOWN drops the table and the two columns (the operator audit keeps the
//! changes themselves).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_admin_operations (
    idempotency_key   UUID          NOT NULL,
    command           VARCHAR(64)   NOT NULL,
    subject_kind      VARCHAR(32)   NOT NULL,
    subject_id        UUID          NOT NULL,
    tenant_id         UUID          NOT NULL,
    operator_ref      VARCHAR(160)  NOT NULL,
    approver_ref      VARCHAR(160),
    approval_ref      VARCHAR(128),
    resulting_state   VARCHAR(32)   NOT NULL,
    resulting_version BIGINT,
    recorded_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_calling_admin_operations PRIMARY KEY (idempotency_key),
    CONSTRAINT ck_calling_admin_operations_command CHECK (command ~ '^calling\.[a-z][a-z0-9.-]{1,55}$'),
    CONSTRAINT ck_calling_admin_operations_subject CHECK (subject_kind IN (
        'ABUSE_REPORT', 'BLOCK_RULE', 'TENANT_LIMITS', 'APPLICATION_LIMITS', 'ROUTING_POLICY',
        'ENTERPRISE_POLICY', 'SPEND_POLICY', 'RECORDING', 'RETENTION_HOLD', 'INCIDENT',
        'WEBHOOK_KEY_RING')),
    CONSTRAINT ck_calling_admin_operations_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_calling_admin_operations_approver CHECK
        ((approver_ref IS NULL) = (approval_ref IS NULL)
         AND (approver_ref IS NULL
              OR (approver_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$' AND approver_ref <> operator_ref))),
    CONSTRAINT ck_calling_admin_operations_approval_ref CHECK
        (approval_ref IS NULL OR approval_ref ~ '^[A-Za-z0-9._:-]{1,128}$'),
    CONSTRAINT ck_calling_admin_operations_state CHECK (resulting_state ~ '^[A-Z][A-Z_]{1,31}$'),
    CONSTRAINT ck_calling_admin_operations_version CHECK (resulting_version IS NULL OR resulting_version >= 0)
);
CREATE INDEX idx_calling_admin_operations_subject
    ON calling_admin_operations (subject_kind, subject_id, recorded_at);
CREATE INDEX idx_calling_admin_operations_tenant
    ON calling_admin_operations (tenant_id, recorded_at);
CREATE TRIGGER trg_calling_admin_operations_no_update
    BEFORE UPDATE OR DELETE ON calling_admin_operations
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
CREATE TRIGGER trg_calling_admin_operations_no_truncate
    BEFORE TRUNCATE ON calling_admin_operations
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

ALTER TABLE platform_tenant_limits
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_platform_tenant_limits_version CHECK (version >= 1);
ALTER TABLE platform_application_limits
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_platform_application_limits_version CHECK (version >= 1);
"#;

const DOWN: &str = r#"
ALTER TABLE platform_application_limits DROP COLUMN IF EXISTS version;
ALTER TABLE platform_tenant_limits DROP COLUMN IF EXISTS version;
DROP TABLE IF EXISTS calling_admin_operations;
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

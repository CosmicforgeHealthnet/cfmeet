//! ADR 0068, phase A3 — commands relayed by the HQ admin bridge. Additive only.
//!
//! `devplat_admin_operations` records every bridge command this service APPLIED, in the
//! same transaction as the change, keyed by HQ's `Idempotency-Key`: who acted
//! (`hq:<staff>`), who approved it (two-person decisions), whether the actor's CF Meet
//! account was linked for the no-self-review check, and the resulting state and version.
//! The bridge settles an unknown outcome from this record
//! (`GET /internal/v1/operator/operations/{key}`); a key that is not here was never
//! applied. Append-only.
//!
//! Rollback: DOWN drops the table (the transitions keep the decisions themselves).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_admin_operations (
    idempotency_key   UUID          NOT NULL,
    command           VARCHAR(48)   NOT NULL,
    subject_kind      VARCHAR(24)   NOT NULL,
    subject_id        UUID          NOT NULL,
    tenant_id         UUID          NOT NULL,
    operator_ref      VARCHAR(160)  NOT NULL,
    approver_ref      VARCHAR(160),
    approval_ref      VARCHAR(128),
    staff_link        VARCHAR(16)   NOT NULL,
    resulting_state   VARCHAR(24)   NOT NULL,
    resulting_version BIGINT        NOT NULL,
    recorded_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_admin_operations PRIMARY KEY (idempotency_key),
    CONSTRAINT ck_devplat_admin_operations_subject CHECK (subject_kind IN ('PRODUCTION_ACCESS', 'APP')),
    CONSTRAINT ck_devplat_admin_operations_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_devplat_admin_operations_approver CHECK
        ((approver_ref IS NULL) = (approval_ref IS NULL)
         AND (approver_ref IS NULL OR (approver_ref ~ '^hq:' AND approver_ref <> operator_ref))),
    CONSTRAINT ck_devplat_admin_operations_link CHECK (staff_link IN ('LINKED', 'UNLINKED')),
    CONSTRAINT ck_devplat_admin_operations_state CHECK (resulting_state ~ '^[A-Z][A-Z_]{1,23}$')
);
CREATE INDEX ix_devplat_admin_operations_subject ON devplat_admin_operations (subject_kind, subject_id, recorded_at);
CREATE TRIGGER tr_devplat_admin_operations_append_only BEFORE UPDATE OR DELETE ON devplat_admin_operations
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_admin_operations_no_truncate BEFORE TRUNCATE ON devplat_admin_operations
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_admin_operations;
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

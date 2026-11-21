//! ADR 0068, phase A4 — commands relayed by the HQ admin bridge. Additive only.
//!
//! `organization_admin_operations` records every bridge command this service APPLIED, in
//! the same transaction as the change, keyed by HQ's `Idempotency-Key`: who acted
//! (`hq:<staff>`), who approved it (two-person R3 commands), the subject and the resulting
//! state and version. The bridge settles an unknown outcome from this record
//! (`GET /internal/v1/operator/operations/{key}`); a key that is not here was never
//! applied, and a repeated key is never applied twice. Append-only.
//!
//! * `subject_id` is the organization for organization-level subjects (organization,
//!   ownership, policy, provisioning, service bindings), the membership or team id for
//!   member / team commands, and the nil UUID for event delivery (no single subject).
//! * `resulting_version` is NULL for unversioned subjects (provisioning, event delivery, a
//!   cleared binding).
//!
//! Rollback: DOWN drops the table (the changes themselves stay, with their audit events).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE organization_admin_operations (
    idempotency_key   UUID          NOT NULL,
    command           VARCHAR(48)   NOT NULL,
    subject_kind      VARCHAR(24)   NOT NULL,
    subject_id        UUID          NOT NULL,
    organization_id   UUID,
    operator_ref      VARCHAR(160)  NOT NULL,
    approver_ref      VARCHAR(160),
    approval_ref      VARCHAR(128),
    resulting_state   VARCHAR(24)   NOT NULL,
    resulting_version BIGINT,
    recorded_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_organization_admin_operations PRIMARY KEY (idempotency_key),
    CONSTRAINT ck_organization_admin_operations_command CHECK (command ~ '^[a-z][a-z.-]{1,47}$'),
    CONSTRAINT ck_organization_admin_operations_subject CHECK (subject_kind IN (
        'ORGANIZATION', 'MEMBERSHIP', 'TEAM', 'POLICY', 'PROVISIONING',
        'CALLING_BINDING', 'MESSAGING_BINDING', 'EVENT_DELIVERY')),
    CONSTRAINT ck_organization_admin_operations_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_organization_admin_operations_approver CHECK
        ((approver_ref IS NULL) = (approval_ref IS NULL)
         AND (approver_ref IS NULL OR (approver_ref ~ '^hq:' AND approver_ref <> operator_ref))),
    CONSTRAINT ck_organization_admin_operations_state CHECK (resulting_state ~ '^[A-Z][A-Z_]{1,23}$'),
    CONSTRAINT ck_organization_admin_operations_version CHECK (resulting_version IS NULL OR resulting_version >= 0)
);
CREATE INDEX ix_organization_admin_operations_subject
    ON organization_admin_operations (subject_kind, subject_id, recorded_at);
CREATE TRIGGER tr_organization_admin_operations_append_only BEFORE UPDATE OR DELETE ON organization_admin_operations
    FOR EACH ROW EXECUTE FUNCTION organization_append_only();
CREATE TRIGGER tr_organization_admin_operations_no_truncate BEFORE TRUNCATE ON organization_admin_operations
    FOR EACH STATEMENT EXECUTE FUNCTION organization_append_only();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS organization_admin_operations;
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

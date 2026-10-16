//! Abuse reports (Part H §69): a participant of a Call reports the communication.
//!
//! One report per (call, reporting principal); the reported principal is derived by the
//! server (the other party of a two-party call), never supplied. The report carries a
//! bounded category and an optional bounded comment CODE — never free text, a number or
//! a provider payload. Operators review through `platform_admin abuse-report
//! set-status` (audited). A burst of reports against one principal raises an
//! ABUSE_REPORTS_BURST incident.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_abuse_reports (
    id                     UUID        NOT NULL,
    tenant_id              UUID        NOT NULL,
    application_id         UUID        NOT NULL,
    environment_id         UUID        NOT NULL,
    reporting_principal_id UUID        NOT NULL,
    call_id                UUID        NOT NULL,
    reported_principal_id  UUID,
    category               TEXT        NOT NULL,
    comment_code           VARCHAR(64),
    status                 TEXT        NOT NULL DEFAULT 'OPEN',
    created_at             TIMESTAMPTZ NOT NULL,
    reviewed_at            TIMESTAMPTZ,
    version                BIGINT      NOT NULL DEFAULT 1,
    CONSTRAINT pk_calling_abuse_reports PRIMARY KEY (id),
    CONSTRAINT uq_calling_abuse_reports_call_reporter UNIQUE (call_id, reporting_principal_id),
    CONSTRAINT fk_calling_abuse_reports_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_calling_abuse_reports_reporter
        FOREIGN KEY (tenant_id, application_id, environment_id, reporting_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_calling_abuse_reports_reported
        FOREIGN KEY (tenant_id, application_id, environment_id, reported_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_calling_abuse_reports_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_calling_abuse_reports_category CHECK (category IN (
        'UNWANTED', 'HARASSMENT', 'SPAM', 'FRAUD_SUSPECTED', 'OTHER')),
    CONSTRAINT ck_calling_abuse_reports_comment_code
        CHECK (comment_code IS NULL OR comment_code ~ '^[A-Z0-9_]{1,64}$'),
    CONSTRAINT ck_calling_abuse_reports_status CHECK (status IN ('OPEN', 'REVIEWED', 'DISMISSED')),
    CONSTRAINT ck_calling_abuse_reports_reviewed CHECK ((status <> 'OPEN') = (reviewed_at IS NOT NULL)),
    CONSTRAINT ck_calling_abuse_reports_version CHECK (version >= 1)
);
CREATE INDEX idx_calling_abuse_reports_reported
    ON calling_abuse_reports (environment_id, reported_principal_id, created_at DESC);
CREATE INDEX idx_calling_abuse_reports_reporter
    ON calling_abuse_reports (environment_id, reporting_principal_id, created_at DESC);
CREATE TRIGGER trg_calling_abuse_reports_immutable
    BEFORE UPDATE ON calling_abuse_reports
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'reporting_principal_id',
        'call_id', 'reported_principal_id', 'category', 'comment_code', 'created_at');
CREATE TRIGGER trg_calling_abuse_reports_no_delete
    BEFORE DELETE ON calling_abuse_reports
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS calling_abuse_reports;
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

//! Operator audit of platform administration (Part H §20, §73, §100).
//!
//! Every mutating `platform_admin` command records WHO (an operator id), WHAT (a bounded
//! action vocabulary), on WHICH target (kind + opaque id, scoped) and WHEN, in the same
//! transaction as the mutation it describes — the row exists if and only if the change
//! was committed. `detail` is bounded (≤ 2 KiB) and holds opaque ids and bounded codes
//! only: never a credential secret, a verifier, the pepper, plaintext or a number.
//! Append-only: UPDATE, DELETE and TRUNCATE are refused by trigger.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_operator_audit (
    id             UUID         NOT NULL,
    actor          VARCHAR(100) NOT NULL,
    correlation_id VARCHAR(64)  NOT NULL,
    action         VARCHAR(64)  NOT NULL,
    target_kind    VARCHAR(32)  NOT NULL,
    target_id      UUID,
    tenant_id      UUID,
    application_id UUID,
    environment_id UUID,
    detail         JSONB        NOT NULL DEFAULT '{}'::jsonb,
    occurred_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_operator_audit PRIMARY KEY (id),
    CONSTRAINT ck_platform_operator_audit_actor CHECK (actor ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$'),
    CONSTRAINT ck_platform_operator_audit_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_platform_operator_audit_action CHECK (action IN (
        'TENANT_STATUS_SET', 'APPLICATION_STATUS_SET', 'ENVIRONMENT_STATUS_SET',
        'ENVIRONMENT_LIVE_TELECOM_SET', 'SERVICE_ACCOUNT_STATUS_SET',
        'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
        'SCOPE_GRANTED', 'SCOPE_REVOKED', 'LIMITS_SET', 'ENTERPRISE_CAPABILITIES_SET',
        'INBOUND_OWNER_SET', 'COMPLIANCE_ELIGIBILITY_SET', 'ABUSE_REPORT_STATUS_SET',
        'BLOCK_RULE_RELEASED', 'PRINCIPAL_STATUS_SET')),
    CONSTRAINT ck_platform_operator_audit_target_kind CHECK (target_kind IN (
        'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
        'BLOCK_RULE', 'ABUSE_REPORT', 'COMPLIANCE_ELIGIBILITY')),
    CONSTRAINT ck_platform_operator_audit_detail CHECK (
        jsonb_typeof(detail) = 'object' AND octet_length(detail::text) <= 2048),
    CONSTRAINT ck_platform_operator_audit_detail_no_secret CHECK (
        NOT (detail ? 'secret' OR detail ? 'pepper' OR detail ? 'verifier' OR detail ? 'plaintext'))
);
CREATE INDEX idx_platform_operator_audit_target ON platform_operator_audit (target_kind, target_id, occurred_at DESC);
CREATE INDEX idx_platform_operator_audit_tenant ON platform_operator_audit (tenant_id, occurred_at DESC);
CREATE INDEX idx_platform_operator_audit_correlation ON platform_operator_audit (correlation_id);
CREATE TRIGGER trg_platform_operator_audit_no_update
    BEFORE UPDATE OR DELETE ON platform_operator_audit
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
CREATE TRIGGER trg_platform_operator_audit_no_truncate
    BEFORE TRUNCATE ON platform_operator_audit
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_operator_audit;
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

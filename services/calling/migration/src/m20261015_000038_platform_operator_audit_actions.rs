//! Operator audit vocabulary for every mutating `platform_admin` command (Part H §72.8).
//!
//! Migration 000034 created `platform_operator_audit` with the status / credential /
//! scope / limits vocabulary. The CLI also creates tenants, applications, environments,
//! service accounts and principals, replaces routing and spend policies and revokes a
//! compliance eligibility; each of those is an audited mutation too, so the action
//! CHECK grows by exactly those codes. The table stays append-only (its triggers are
//! untouched) and `detail` keeps its bounds.
//!
//! DOWN restores the original vocabulary as `NOT VALID`: rows written with the new codes
//! cannot be deleted (append-only), and a rollback must not fail because of them. UP adds
//! its list `NOT VALID` too (new rows are still checked): after a rollback, rows written
//! with a LATER migration's codes (000042) must not make the re-apply fail.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_action;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_action CHECK (action IN (
    'TENANT_STATUS_SET', 'APPLICATION_STATUS_SET', 'ENVIRONMENT_STATUS_SET',
    'ENVIRONMENT_LIVE_TELECOM_SET', 'SERVICE_ACCOUNT_STATUS_SET',
    'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
    'SCOPE_GRANTED', 'SCOPE_REVOKED', 'LIMITS_SET', 'ENTERPRISE_CAPABILITIES_SET',
    'INBOUND_OWNER_SET', 'COMPLIANCE_ELIGIBILITY_SET', 'ABUSE_REPORT_STATUS_SET',
    'BLOCK_RULE_RELEASED', 'PRINCIPAL_STATUS_SET',
    'TENANT_CREATED', 'APPLICATION_CREATED', 'ENVIRONMENT_CREATED',
    'SERVICE_ACCOUNT_CREATED', 'PRINCIPAL_REGISTERED', 'ROUTING_POLICY_SET',
    'SPEND_POLICY_SET', 'COMPLIANCE_ELIGIBILITY_REVOKED')) NOT VALID;
"#;

const DOWN: &str = r#"
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_action;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_action CHECK (action IN (
    'TENANT_STATUS_SET', 'APPLICATION_STATUS_SET', 'ENVIRONMENT_STATUS_SET',
    'ENVIRONMENT_LIVE_TELECOM_SET', 'SERVICE_ACCOUNT_STATUS_SET',
    'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
    'SCOPE_GRANTED', 'SCOPE_REVOKED', 'LIMITS_SET', 'ENTERPRISE_CAPABILITIES_SET',
    'INBOUND_OWNER_SET', 'COMPLIANCE_ELIGIBILITY_SET', 'ABUSE_REPORT_STATUS_SET',
    'BLOCK_RULE_RELEASED', 'PRINCIPAL_STATUS_SET')) NOT VALID;
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

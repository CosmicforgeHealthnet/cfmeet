//! Part G §70 closure: SERVICE_ACCOUNT and PRINCIPAL quota levels (ADR 0028).
//!
//! `platform_service_account_limits` configures an aggregate ceiling across every API
//! credential of one service account; `platform_principal_limits` configures the
//! independent communication-subject ceiling of one principal (evaluated only for an
//! operation whose principal was resolved by the server in the caller's scope). Same
//! columns and semantics as the tenant / application / credential levels: NULL = not
//! configured, configured values >= 0 (requests per minute > 0). Both reference Calling's
//! own authorities with ownership-consistent composite keys (no cross-service key).
//! The request-rate counters gain SERVICE_ACCOUNT and PRINCIPAL subjects.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_service_account_limits (
    service_account_id                UUID PRIMARY KEY,
    tenant_id                         UUID        NOT NULL,
    application_id                    UUID        NOT NULL,
    environment_id                    UUID        NOT NULL,
    requests_per_minute               INTEGER,
    max_concurrent_communications     INTEGER,
    telecom_spend_daily_limit_minor   BIGINT,
    telecom_spend_monthly_limit_minor BIGINT,
    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_service_account_limits_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_platform_service_account_limits_requests_per_minute
        CHECK (requests_per_minute IS NULL OR requests_per_minute > 0),
    CONSTRAINT ck_platform_service_account_limits_concurrent
        CHECK (max_concurrent_communications IS NULL OR max_concurrent_communications >= 0),
    CONSTRAINT ck_platform_service_account_limits_spend_daily
        CHECK (telecom_spend_daily_limit_minor IS NULL OR telecom_spend_daily_limit_minor >= 0),
    CONSTRAINT ck_platform_service_account_limits_spend_monthly
        CHECK (telecom_spend_monthly_limit_minor IS NULL OR telecom_spend_monthly_limit_minor >= 0)
);
CREATE INDEX idx_platform_service_account_limits_application
    ON platform_service_account_limits (application_id);
CREATE TRIGGER trg_platform_service_account_limits_immutable
    BEFORE UPDATE ON platform_service_account_limits
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'service_account_id', 'tenant_id', 'application_id', 'environment_id', 'created_at');

CREATE TABLE platform_principal_limits (
    principal_id                      UUID PRIMARY KEY,
    tenant_id                         UUID        NOT NULL,
    application_id                    UUID        NOT NULL,
    environment_id                    UUID        NOT NULL,
    requests_per_minute               INTEGER,
    max_concurrent_communications     INTEGER,
    telecom_spend_daily_limit_minor   BIGINT,
    telecom_spend_monthly_limit_minor BIGINT,
    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_principal_limits_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_platform_principal_limits_requests_per_minute
        CHECK (requests_per_minute IS NULL OR requests_per_minute > 0),
    CONSTRAINT ck_platform_principal_limits_concurrent
        CHECK (max_concurrent_communications IS NULL OR max_concurrent_communications >= 0),
    CONSTRAINT ck_platform_principal_limits_spend_daily
        CHECK (telecom_spend_daily_limit_minor IS NULL OR telecom_spend_daily_limit_minor >= 0),
    CONSTRAINT ck_platform_principal_limits_spend_monthly
        CHECK (telecom_spend_monthly_limit_minor IS NULL OR telecom_spend_monthly_limit_minor >= 0)
);
CREATE INDEX idx_platform_principal_limits_application
    ON platform_principal_limits (application_id);
CREATE TRIGGER trg_platform_principal_limits_immutable
    BEFORE UPDATE ON platform_principal_limits
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'principal_id', 'tenant_id', 'application_id', 'environment_id', 'created_at');

ALTER TABLE platform_request_rate_windows
    DROP CONSTRAINT ck_platform_request_rate_windows_subject_kind,
    ADD CONSTRAINT ck_platform_request_rate_windows_subject_kind
        CHECK (subject_kind IN ('TENANT', 'APPLICATION', 'SERVICE_ACCOUNT', 'CREDENTIAL',
                                'PRINCIPAL'));

-- Concurrency counts of the new levels.
CREATE INDEX idx_calls_created_by_service_account ON calls (created_by_service_account_id);
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_calls_created_by_service_account;
DELETE FROM platform_request_rate_windows WHERE subject_kind IN ('SERVICE_ACCOUNT', 'PRINCIPAL');
ALTER TABLE platform_request_rate_windows
    DROP CONSTRAINT ck_platform_request_rate_windows_subject_kind,
    ADD CONSTRAINT ck_platform_request_rate_windows_subject_kind
        CHECK (subject_kind IN ('TENANT', 'APPLICATION', 'CREDENTIAL'));
DROP TABLE IF EXISTS platform_principal_limits;
DROP TABLE IF EXISTS platform_service_account_limits;
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

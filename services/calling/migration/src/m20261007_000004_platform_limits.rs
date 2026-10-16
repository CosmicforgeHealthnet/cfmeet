//! Server-authoritative limit policy at application and credential level, plus the
//! request-rate counters used to enforce the request-rate limit.
//!
//! A NULL limit column means "not configured at this level". The effective policy for an
//! authenticated credential is resolved by the service: where both levels configure the
//! same limit, the stricter (smaller) value wins; where neither does, the platform default
//! applies. Telecom-spend values are minor units of the tenant's billing currency.
//!
//! `platform_request_rate_windows` holds short-lived fixed-window counters. It is UNLOGGED:
//! counters are disposable (a crash resets the current window) and are pruned
//! continuously by the service.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_application_limits (
    application_id                    UUID PRIMARY KEY,
    tenant_id                         UUID        NOT NULL,
    requests_per_minute               INTEGER,
    max_concurrent_communications     INTEGER,
    telecom_spend_daily_limit_minor   BIGINT,
    telecom_spend_monthly_limit_minor BIGINT,
    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_application_limits_application
        FOREIGN KEY (tenant_id, application_id)
        REFERENCES platform_applications (tenant_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_application_limits_requests_per_minute
        CHECK (requests_per_minute IS NULL OR requests_per_minute > 0),
    CONSTRAINT ck_platform_application_limits_concurrent
        CHECK (max_concurrent_communications IS NULL OR max_concurrent_communications >= 0),
    CONSTRAINT ck_platform_application_limits_spend_daily
        CHECK (telecom_spend_daily_limit_minor IS NULL OR telecom_spend_daily_limit_minor >= 0),
    CONSTRAINT ck_platform_application_limits_spend_monthly
        CHECK (telecom_spend_monthly_limit_minor IS NULL OR telecom_spend_monthly_limit_minor >= 0)
);
CREATE INDEX idx_platform_application_limits_tenant ON platform_application_limits (tenant_id);
CREATE TRIGGER trg_platform_application_limits_immutable
    BEFORE UPDATE ON platform_application_limits
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('application_id', 'tenant_id', 'created_at');

CREATE TABLE platform_credential_limits (
    credential_id                     UUID PRIMARY KEY,
    tenant_id                         UUID        NOT NULL,
    application_id                    UUID        NOT NULL,
    requests_per_minute               INTEGER,
    max_concurrent_communications     INTEGER,
    telecom_spend_daily_limit_minor   BIGINT,
    telecom_spend_monthly_limit_minor BIGINT,
    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_credential_limits_credential
        FOREIGN KEY (tenant_id, application_id, credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_credential_limits_requests_per_minute
        CHECK (requests_per_minute IS NULL OR requests_per_minute > 0),
    CONSTRAINT ck_platform_credential_limits_concurrent
        CHECK (max_concurrent_communications IS NULL OR max_concurrent_communications >= 0),
    CONSTRAINT ck_platform_credential_limits_spend_daily
        CHECK (telecom_spend_daily_limit_minor IS NULL OR telecom_spend_daily_limit_minor >= 0),
    CONSTRAINT ck_platform_credential_limits_spend_monthly
        CHECK (telecom_spend_monthly_limit_minor IS NULL OR telecom_spend_monthly_limit_minor >= 0)
);
CREATE INDEX idx_platform_credential_limits_application ON platform_credential_limits (application_id);
CREATE TRIGGER trg_platform_credential_limits_immutable
    BEFORE UPDATE ON platform_credential_limits
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('credential_id', 'tenant_id', 'application_id', 'created_at');

CREATE UNLOGGED TABLE platform_request_rate_windows (
    subject_kind  TEXT        NOT NULL,
    subject_id    UUID        NOT NULL,
    window_start  TIMESTAMPTZ NOT NULL,
    request_count BIGINT      NOT NULL,
    CONSTRAINT pk_platform_request_rate_windows PRIMARY KEY (subject_kind, subject_id, window_start),
    CONSTRAINT ck_platform_request_rate_windows_subject_kind CHECK (subject_kind IN ('APPLICATION', 'CREDENTIAL')),
    CONSTRAINT ck_platform_request_rate_windows_count CHECK (request_count > 0)
);
CREATE INDEX idx_platform_request_rate_windows_window_start ON platform_request_rate_windows (window_start);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_request_rate_windows;
DROP TABLE IF EXISTS platform_credential_limits;
DROP TABLE IF EXISTS platform_application_limits;
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

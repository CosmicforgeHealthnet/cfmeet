//! Part G §69 / §70: usage completeness (ADR 0028).
//!
//! `call_usage_records` stays the ONE authoritative store of consumption facts (never an
//! invoice): it gains the public-network LEG facts (route class, destination country,
//! billable seconds and an honest cost — KNOWN / ESTIMATED / UNKNOWN, an UNKNOWN cost has
//! no amount, never zero) and ARTIFACT facts (voicemail / recording duration and stored
//! size). Every fact keeps its stable idempotency key (unique per Call), so late or
//! repeated provider events and finalizations never double minutes, cost or storage.
//!
//! §70 adds the TENANT level of the limit policy (`platform_tenant_limits`, same columns
//! and semantics as the application / credential levels: NULL = not configured). The
//! effective limit is the most restrictive of tenant → application → credential (→ the
//! platform default), and each configured level is also an aggregate ceiling across
//! everything beneath it. The request-rate counters gain a TENANT subject.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE call_usage_records
    ADD COLUMN route_class         TEXT,
    ADD COLUMN destination_country CHAR(2),
    ADD COLUMN billable_seconds    INTEGER,
    ADD COLUMN cost_status         TEXT,
    ADD COLUMN cost_minor          BIGINT,
    ADD COLUMN cost_currency       CHAR(3),
    ADD COLUMN size_bytes          BIGINT,
    DROP CONSTRAINT ck_call_usage_records_type,
    ADD CONSTRAINT ck_call_usage_records_type CHECK (usage_type IN (
        'CALL_CONNECTED_TIME', 'PARTICIPANT_CONNECTED_TIME', 'SESSION_CONNECTED_TIME',
        'LEG_CONNECTED_TIME', 'VOICEMAIL_ARTIFACT', 'RECORDING_ARTIFACT')),
    ADD CONSTRAINT ck_call_usage_records_cost CHECK (
        (cost_status IS NULL AND cost_minor IS NULL AND cost_currency IS NULL)
        OR (cost_status = 'UNKNOWN' AND cost_minor IS NULL AND cost_currency IS NULL)
        OR (cost_status IN ('KNOWN', 'ESTIMATED') AND cost_minor >= 0
            AND cost_currency ~ '^[A-Z]{3}$')),
    ADD CONSTRAINT ck_call_usage_records_leg_facts CHECK (
        (usage_type = 'LEG_CONNECTED_TIME') OR (route_class IS NULL AND destination_country IS NULL
            AND billable_seconds IS NULL AND cost_status IS NULL)),
    ADD CONSTRAINT ck_call_usage_records_artifact CHECK (
        (usage_type IN ('VOICEMAIL_ARTIFACT', 'RECORDING_ARTIFACT')) = (size_bytes IS NOT NULL)
        AND (size_bytes IS NULL OR size_bytes >= 0)),
    ADD CONSTRAINT ck_call_usage_records_country CHECK (
        destination_country IS NULL OR destination_country ~ '^[A-Z]{2}$'),
    ADD CONSTRAINT ck_call_usage_records_billable CHECK (billable_seconds IS NULL OR billable_seconds >= 0);
-- A recorded fact never changes (a late provider cost cannot overwrite it).
CREATE TRIGGER trg_call_usage_records_facts_immutable
    BEFORE UPDATE ON call_usage_records
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'route_class', 'destination_country', 'billable_seconds', 'cost_status', 'cost_minor',
        'cost_currency', 'size_bytes');
-- The usage API reads FINAL facts of one application environment by finalization time.
CREATE INDEX idx_call_usage_records_scope
    ON call_usage_records (tenant_id, application_id, environment_id, finalized_at, id)
    WHERE finalized_at IS NOT NULL;

CREATE TABLE platform_tenant_limits (
    tenant_id                         UUID PRIMARY KEY,
    requests_per_minute               INTEGER,
    max_concurrent_communications     INTEGER,
    telecom_spend_daily_limit_minor   BIGINT,
    telecom_spend_monthly_limit_minor BIGINT,
    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_tenant_limits_tenant
        FOREIGN KEY (tenant_id) REFERENCES platform_tenants (id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_tenant_limits_requests_per_minute
        CHECK (requests_per_minute IS NULL OR requests_per_minute > 0),
    CONSTRAINT ck_platform_tenant_limits_concurrent
        CHECK (max_concurrent_communications IS NULL OR max_concurrent_communications >= 0),
    CONSTRAINT ck_platform_tenant_limits_spend_daily
        CHECK (telecom_spend_daily_limit_minor IS NULL OR telecom_spend_daily_limit_minor >= 0),
    CONSTRAINT ck_platform_tenant_limits_spend_monthly
        CHECK (telecom_spend_monthly_limit_minor IS NULL OR telecom_spend_monthly_limit_minor >= 0)
);
CREATE TRIGGER trg_platform_tenant_limits_immutable
    BEFORE UPDATE ON platform_tenant_limits
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('tenant_id', 'created_at');

ALTER TABLE platform_request_rate_windows
    DROP CONSTRAINT ck_platform_request_rate_windows_subject_kind,
    ADD CONSTRAINT ck_platform_request_rate_windows_subject_kind
        CHECK (subject_kind IN ('TENANT', 'APPLICATION', 'CREDENTIAL'));
"#;

const DOWN: &str = r#"
DELETE FROM platform_request_rate_windows WHERE subject_kind = 'TENANT';
ALTER TABLE platform_request_rate_windows
    DROP CONSTRAINT ck_platform_request_rate_windows_subject_kind,
    ADD CONSTRAINT ck_platform_request_rate_windows_subject_kind
        CHECK (subject_kind IN ('APPLICATION', 'CREDENTIAL'));
DROP TABLE IF EXISTS platform_tenant_limits;
DROP INDEX IF EXISTS idx_call_usage_records_scope;
DROP TRIGGER IF EXISTS trg_call_usage_records_facts_immutable ON call_usage_records;
ALTER TABLE call_usage_records
    DROP CONSTRAINT ck_call_usage_records_billable,
    DROP CONSTRAINT ck_call_usage_records_country,
    DROP CONSTRAINT ck_call_usage_records_artifact,
    DROP CONSTRAINT ck_call_usage_records_leg_facts,
    DROP CONSTRAINT ck_call_usage_records_cost,
    DROP CONSTRAINT ck_call_usage_records_type,
    ADD CONSTRAINT ck_call_usage_records_type CHECK (usage_type IN (
        'CALL_CONNECTED_TIME', 'PARTICIPANT_CONNECTED_TIME', 'SESSION_CONNECTED_TIME',
        'LEG_CONNECTED_TIME')) NOT VALID,
    DROP COLUMN size_bytes,
    DROP COLUMN cost_currency,
    DROP COLUMN cost_minor,
    DROP COLUMN cost_status,
    DROP COLUMN billable_seconds,
    DROP COLUMN destination_country,
    DROP COLUMN route_class;
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

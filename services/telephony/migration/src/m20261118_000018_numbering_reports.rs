//! Gap C10: Telephony reports DCN voice usage and DCN risk signals to Numbering.
//!
//! `telephony_numbering_reports` — a durable outbox of reports for Numbering's internal
//! API (ADR 0043 decisions 7 and 8): `USAGE` (`POST /internal/v1/number-usage`, the
//! billable seconds of a finalized DCN leg) and `RISK_SIGNAL`
//! (`POST /internal/v1/number-risk-signals`, a DCN call-pattern signal). A report is
//! written in the SAME transaction as the fact it reports (the leg's usage record / the
//! scan), delivered later with capped backoff, never blocks a call, and is idempotent at
//! Numbering per `source_key`. The payload holds CF Meet ids, counts and timestamps only —
//! never a telephone number, provider id or provider call id.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_numbering_reports (
    id UUID NOT NULL,
    report_kind VARCHAR(16) NOT NULL,
    source_key VARCHAR(160) NOT NULL,
    number_resource_id UUID NOT NULL,
    telephony_leg_id UUID,
    payload JSONB NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL,
    last_error VARCHAR(64),
    created_at TIMESTAMPTZ NOT NULL,
    delivered_at TIMESTAMPTZ,
    dead_lettered_at TIMESTAMPTZ,
    CONSTRAINT pk_telephony_numbering_reports PRIMARY KEY (id),
    CONSTRAINT uq_telephony_numbering_reports_source UNIQUE (source_key),
    CONSTRAINT ck_telephony_numbering_reports_kind CHECK (report_kind IN ('USAGE', 'RISK_SIGNAL')),
    CONSTRAINT ck_telephony_numbering_reports_source CHECK (source_key ~ '^[A-Za-z0-9._:-]{1,160}$'),
    CONSTRAINT ck_telephony_numbering_reports_status CHECK (status IN ('PENDING', 'DELIVERED', 'DEAD_LETTER')),
    CONSTRAINT ck_telephony_numbering_reports_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_telephony_numbering_reports_final CHECK (
        (status = 'DELIVERED') = (delivered_at IS NOT NULL)
        AND (status = 'DEAD_LETTER') = (dead_lettered_at IS NOT NULL)),
    CONSTRAINT ck_telephony_numbering_reports_usage_leg CHECK (
        report_kind <> 'USAGE' OR telephony_leg_id IS NOT NULL)
);
CREATE INDEX idx_telephony_numbering_reports_due ON telephony_numbering_reports (next_attempt_at)
    WHERE status = 'PENDING';
CREATE INDEX idx_telephony_numbering_reports_delivered ON telephony_numbering_reports (delivered_at)
    WHERE status = 'DELIVERED';
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM telephony_numbering_reports WHERE status = 'PENDING') THEN
        RAISE EXCEPTION 'undelivered Numbering reports exist: deliver them (or disable reporting and wait) before rolling back'
            USING ERRCODE = '55000';
    END IF;
END $$;
DROP TABLE telephony_numbering_reports;
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

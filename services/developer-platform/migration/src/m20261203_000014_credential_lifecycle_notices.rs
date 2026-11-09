//! Blueprint Section 15 (PLT-008) — credential lifecycle webhooks; DATA-017 schedule.
//!
//! * `devplat_credential_lifecycle_notices`: one row per credential lifecycle notice the
//!   worker emitted (`credential.expiring_soon` at a configured lead time,
//!   `credential.expired`). The primary key (credential, event type, threshold, expiry)
//!   is the persisted deduplication key: a notice is emitted at most once per threshold
//!   for a given expiry, whatever the number of worker instances or restarts. It holds
//!   opaque Platform Access ids, the expiry instant, the threshold in days and the public
//!   webhook event id — never a credential secret, prefix, verifier or scope.
//! * `devplat_worker_schedules`: the last start / completion of periodic worker jobs
//!   (credential lifecycle sweep, reference reconciliation) so that several worker
//!   instances run each job at most once per interval.
//!
//! Additive and rollback-safe: the down migration drops only what this one created.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_credential_lifecycle_notices (
    credential_id       UUID         NOT NULL,
    event_type          VARCHAR(40)  NOT NULL,
    threshold_days      INTEGER      NOT NULL,
    expires_at          TIMESTAMPTZ  NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    service_account_id  UUID         NOT NULL,
    source_event_id     VARCHAR(64)  NOT NULL,
    webhook_event_id    VARCHAR(64)  NOT NULL,
    deliveries          INTEGER      NOT NULL DEFAULT 0,
    notified_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_credential_lifecycle_notices
        PRIMARY KEY (credential_id, event_type, threshold_days, expires_at),
    CONSTRAINT ck_devplat_credential_lifecycle_notices_type
        CHECK (event_type IN ('credential.expiring_soon', 'credential.expired')),
    CONSTRAINT ck_devplat_credential_lifecycle_notices_threshold CHECK (
        (event_type = 'credential.expired' AND threshold_days = 0)
        OR (event_type = 'credential.expiring_soon' AND threshold_days BETWEEN 1 AND 365)),
    CONSTRAINT ck_devplat_credential_lifecycle_notices_source
        CHECK (source_event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_devplat_credential_lifecycle_notices_event
        CHECK (webhook_event_id ~ '^whe_[0-9a-f]{32}$'),
    CONSTRAINT ck_devplat_credential_lifecycle_notices_deliveries CHECK (deliveries >= 0)
);
CREATE INDEX ix_devplat_credential_lifecycle_notices_environment
    ON devplat_credential_lifecycle_notices (environment_id, notified_at DESC);
CREATE TRIGGER tr_devplat_credential_lifecycle_notices_append_only
    BEFORE UPDATE OR DELETE ON devplat_credential_lifecycle_notices
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_worker_schedules (
    job                 VARCHAR(64)  NOT NULL,
    last_started_at     TIMESTAMPTZ  NOT NULL,
    last_completed_at   TIMESTAMPTZ,
    last_outcome        VARCHAR(16),
    CONSTRAINT pk_devplat_worker_schedules PRIMARY KEY (job),
    CONSTRAINT ck_devplat_worker_schedules_job CHECK (job ~ '^[a-z][a-z0-9_-]{0,63}$'),
    CONSTRAINT ck_devplat_worker_schedules_outcome
        CHECK (last_outcome IS NULL OR last_outcome IN ('SUCCEEDED', 'FAILED', 'ATTENTION'))
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_worker_schedules;
DROP TABLE IF EXISTS devplat_credential_lifecycle_notices;
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

//! ADR 0062 / 0063 — CF Meet Platform "Build" (Developer Console part). Additive only.
//!
//! * `devplat_usage_facts.usage_type` also accepts `MESSAGE_SENT` (Messaging, unit
//!   OPERATION, build contract §5): the CHECK is widened, nothing else changes.
//! * `devplat_push_providers`: METADATA of an environment's partner push credentials
//!   (ADR 0063). The secret itself is sealed to the Notification Service on receipt and
//!   never stored here: the table has no column that could hold it (provider, bundle /
//!   project and key ids, APNs host, a SHA-256 fingerprint, the verification outcome).
//!   One row per (environment, provider); SANDBOX / TEST rows can only name the APNs
//!   sandbox host.
//!
//! Rollback: DOWN drops the metadata table (the Notification Service keeps the sealed
//! credentials; re-upload to restore the console view) and restores the original usage
//! type CHECK as NOT VALID (MESSAGE_SENT facts already recorded stay).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE devplat_usage_facts DROP CONSTRAINT ck_devplat_usage_facts_type;
ALTER TABLE devplat_usage_facts ADD CONSTRAINT ck_devplat_usage_facts_type CHECK (usage_type IN (
    'API_OPERATION', 'MEETING_PARTICIPANT_MINUTE', 'CALL_MINUTE_APP', 'CALL_MINUTE_PUBLIC_NETWORK',
    'NUMBER_ACTIVE', 'SMS_SEGMENT', 'RECORDING_MINUTE', 'STORAGE_BYTE_TIME', 'MESSAGE_SENT'));

CREATE TABLE devplat_push_providers (
    id               UUID          NOT NULL,
    tenant_id        UUID          NOT NULL,
    application_id   UUID          NOT NULL,
    environment_id   UUID          NOT NULL,
    environment_kind VARCHAR(16)   NOT NULL,
    provider         VARCHAR(16)   NOT NULL,
    apns_team_id     VARCHAR(10),
    apns_key_id      VARCHAR(10),
    apns_bundle_id   VARCHAR(155),
    apns_host        VARCHAR(16),
    fcm_project_id   VARCHAR(30),
    fingerprint      CHAR(64)      NOT NULL,
    verification     VARCHAR(16)   NOT NULL,
    last_verified_at TIMESTAMPTZ,
    last_error_code  VARCHAR(64),
    created_by       VARCHAR(100)  NOT NULL,
    created_at       TIMESTAMPTZ   NOT NULL,
    updated_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_push_providers PRIMARY KEY (id),
    CONSTRAINT uq_devplat_push_providers_environment UNIQUE (environment_id, provider),
    CONSTRAINT ck_devplat_push_providers_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_devplat_push_providers_provider CHECK (provider IN ('APNS', 'APNS_VOIP', 'FCM')),
    CONSTRAINT ck_devplat_push_providers_shape CHECK (
        (provider IN ('APNS', 'APNS_VOIP') AND apns_team_id ~ '^[A-Z0-9]{10}$' AND apns_key_id ~ '^[A-Z0-9]{10}$'
            AND apns_bundle_id ~ '^[A-Za-z0-9.-]{3,155}$' AND apns_host IN ('SANDBOX', 'PRODUCTION')
            AND fcm_project_id IS NULL)
        OR (provider = 'FCM' AND fcm_project_id ~ '^[a-z][a-z0-9-]{4,28}[a-z0-9]$' AND apns_team_id IS NULL
            AND apns_key_id IS NULL AND apns_bundle_id IS NULL AND apns_host IS NULL)),
    CONSTRAINT ck_devplat_push_providers_sandbox_host CHECK (environment_kind = 'PRODUCTION' OR apns_host IS NULL OR apns_host = 'SANDBOX'),
    CONSTRAINT ck_devplat_push_providers_fingerprint CHECK (fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_devplat_push_providers_verification CHECK (verification IN ('UNVERIFIED', 'VERIFIED', 'FAILED')),
    CONSTRAINT ck_devplat_push_providers_error CHECK (last_error_code IS NULL OR last_error_code ~ '^[A-Z0-9_]{1,64}$')
);
CREATE INDEX ix_devplat_push_providers_tenant ON devplat_push_providers (tenant_id, environment_id);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_push_providers;
ALTER TABLE devplat_usage_facts DROP CONSTRAINT ck_devplat_usage_facts_type;
ALTER TABLE devplat_usage_facts ADD CONSTRAINT ck_devplat_usage_facts_type CHECK (usage_type IN (
    'API_OPERATION', 'MEETING_PARTICIPANT_MINUTE', 'CALL_MINUTE_APP', 'CALL_MINUTE_PUBLIC_NETWORK',
    'NUMBER_ACTIVE', 'SMS_SEGMENT', 'RECORDING_MINUTE', 'STORAGE_BYTE_TIME')) NOT VALID;
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

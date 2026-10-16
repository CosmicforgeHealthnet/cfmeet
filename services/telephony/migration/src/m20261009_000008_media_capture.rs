//! Edge media capture (Part F §60, §105–§108; ADR 0023).
//!
//! * `telephony_captures` — one capture Calling commanded on an internal enterprise SIP
//!   leg (CAPTURE): the edge records the caller into its TRANSIENT spool under a name
//!   Telephony derives from the capture id; when the recording finishes the audio is
//!   delivered to Calling (which stores it in protected object storage) and only then is
//!   the spool copy deleted. Delivery retries with backoff; after the bound the capture is
//!   dead-lettered (incident) and its spool copy removed anyway — application nodes never
//!   keep audio. No audio, object key, URL, number or caller data is stored here; Calling
//!   ids are opaque UUIDs (no cross-service foreign key).
//! * `telephony_commands` gains CAPTURE; incidents gain CAPTURE_DELIVERY_DEAD.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_captures (
    id                   UUID        PRIMARY KEY,
    telephony_leg_id     UUID        NOT NULL REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    command_id           UUID        NOT NULL REFERENCES telephony_commands (id) ON DELETE RESTRICT,
    tenant_id            UUID        NOT NULL,
    application_id       UUID        NOT NULL,
    environment_id       UUID        NOT NULL,
    calling_call_id      UUID        NOT NULL,
    calling_call_leg_id  UUID        NOT NULL,
    purpose              TEXT        NOT NULL,
    spool_name           VARCHAR(48) NOT NULL,
    max_duration_seconds INTEGER     NOT NULL,
    status               TEXT        NOT NULL,
    duration_ms          BIGINT,
    size_bytes           BIGINT,
    failure_code         VARCHAR(64),
    delivery_attempts    INTEGER     NOT NULL DEFAULT 0,
    next_attempt_at      TIMESTAMPTZ,
    last_error           VARCHAR(64),
    created_at           TIMESTAMPTZ NOT NULL,
    updated_at           TIMESTAMPTZ NOT NULL,
    finished_at          TIMESTAMPTZ,
    delivered_at         TIMESTAMPTZ,
    spool_deleted_at     TIMESTAMPTZ,
    CONSTRAINT uq_telephony_captures_spool UNIQUE (spool_name),
    CONSTRAINT ck_telephony_captures_spool CHECK (spool_name ~ '^cfm-cap-[0-9a-f]{32}$'),
    CONSTRAINT ck_telephony_captures_purpose CHECK (purpose IN ('VOICEMAIL')),
    CONSTRAINT ck_telephony_captures_status
        CHECK (status IN ('RECORDING', 'FINISHED', 'FAILED', 'DELIVERED', 'DEAD')),
    CONSTRAINT ck_telephony_captures_max CHECK (max_duration_seconds BETWEEN 1 AND 600),
    CONSTRAINT ck_telephony_captures_duration CHECK (duration_ms IS NULL OR duration_ms >= 0),
    CONSTRAINT ck_telephony_captures_size CHECK (size_bytes IS NULL OR size_bytes >= 0),
    CONSTRAINT ck_telephony_captures_failure
        CHECK (failure_code IS NULL OR failure_code ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_telephony_captures_error
        CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_telephony_captures_attempts CHECK (delivery_attempts >= 0),
    CONSTRAINT ck_telephony_captures_delivered CHECK ((status = 'DELIVERED') = (delivered_at IS NOT NULL)),
    CONSTRAINT ck_telephony_captures_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_telephony_captures_due ON telephony_captures (next_attempt_at)
    WHERE status IN ('FINISHED', 'FAILED');
CREATE INDEX idx_telephony_captures_recording ON telephony_captures (created_at)
    WHERE status = 'RECORDING';
CREATE INDEX idx_telephony_captures_spool ON telephony_captures (updated_at)
    WHERE status IN ('DELIVERED', 'DEAD') AND spool_deleted_at IS NULL;
CREATE INDEX idx_telephony_captures_leg ON telephony_captures (telephony_leg_id);

ALTER TABLE telephony_commands
    DROP CONSTRAINT ck_telephony_commands_kind,
    ADD CONSTRAINT ck_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'SEND_DTMF', 'QUERY_STATUS',
        'ANSWER', 'PROMPT', 'CAPTURE'));

ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL',
        'ORPHAN_ENTERPRISE_SIP_LEG', 'ENTERPRISE_DEVICE_REJECTED', 'CAPTURE_DELIVERY_DEAD'));
"#;

const DOWN: &str = r#"
ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL',
        'ORPHAN_ENTERPRISE_SIP_LEG', 'ENTERPRISE_DEVICE_REJECTED'));
ALTER TABLE telephony_commands
    DROP CONSTRAINT ck_telephony_commands_kind,
    ADD CONSTRAINT ck_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'SEND_DTMF', 'QUERY_STATUS',
        'ANSWER', 'PROMPT'));
DROP TABLE IF EXISTS telephony_captures;
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

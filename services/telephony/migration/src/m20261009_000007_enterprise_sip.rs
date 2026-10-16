//! Enterprise SIP execution primitives (Part F §54, §57, §179; ADR 0018).
//!
//! * `telephony_enterprise_devices` — desk phones / softphones registered on CF Meet's
//!   own SIP / media edge: an opaque device reference (shared with Calling, which binds it
//!   to a principal) ↔ the edge endpoint the device authenticates as, and the scope whose
//!   Calling receives its dialogs. Never a number, a SIP URI of a person, or a credential
//!   (device credentials live in the edge's own configuration / secret store).
//! * `telephony_legs` gains the ENTERPRISE_INTERNAL route class: an internal dialog of an
//!   enterprise device on the edge — no carrier, no provider, no spend, no usage fact.
//!   Its channel is bound directly (`edge_channel_ref`, internal to Telephony).
//! * `telephony_commands` gains ANSWER and PROMPT (built-in prompt playback).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_enterprise_devices (
    id             UUID         PRIMARY KEY,
    device_ref     VARCHAR(64)  NOT NULL,
    edge_endpoint  VARCHAR(64)  NOT NULL,
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    environment_id UUID         NOT NULL,
    status         TEXT         NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL,
    updated_at     TIMESTAMPTZ  NOT NULL,
    CONSTRAINT uq_telephony_enterprise_devices_ref UNIQUE (device_ref),
    CONSTRAINT uq_telephony_enterprise_devices_endpoint UNIQUE (edge_endpoint),
    CONSTRAINT ck_telephony_enterprise_devices_ref CHECK (device_ref ~ '^dev-[a-z0-9-]{4,60}$'),
    CONSTRAINT ck_telephony_enterprise_devices_endpoint
        CHECK (edge_endpoint ~ '^[a-z0-9][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_telephony_enterprise_devices_status CHECK (status IN ('ENABLED', 'DISABLED')),
    CONSTRAINT ck_telephony_enterprise_devices_timestamps CHECK (updated_at >= created_at)
);

ALTER TABLE telephony_legs
    ADD COLUMN enterprise_device_id UUID REFERENCES telephony_enterprise_devices (id) ON DELETE RESTRICT,
    ADD COLUMN edge_channel_ref     VARCHAR(160),
    DROP CONSTRAINT ck_telephony_legs_route_class,
    ADD CONSTRAINT ck_telephony_legs_route_class CHECK (route_class IN (
        'APP_TO_PSTN', 'PSTN_TO_APP', 'PSTN_RELAY', 'CALLBACK_RELAY', 'ENTERPRISE_INTERNAL')),
    DROP CONSTRAINT ck_telephony_legs_inbound,
    ADD CONSTRAINT ck_telephony_legs_inbound CHECK (direction = 'OUTBOUND'
        OR inbound_resolution_id IS NOT NULL OR route_class = 'ENTERPRISE_INTERNAL'),
    -- An internal enterprise leg is never a paid / provider / protected-party leg.
    ADD CONSTRAINT ck_telephony_legs_enterprise CHECK (
        (route_class = 'ENTERPRISE_INTERNAL') = (enterprise_device_id IS NOT NULL)
        AND (route_class <> 'ENTERPRISE_INTERNAL' OR (
            direction = 'INBOUND' AND route_id IS NULL AND provider_id IS NULL
            AND spend_reservation_id IS NULL AND contact_reference IS NULL
            AND relay_session_id IS NULL AND usage_status = 'NOT_APPLICABLE'
            AND edge_channel_ref IS NOT NULL))),
    ADD CONSTRAINT ck_telephony_legs_edge_channel
        CHECK (edge_channel_ref IS NULL OR edge_channel_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:@-]{0,159}$');
CREATE UNIQUE INDEX uq_telephony_legs_edge_channel ON telephony_legs (edge_channel_ref)
    WHERE edge_channel_ref IS NOT NULL;
CREATE INDEX idx_telephony_legs_enterprise_live ON telephony_legs (updated_at)
    WHERE route_class = 'ENTERPRISE_INTERNAL' AND status NOT IN ('ENDED', 'FAILED');

ALTER TABLE telephony_commands
    DROP CONSTRAINT ck_telephony_commands_kind,
    ADD CONSTRAINT ck_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'SEND_DTMF', 'QUERY_STATUS',
        'ANSWER', 'PROMPT'));

-- Part F §168: an enterprise SIP leg whose dialog vanished at the edge / never released,
-- and enterprise dialogs from devices that are unknown or disabled.
ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL',
        'ORPHAN_ENTERPRISE_SIP_LEG', 'ENTERPRISE_DEVICE_REJECTED'));
"#;

const DOWN: &str = r#"
ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL'));
ALTER TABLE telephony_commands
    DROP CONSTRAINT ck_telephony_commands_kind,
    ADD CONSTRAINT ck_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'SEND_DTMF', 'QUERY_STATUS'));
DROP INDEX IF EXISTS idx_telephony_legs_enterprise_live;
DROP INDEX IF EXISTS uq_telephony_legs_edge_channel;
ALTER TABLE telephony_legs
    DROP CONSTRAINT IF EXISTS ck_telephony_legs_edge_channel,
    DROP CONSTRAINT IF EXISTS ck_telephony_legs_enterprise,
    DROP CONSTRAINT ck_telephony_legs_inbound,
    ADD CONSTRAINT ck_telephony_legs_inbound CHECK (direction = 'OUTBOUND' OR inbound_resolution_id IS NOT NULL),
    DROP CONSTRAINT ck_telephony_legs_route_class,
    ADD CONSTRAINT ck_telephony_legs_route_class
        CHECK (route_class IN ('APP_TO_PSTN', 'PSTN_TO_APP', 'PSTN_RELAY', 'CALLBACK_RELAY')),
    DROP COLUMN IF EXISTS edge_channel_ref,
    DROP COLUMN IF EXISTS enterprise_device_id;
DROP TABLE IF EXISTS telephony_enterprise_devices;
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

//! Outbox dead letter, correlation continuity and operator incident resolution (Part I
//! §81, §85, §86).
//!
//! * `telephony_outbox.dead_lettered_at`: an event Calling refused permanently (a 4xx /
//!   unknown call) or that exhausted its delivery attempts is DEAD-LETTERED — set once,
//!   never delivered afterwards, never retried blindly; it raises one deduplicated
//!   `OUTBOX_DEAD_LETTER` incident. Delivered and dead-lettered are mutually exclusive.
//! * `telephony_outbox.correlation_id`: the correlation id of the command whose flow
//!   produced the event (nullable: rows written before this migration and events of
//!   legs no command touched have none). Delivery continues it instead of minting a
//!   fresh one, so Calling's trace of a call joins Telephony's. Immutable after insert.
//! * `telephony_incidents.category` gains `OUTBOX_DEAD_LETTER`,
//!   `PRIVACY_RELAY_UNAVAILABLE` (protected-contact / inbound mapping resolution could
//!   not reach Privacy Relay) and `SIP_EDGE_UNAVAILABLE` (the SIP / media edge reported
//!   degraded or unavailable health).
//! * `telephony_operator_audit` gains `INCIDENT_RESOLVE` on target kind `INCIDENT`
//!   (`telephony-admin incidents resolve <id>`).
//! * An index on dead-lettered commands serves the command-queue gauges.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE telephony_outbox
    ADD COLUMN correlation_id   VARCHAR(64),
    ADD COLUMN dead_lettered_at TIMESTAMPTZ,
    ADD CONSTRAINT ck_telephony_outbox_correlation
        CHECK (correlation_id IS NULL OR correlation_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$'),
    ADD CONSTRAINT ck_telephony_outbox_final
        CHECK (delivered_at IS NULL OR dead_lettered_at IS NULL);
CREATE INDEX idx_telephony_outbox_dead_letter ON telephony_outbox (dead_lettered_at)
    WHERE dead_lettered_at IS NOT NULL;
DROP TRIGGER trg_telephony_outbox_immutable ON telephony_outbox;
CREATE TRIGGER trg_telephony_outbox_immutable
    BEFORE UPDATE ON telephony_outbox
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'telephony_leg_id', 'bridge_id', 'calling_call_id', 'event_type', 'leg_sequence',
        'payload', 'created_at', 'correlation_id');
CREATE TRIGGER trg_telephony_outbox_dead_letter_once
    BEFORE UPDATE ON telephony_outbox
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment('dead_lettered_at');
-- Queue gauges: dead-lettered commands without scanning the command history.
CREATE INDEX idx_telephony_commands_dead_letter ON telephony_commands (finished_at)
    WHERE status = 'DEAD_LETTER';

ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL',
        'ORPHAN_ENTERPRISE_SIP_LEG', 'ENTERPRISE_DEVICE_REJECTED', 'CAPTURE_DELIVERY_DEAD',
        'CALLBACK_MALFORMED', 'CALLBACK_UNKNOWN_PROVIDER', 'CALLBACK_REPLAY_ABUSE', 'SIP_INGRESS_RATE_LIMITED',
        'OUTBOX_DEAD_LETTER', 'PRIVACY_RELAY_UNAVAILABLE', 'SIP_EDGE_UNAVAILABLE'));

ALTER TABLE telephony_operator_audit
    DROP CONSTRAINT ck_telephony_operator_audit_action,
    ADD CONSTRAINT ck_telephony_operator_audit_action CHECK (action IN (
        'PROVIDER_ADD', 'PROVIDER_ENABLE', 'PROVIDER_DISABLE', 'PROVIDER_CIRCUIT',
        'ROUTE_ADD', 'ROUTE_ENABLE', 'ROUTE_DISABLE', 'ROUTE_CIRCUIT',
        'SIP_PEER_ADD', 'NUMBER_BIND', 'NUMBER_DISABLE',
        'ENTERPRISE_DEVICE_ADD', 'ENTERPRISE_DEVICE_ENABLE', 'ENTERPRISE_DEVICE_DISABLE',
        'DESTINATION_CLASS_UPSERT', 'DESTINATION_CLASS_DISABLE',
        'RETENTION_HOLD', 'RETENTION_RELEASE', 'INCIDENT_RESOLVE')),
    DROP CONSTRAINT ck_telephony_operator_audit_target_kind,
    ADD CONSTRAINT ck_telephony_operator_audit_target_kind CHECK (target_kind IN (
        'PROVIDER', 'ROUTE', 'SIP_PEER', 'NUMBER_BINDING', 'ENTERPRISE_DEVICE', 'DESTINATION_CLASS',
        'RETENTION_HOLD', 'INCIDENT'));
"#;

/// The operator audit is append-only: its rows are never deleted, so the narrowed
/// vocabulary is restored `NOT VALID` (enforced for new rows; recorded resolutions stay).
/// Incidents of the new categories are removed (as by the retention worker).
const DOWN: &str = r#"
ALTER TABLE telephony_operator_audit
    DROP CONSTRAINT ck_telephony_operator_audit_target_kind,
    ADD CONSTRAINT ck_telephony_operator_audit_target_kind CHECK (target_kind IN (
        'PROVIDER', 'ROUTE', 'SIP_PEER', 'NUMBER_BINDING', 'ENTERPRISE_DEVICE', 'DESTINATION_CLASS',
        'RETENTION_HOLD')) NOT VALID,
    DROP CONSTRAINT ck_telephony_operator_audit_action,
    ADD CONSTRAINT ck_telephony_operator_audit_action CHECK (action IN (
        'PROVIDER_ADD', 'PROVIDER_ENABLE', 'PROVIDER_DISABLE', 'PROVIDER_CIRCUIT',
        'ROUTE_ADD', 'ROUTE_ENABLE', 'ROUTE_DISABLE', 'ROUTE_CIRCUIT',
        'SIP_PEER_ADD', 'NUMBER_BIND', 'NUMBER_DISABLE',
        'ENTERPRISE_DEVICE_ADD', 'ENTERPRISE_DEVICE_ENABLE', 'ENTERPRISE_DEVICE_DISABLE',
        'DESTINATION_CLASS_UPSERT', 'DESTINATION_CLASS_DISABLE',
        'RETENTION_HOLD', 'RETENTION_RELEASE')) NOT VALID;

SELECT set_config('telephony.retention_worker', 'on', true);
DELETE FROM telephony_incidents WHERE category IN (
    'OUTBOX_DEAD_LETTER', 'PRIVACY_RELAY_UNAVAILABLE', 'SIP_EDGE_UNAVAILABLE');
SELECT set_config('telephony.retention_worker', 'off', true);
ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL',
        'ORPHAN_ENTERPRISE_SIP_LEG', 'ENTERPRISE_DEVICE_REJECTED', 'CAPTURE_DELIVERY_DEAD',
        'CALLBACK_MALFORMED', 'CALLBACK_UNKNOWN_PROVIDER', 'CALLBACK_REPLAY_ABUSE', 'SIP_INGRESS_RATE_LIMITED'));

DROP INDEX IF EXISTS idx_telephony_commands_dead_letter;
DROP TRIGGER trg_telephony_outbox_dead_letter_once ON telephony_outbox;
DROP TRIGGER trg_telephony_outbox_immutable ON telephony_outbox;
CREATE TRIGGER trg_telephony_outbox_immutable
    BEFORE UPDATE ON telephony_outbox
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'telephony_leg_id', 'bridge_id', 'calling_call_id', 'event_type', 'leg_sequence',
        'payload', 'created_at');
DROP INDEX IF EXISTS idx_telephony_outbox_dead_letter;
ALTER TABLE telephony_outbox
    DROP CONSTRAINT ck_telephony_outbox_final,
    DROP CONSTRAINT ck_telephony_outbox_correlation,
    DROP COLUMN dead_lettered_at,
    DROP COLUMN correlation_id;
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

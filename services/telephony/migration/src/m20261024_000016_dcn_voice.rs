//! Part IV Workstream L: Digital Communications Numbers (DCNs) on the voice path.
//!
//! * `telecom_number_bindings.number_kind` — a number Telephony carries is either a
//!   Privacy Relay relay number (`RELAY`, the default for every existing binding) or a
//!   Numbering DCN (`DCN`, with its canonical `number_resource_id`). One public number has
//!   ONE kind across its interconnect bindings (trigger).
//! * `telephony_legs.dcn_*` — the DCN evidence of a leg: the Number Resource, the
//!   assignment and routing revision Numbering authorized, and the routing grant id
//!   (never the number, never the grant itself). Set once. An inbound leg is proven by a
//!   Privacy Relay inbound resolution OR a Numbering grant.
//! * `telephony_commands.dcn_identity_grant` — the single-use CALLER_IDENTITY grant an
//!   originate command carries until execution; scrubbed (trigger) as soon as the command
//!   transmits or leaves the PENDING / CLAIMED / RUNNING states.
//! * operator audit: `DCN_BIND`, `DCN_DISABLE`, `DCN_ACKNOWLEDGE` (DCN voice activation).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE telecom_number_bindings
    ADD COLUMN number_kind VARCHAR(8) NOT NULL DEFAULT 'RELAY',
    ADD COLUMN number_resource_id UUID,
    ADD CONSTRAINT ck_telecom_number_bindings_kind CHECK (
        number_kind IN ('RELAY', 'DCN') AND (number_kind = 'DCN') = (number_resource_id IS NOT NULL));
DROP TRIGGER trg_telecom_number_bindings_immutable ON telecom_number_bindings;
CREATE TRIGGER trg_telecom_number_bindings_immutable
    BEFORE UPDATE ON telecom_number_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'public_e164', 'route_id', 'created_at', 'number_kind', 'number_resource_id');
CREATE FUNCTION telephony_guard_number_kind() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM telecom_number_bindings b WHERE b.public_e164 = NEW.public_e164
               AND (b.number_kind <> NEW.number_kind
                    OR b.number_resource_id IS DISTINCT FROM NEW.number_resource_id)) THEN
        RAISE EXCEPTION 'a public number is bound with one kind (relay number or DCN) and one Number Resource'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_telecom_number_bindings_kind
    BEFORE INSERT ON telecom_number_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_guard_number_kind();
CREATE UNIQUE INDEX uq_telecom_number_bindings_dcn ON telecom_number_bindings (number_resource_id, route_id)
    WHERE number_resource_id IS NOT NULL;

ALTER TABLE telephony_legs
    ADD COLUMN dcn_number_resource_id UUID,
    ADD COLUMN dcn_assignment_id UUID,
    ADD COLUMN dcn_routing_revision BIGINT,
    ADD COLUMN dcn_grant_id UUID,
    ADD CONSTRAINT ck_telephony_legs_dcn CHECK (
        (dcn_number_resource_id IS NULL) = (dcn_assignment_id IS NULL)
        AND (dcn_number_resource_id IS NULL) = (dcn_routing_revision IS NULL)
        AND (dcn_number_resource_id IS NULL) = (dcn_grant_id IS NULL)
        AND (dcn_routing_revision IS NULL OR dcn_routing_revision >= 0)),
    DROP CONSTRAINT ck_telephony_legs_inbound,
    ADD CONSTRAINT ck_telephony_legs_inbound CHECK (direction = 'OUTBOUND'
        OR inbound_resolution_id IS NOT NULL OR dcn_grant_id IS NOT NULL
        OR route_class = 'ENTERPRISE_INTERNAL');
DROP TRIGGER trg_telephony_legs_set_once ON telephony_legs;
CREATE TRIGGER trg_telephony_legs_set_once
    BEFORE UPDATE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment(
        'route_id', 'provider_id', 'destination_country', 'provider_cost_minor', 'provider_cost_currency',
        'originating_at', 'ringing_at',
        'answered_at', 'connected_at', 'ended_at', 'end_reason', 'failure_category', 'relay_assignment_ref',
        'dcn_number_resource_id', 'dcn_assignment_id', 'dcn_routing_revision', 'dcn_grant_id');
CREATE INDEX idx_telephony_legs_dcn ON telephony_legs (dcn_number_resource_id)
    WHERE dcn_number_resource_id IS NOT NULL;

ALTER TABLE telephony_commands
    ADD COLUMN dcn_identity_grant TEXT,
    ADD CONSTRAINT ck_telephony_commands_dcn_grant CHECK (
        dcn_identity_grant IS NULL OR (kind = 'ORIGINATE' AND octet_length(dcn_identity_grant) <= 4096));
CREATE FUNCTION telephony_scrub_dcn_grant() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.transmitted_at IS NOT NULL OR NEW.status NOT IN ('PENDING', 'CLAIMED', 'RUNNING') THEN
        NEW.dcn_identity_grant := NULL;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_telephony_commands_scrub_dcn_grant
    BEFORE UPDATE ON telephony_commands
    FOR EACH ROW EXECUTE FUNCTION telephony_scrub_dcn_grant();

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
        'OUTBOX_DEAD_LETTER', 'PRIVACY_RELAY_UNAVAILABLE', 'SIP_EDGE_UNAVAILABLE', 'NUMBERING_UNAVAILABLE'));
ALTER TABLE telephony_operator_audit
    DROP CONSTRAINT ck_telephony_operator_audit_action,
    ADD CONSTRAINT ck_telephony_operator_audit_action CHECK (action IN (
        'PROVIDER_ADD', 'PROVIDER_ENABLE', 'PROVIDER_DISABLE', 'PROVIDER_CIRCUIT',
        'ROUTE_ADD', 'ROUTE_ENABLE', 'ROUTE_DISABLE', 'ROUTE_CIRCUIT',
        'SIP_PEER_ADD', 'NUMBER_BIND', 'NUMBER_DISABLE',
        'ENTERPRISE_DEVICE_ADD', 'ENTERPRISE_DEVICE_ENABLE', 'ENTERPRISE_DEVICE_DISABLE',
        'DESTINATION_CLASS_UPSERT', 'DESTINATION_CLASS_DISABLE',
        'RETENTION_HOLD', 'RETENTION_RELEASE', 'INCIDENT_RESOLVE',
        'DCN_BIND', 'DCN_DISABLE', 'DCN_ACKNOWLEDGE'));
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM telecom_number_bindings WHERE number_kind = 'DCN')
       OR EXISTS (SELECT 1 FROM telephony_legs WHERE dcn_grant_id IS NOT NULL) THEN
        RAISE EXCEPTION 'DCN bindings or DCN legs are recorded; they cannot be represented before Workstream L';
    END IF;
END $$;
ALTER TABLE telephony_operator_audit
    DROP CONSTRAINT ck_telephony_operator_audit_action,
    ADD CONSTRAINT ck_telephony_operator_audit_action CHECK (action IN (
        'PROVIDER_ADD', 'PROVIDER_ENABLE', 'PROVIDER_DISABLE', 'PROVIDER_CIRCUIT',
        'ROUTE_ADD', 'ROUTE_ENABLE', 'ROUTE_DISABLE', 'ROUTE_CIRCUIT',
        'SIP_PEER_ADD', 'NUMBER_BIND', 'NUMBER_DISABLE',
        'ENTERPRISE_DEVICE_ADD', 'ENTERPRISE_DEVICE_ENABLE', 'ENTERPRISE_DEVICE_DISABLE',
        'DESTINATION_CLASS_UPSERT', 'DESTINATION_CLASS_DISABLE',
        'RETENTION_HOLD', 'RETENTION_RELEASE', 'INCIDENT_RESOLVE')) NOT VALID;
SELECT set_config('telephony.retention_worker', 'on', true);
DELETE FROM telephony_incidents WHERE category = 'NUMBERING_UNAVAILABLE';
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
        'CALLBACK_MALFORMED', 'CALLBACK_UNKNOWN_PROVIDER', 'CALLBACK_REPLAY_ABUSE', 'SIP_INGRESS_RATE_LIMITED',
        'OUTBOX_DEAD_LETTER', 'PRIVACY_RELAY_UNAVAILABLE', 'SIP_EDGE_UNAVAILABLE'));
DROP TRIGGER IF EXISTS trg_telephony_commands_scrub_dcn_grant ON telephony_commands;
DROP FUNCTION IF EXISTS telephony_scrub_dcn_grant();
ALTER TABLE telephony_commands DROP CONSTRAINT ck_telephony_commands_dcn_grant, DROP COLUMN dcn_identity_grant;
DROP INDEX IF EXISTS idx_telephony_legs_dcn;
DROP TRIGGER trg_telephony_legs_set_once ON telephony_legs;
CREATE TRIGGER trg_telephony_legs_set_once
    BEFORE UPDATE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment(
        'route_id', 'provider_id', 'destination_country', 'provider_cost_minor', 'provider_cost_currency',
        'originating_at', 'ringing_at',
        'answered_at', 'connected_at', 'ended_at', 'end_reason', 'failure_category', 'relay_assignment_ref');
ALTER TABLE telephony_legs
    DROP CONSTRAINT ck_telephony_legs_inbound,
    ADD CONSTRAINT ck_telephony_legs_inbound CHECK (direction = 'OUTBOUND'
        OR inbound_resolution_id IS NOT NULL OR route_class = 'ENTERPRISE_INTERNAL'),
    DROP CONSTRAINT ck_telephony_legs_dcn,
    DROP COLUMN dcn_grant_id,
    DROP COLUMN dcn_routing_revision,
    DROP COLUMN dcn_assignment_id,
    DROP COLUMN dcn_number_resource_id;
DROP INDEX IF EXISTS uq_telecom_number_bindings_dcn;
DROP TRIGGER IF EXISTS trg_telecom_number_bindings_kind ON telecom_number_bindings;
DROP FUNCTION IF EXISTS telephony_guard_number_kind();
DROP TRIGGER trg_telecom_number_bindings_immutable ON telecom_number_bindings;
CREATE TRIGGER trg_telecom_number_bindings_immutable
    BEFORE UPDATE ON telecom_number_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change('public_e164', 'route_id', 'created_at');
ALTER TABLE telecom_number_bindings
    DROP CONSTRAINT ck_telecom_number_bindings_kind,
    DROP COLUMN number_resource_id,
    DROP COLUMN number_kind;
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

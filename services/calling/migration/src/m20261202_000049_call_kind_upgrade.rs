//! C5 — upgrading a live voice call to video (`POST /v1/calls/{id}/upgrade`).
//!
//! * `calls.call_kind` is no longer frozen at creation: it may move VOICE → VIDEO exactly
//!   once, and only while the Call is live and answered (ANSWERED, CONNECTED or HELD) with
//!   its state unchanged by the same write. Every other change (VIDEO → VOICE, any change in
//!   a pre-answer, TRANSFERRING, ENDING or terminal Call) is refused by
//!   `calling_calls_guard_kind_change`. The version is untouched: it counts state
//!   transitions only (000005), so the kind change is evidenced by its own
//!   CALL_KIND_CHANGED event instead.
//! * `call_participant_media_permissions` stays pinned to its Call's kind: the composite
//!   foreign key now cascades the kind change (`ON UPDATE CASCADE`), its column guard lets
//!   `call_kind` follow VOICE → VIDEO only, and the CHECK that a VOICE row never carries a
//!   video publish permission is unchanged.
//! * `call_events` gains CALL_KIND_CHANGED; `call_commands` gains the UPGRADE operation
//!   (idempotent replays).
//!
//! Forward-only in spirit: DOWN restores the frozen kind, but a Call that was already
//! upgraded keeps its VIDEO kind.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
DROP TRIGGER trg_calls_immutable ON calls;
CREATE TRIGGER trg_calls_immutable
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_by_service_account_id',
        'created_by_credential_id', 'initiator_principal_id', 'requested_at', 'created_at');

-- VOICE -> VIDEO once, on an answered live Call, never together with a state change.
CREATE FUNCTION calling_calls_guard_kind_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.call_kind IS DISTINCT FROM OLD.call_kind
       AND NOT (OLD.call_kind = 'VOICE' AND NEW.call_kind = 'VIDEO'
                AND OLD.current_state IN ('ANSWERED', 'CONNECTED', 'HELD')
                AND NEW.current_state = OLD.current_state) THEN
        RAISE EXCEPTION 'calling invariant violated: call % kind % -> % is not allowed in %',
            OLD.id, OLD.call_kind, NEW.call_kind, OLD.current_state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_calls_guard_kind_change
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION calling_calls_guard_kind_change();

-- Authorized permissions follow their Call's kind (cascade), VOICE -> VIDEO only.
ALTER TABLE call_participant_media_permissions
    DROP CONSTRAINT fk_call_participant_media_permissions_call,
    ADD CONSTRAINT fk_call_participant_media_permissions_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, call_kind)
        REFERENCES calls (tenant_id, application_id, environment_id, id, call_kind)
        ON UPDATE CASCADE ON DELETE RESTRICT;
DROP TRIGGER trg_call_participant_media_permissions_immutable ON call_participant_media_permissions;
CREATE TRIGGER trg_call_participant_media_permissions_immutable
    BEFORE UPDATE ON call_participant_media_permissions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'participant_id', 'tenant_id', 'application_id', 'environment_id',
        'created_at');
CREATE FUNCTION calling_media_permissions_guard_kind_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.call_kind IS DISTINCT FROM OLD.call_kind
       AND NOT (OLD.call_kind = 'VOICE' AND NEW.call_kind = 'VIDEO') THEN
        RAISE EXCEPTION 'calling invariant violated: media permissions kind % -> % is not allowed',
            OLD.call_kind, NEW.call_kind
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_participant_media_permissions_kind
    BEFORE UPDATE ON call_participant_media_permissions
    FOR EACH ROW EXECUTE FUNCTION calling_media_permissions_guard_kind_change();

ALTER TABLE call_commands
    DROP CONSTRAINT ck_call_commands_operation,
    ADD CONSTRAINT ck_call_commands_operation CHECK (operation IN (
        'CREATE_CALL', 'ACCEPT', 'DECLINE', 'CANCEL', 'END', 'HOLD', 'RESUME', 'TRANSFER',
        'REMOVE_SESSION', 'UPGRADE'));

ALTER TABLE call_events
    DROP CONSTRAINT ck_call_events_type,
    ADD CONSTRAINT ck_call_events_type CHECK (event_type IN (
        'CALL_REQUESTED', 'CALL_AUTHORIZATION_RECORDED', 'CALL_STATE_CHANGED',
        'CALL_PARTICIPANT_ADDED', 'CALL_PARTICIPANT_UPDATED', 'CALL_PARTICIPANT_REMOVED',
        'CALL_SESSION_CREATED', 'CALL_SESSION_CONNECTED', 'CALL_SESSION_DISCONNECTED',
        'CALL_SESSION_RECONNECTED', 'CALL_SESSION_ENDED',
        'CALL_ROUTE_RECORDED', 'CALL_ROUTE_STATE_CHANGED',
        'CALL_LEG_RECORDED', 'CALL_LEG_STATE_CHANGED', 'CALL_LEG_BINDING_ASSIGNED',
        'CALL_ATTEMPT_RECORDED', 'CALL_ATTEMPT_FINISHED',
        'CALL_QUALITY_RECORDED', 'CALL_USAGE_RECORDED', 'CALL_USAGE_FINALIZED',
        'CALL_INVITATION_ISSUED', 'CALL_INVITATION_ACCEPTED', 'CALL_INVITATION_DECLINED',
        'CALL_INVITATION_ANSWERED_ELSEWHERE', 'CALL_INVITATION_CANCELLED', 'CALL_INVITATION_EXPIRED',
        'CALL_TRANSFER_REQUESTED', 'CALL_TRANSFER_COMPLETED', 'CALL_TRANSFER_FAILED',
        'CALL_TRANSFER_CANCELLED',
        'CALL_MEDIA_SESSION_CREATED', 'CALL_MEDIA_SESSION_TERMINATED',
        'CALL_MEDIA_PERMISSIONS_UPDATED', 'CALL_MEDIA_CREDENTIAL_ISSUED',
        'CALL_MEDIA_PARTICIPANT_EVICTED', 'CALL_MEDIA_ENFORCEMENT_PENDING', 'CALL_MEDIA_RECONCILED',
        'VOICEMAIL_RECEIVED', 'VOICEMAIL_READY', 'VOICEMAIL_DELETED',
        'RECORDING_STARTED', 'RECORDING_STOPPED', 'RECORDING_READY', 'RECORDING_FAILED',
        'RECORDING_DELETED',
        'ENTERPRISE_ROUTING_STEP',
        'CALL_KIND_CHANGED'));
"#;

const DOWN: &str = r#"
ALTER TABLE call_events
    DROP CONSTRAINT ck_call_events_type,
    ADD CONSTRAINT ck_call_events_type CHECK (event_type IN (
        'CALL_REQUESTED', 'CALL_AUTHORIZATION_RECORDED', 'CALL_STATE_CHANGED',
        'CALL_PARTICIPANT_ADDED', 'CALL_PARTICIPANT_UPDATED', 'CALL_PARTICIPANT_REMOVED',
        'CALL_SESSION_CREATED', 'CALL_SESSION_CONNECTED', 'CALL_SESSION_DISCONNECTED',
        'CALL_SESSION_RECONNECTED', 'CALL_SESSION_ENDED',
        'CALL_ROUTE_RECORDED', 'CALL_ROUTE_STATE_CHANGED',
        'CALL_LEG_RECORDED', 'CALL_LEG_STATE_CHANGED', 'CALL_LEG_BINDING_ASSIGNED',
        'CALL_ATTEMPT_RECORDED', 'CALL_ATTEMPT_FINISHED',
        'CALL_QUALITY_RECORDED', 'CALL_USAGE_RECORDED', 'CALL_USAGE_FINALIZED',
        'CALL_INVITATION_ISSUED', 'CALL_INVITATION_ACCEPTED', 'CALL_INVITATION_DECLINED',
        'CALL_INVITATION_ANSWERED_ELSEWHERE', 'CALL_INVITATION_CANCELLED', 'CALL_INVITATION_EXPIRED',
        'CALL_TRANSFER_REQUESTED', 'CALL_TRANSFER_COMPLETED', 'CALL_TRANSFER_FAILED',
        'CALL_TRANSFER_CANCELLED',
        'CALL_MEDIA_SESSION_CREATED', 'CALL_MEDIA_SESSION_TERMINATED',
        'CALL_MEDIA_PERMISSIONS_UPDATED', 'CALL_MEDIA_CREDENTIAL_ISSUED',
        'CALL_MEDIA_PARTICIPANT_EVICTED', 'CALL_MEDIA_ENFORCEMENT_PENDING', 'CALL_MEDIA_RECONCILED',
        'VOICEMAIL_RECEIVED', 'VOICEMAIL_READY', 'VOICEMAIL_DELETED',
        'RECORDING_STARTED', 'RECORDING_STOPPED', 'RECORDING_READY', 'RECORDING_FAILED',
        'RECORDING_DELETED',
        'ENTERPRISE_ROUTING_STEP'));
ALTER TABLE call_commands
    DROP CONSTRAINT ck_call_commands_operation,
    ADD CONSTRAINT ck_call_commands_operation CHECK (operation IN (
        'CREATE_CALL', 'ACCEPT', 'DECLINE', 'CANCEL', 'END', 'HOLD', 'RESUME', 'TRANSFER',
        'REMOVE_SESSION'));
DROP TRIGGER IF EXISTS trg_call_participant_media_permissions_kind ON call_participant_media_permissions;
DROP FUNCTION IF EXISTS calling_media_permissions_guard_kind_change();
DROP TRIGGER trg_call_participant_media_permissions_immutable ON call_participant_media_permissions;
CREATE TRIGGER trg_call_participant_media_permissions_immutable
    BEFORE UPDATE ON call_participant_media_permissions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'participant_id', 'tenant_id', 'application_id', 'environment_id',
        'call_kind', 'created_at');
ALTER TABLE call_participant_media_permissions
    DROP CONSTRAINT fk_call_participant_media_permissions_call,
    ADD CONSTRAINT fk_call_participant_media_permissions_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, call_kind)
        REFERENCES calls (tenant_id, application_id, environment_id, id, call_kind) ON DELETE RESTRICT;
DROP TRIGGER IF EXISTS trg_calls_guard_kind_change ON calls;
DROP FUNCTION IF EXISTS calling_calls_guard_kind_change();
DROP TRIGGER trg_calls_immutable ON calls;
CREATE TRIGGER trg_calls_immutable
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_by_service_account_id',
        'created_by_credential_id', 'initiator_principal_id', 'call_kind', 'requested_at', 'created_at');
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

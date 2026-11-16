//! C6 — screen sharing with server-side presenter state.
//!
//! * `call_participant_media_permissions.screen_share_started_at` — set while the
//!   participant is the Call's presenter. At most ONE presenter per Call
//!   (`uq_call_participant_media_permissions_presenter`, a partial unique index), so two
//!   concurrent starts can never both win. Only the presenter's provider grant includes the
//!   screen sources (SCREEN_SHARE, SCREEN_SHARE_AUDIO); HOLD masks them, and stopping,
//!   the presenter leaving or the Call ending clears the state (evidence:
//!   CALL_SCREEN_SHARE_STARTED / CALL_SCREEN_SHARE_STOPPED).
//! * `call_commands` gains START_SCREEN_SHARE / STOP_SCREEN_SHARE (idempotent replays).
//!
//! Additive: the column is nullable (existing rows: nobody presents).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE call_participant_media_permissions
    ADD COLUMN screen_share_started_at TIMESTAMPTZ,
    ADD CONSTRAINT ck_call_participant_media_permissions_presenter
        CHECK (screen_share_started_at IS NULL OR screen_share_started_at >= created_at);
CREATE UNIQUE INDEX uq_call_participant_media_permissions_presenter
    ON call_participant_media_permissions (call_id) WHERE screen_share_started_at IS NOT NULL;

ALTER TABLE call_commands
    DROP CONSTRAINT ck_call_commands_operation,
    ADD CONSTRAINT ck_call_commands_operation CHECK (operation IN (
        'CREATE_CALL', 'ACCEPT', 'DECLINE', 'CANCEL', 'END', 'HOLD', 'RESUME', 'TRANSFER',
        'REMOVE_SESSION', 'UPGRADE', 'START_SCREEN_SHARE', 'STOP_SCREEN_SHARE'));

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
        'CALL_KIND_CHANGED', 'CALL_SCREEN_SHARE_STARTED', 'CALL_SCREEN_SHARE_STOPPED'));
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
        'ENTERPRISE_ROUTING_STEP',
        'CALL_KIND_CHANGED'));

ALTER TABLE call_commands
    DROP CONSTRAINT ck_call_commands_operation,
    ADD CONSTRAINT ck_call_commands_operation CHECK (operation IN (
        'CREATE_CALL', 'ACCEPT', 'DECLINE', 'CANCEL', 'END', 'HOLD', 'RESUME', 'TRANSFER',
        'REMOVE_SESSION', 'UPGRADE'));
DROP INDEX IF EXISTS uq_call_participant_media_permissions_presenter;
ALTER TABLE call_participant_media_permissions
    DROP CONSTRAINT IF EXISTS ck_call_participant_media_permissions_presenter,
    DROP COLUMN IF EXISTS screen_share_started_at;
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

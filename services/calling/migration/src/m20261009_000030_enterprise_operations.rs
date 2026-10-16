//! Part F cross-cutting operations (§147, §168; ADR 0026).
//!
//! * `call_events` gains ENTERPRISE_ROUTING_STEP: every enterprise routing decision is a
//!   durable Call event published through the transactional outbox, in the same
//!   transaction as its `enterprise_routing_steps` evidence.
//! * `calling_incidents` — conditions an operator must look at, recorded with the same
//!   shape and discipline as Telephony's incidents (Part E §50); Calling never writes
//!   another service's database, so each service keeps its own.
//!
//! Incidents are:
//! * Deduplicated while OPEN by a stable key: a repeat bumps `occurrences` and
//!   `last_seen_at` (a repeated forwarding loop block is ONE open incident with a count).
//! * Categories are a closed vocabulary; evidence is bounded (≤ 4 KiB) and restricted:
//!   opaque ids and bounded codes only — never a telephone number, digit, object key, URL
//!   or provider identifier.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_incidents (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    dedupe_key          VARCHAR(160) NOT NULL,
    category            TEXT         NOT NULL,
    severity            TEXT         NOT NULL,
    status              TEXT         NOT NULL,
    call_id             UUID,
    resource_type       TEXT,
    resource_id         UUID,
    occurrences         INTEGER      NOT NULL,
    restricted_evidence JSONB        NOT NULL,
    first_seen_at       TIMESTAMPTZ  NOT NULL,
    last_seen_at        TIMESTAMPTZ  NOT NULL,
    resolved_at         TIMESTAMPTZ,
    CONSTRAINT pk_calling_incidents PRIMARY KEY (id),
    CONSTRAINT fk_calling_incidents_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_calling_incidents_key CHECK (dedupe_key ~ '^[a-z][a-z0-9:_-]{2,159}$'),
    CONSTRAINT ck_calling_incidents_category CHECK (category IN (
        'STUCK_QUEUE_ENTRY', 'QUEUE_ASSIGNMENT_CONFLICT', 'IVR_ROUTING_CORRUPTION',
        'VOICEMAIL_FINALIZATION_FAILURE', 'RECORDING_ARTIFACT_FAILURE', 'FORWARDING_LOOP_REPEATED')),
    CONSTRAINT ck_calling_incidents_severity CHECK (severity IN ('LOW', 'MEDIUM', 'HIGH', 'CRITICAL')),
    CONSTRAINT ck_calling_incidents_status CHECK (status IN ('OPEN', 'RESOLVED')),
    CONSTRAINT ck_calling_incidents_resolved CHECK ((status = 'RESOLVED') = (resolved_at IS NOT NULL)),
    CONSTRAINT ck_calling_incidents_resource_type CHECK (resource_type IS NULL OR resource_type IN (
        'QUEUE_ENTRY', 'IVR_SESSION', 'VOICEMAIL_MESSAGE', 'CALL_RECORDING', 'DESTINATION')),
    CONSTRAINT ck_calling_incidents_resource CHECK ((resource_type IS NULL) = (resource_id IS NULL)),
    CONSTRAINT ck_calling_incidents_occurrences CHECK (occurrences >= 1),
    CONSTRAINT ck_calling_incidents_evidence CHECK (
        jsonb_typeof(restricted_evidence) = 'object' AND octet_length(restricted_evidence::text) <= 4096),
    CONSTRAINT ck_calling_incidents_seen CHECK (last_seen_at >= first_seen_at)
);
CREATE UNIQUE INDEX uq_calling_incidents_open_key ON calling_incidents (dedupe_key) WHERE status = 'OPEN';
CREATE INDEX idx_calling_incidents_scope
    ON calling_incidents (tenant_id, application_id, environment_id, status, last_seen_at DESC);
CREATE INDEX idx_calling_incidents_category ON calling_incidents (category, status);
CREATE TRIGGER trg_calling_incidents_immutable
    BEFORE UPDATE ON calling_incidents
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'dedupe_key', 'category',
        'call_id', 'resource_type', 'resource_id', 'first_seen_at');

-- §147: enterprise routing decisions are durable Call facts (outbox).
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
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS calling_incidents;
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
        'RECORDING_DELETED'));
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

//! Provider callback evidence, the §20–§25 event vocabulary and endpoint actors
//! (Part C §20–§25).
//!
//! * `call_media_provider_events` — Calling-local, operational record of authenticated
//!   media-provider callbacks: the provider's event id (dedupe key, UNIQUE per adapter),
//!   the normalized event kind, the Call / media binding / session it was safely resolved
//!   to (or none), whether the provider identity was known, and the processing outcome.
//!   No raw provider payload is stored. Callbacks are evidence only — they never create
//!   participants, authorizations or permissions. A callback is first CLAIMED (the unique
//!   key makes a duplicate delivery a no-op), then completed once with its outcome; rows
//!   may be pruned by retention.
//! * `call_events.event_type` gains the facts §20–§25 produce (invitations, transfers,
//!   media binding, media permissions, credential issuance audit, evictions, removals,
//!   pending provider enforcement and reconciliation).
//! * `call_events` can attribute a mutation to an application endpoint acting through a
//!   short-lived signaling credential (`APPLICATION_ENDPOINT`): the endpoint and the API
//!   credential that minted the signaling credential are both recorded.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Provider callback evidence.
-- ---------------------------------------------------------------------------
CREATE TABLE call_media_provider_events (
    id                UUID PRIMARY KEY,
    provider_adapter  VARCHAR(32)  NOT NULL,
    provider_event_id VARCHAR(128) NOT NULL,
    event_kind        TEXT         NOT NULL,
    call_id           UUID,
    media_session_id  UUID,
    session_id        UUID,
    tenant_id         UUID,
    application_id    UUID,
    environment_id    UUID,
    identity_status   TEXT         NOT NULL,
    -- NULL while claimed and being processed; set once with the processing outcome.
    outcome           TEXT,
    occurred_at       TIMESTAMPTZ,
    received_at       TIMESTAMPTZ  NOT NULL,
    processed_at      TIMESTAMPTZ,
    CONSTRAINT fk_call_media_provider_events_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_media_provider_events_session
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, session_id)
        REFERENCES call_sessions (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_media_provider_events_media_session
        FOREIGN KEY (media_session_id) REFERENCES call_media_sessions (id) ON DELETE RESTRICT,
    -- Dedupe: one record per provider event.
    CONSTRAINT uq_call_media_provider_events_dedupe UNIQUE (provider_adapter, provider_event_id),
    CONSTRAINT ck_call_media_provider_events_adapter CHECK (provider_adapter ~ '^[a-z][a-z0-9_-]{1,31}$'),
    CONSTRAINT ck_call_media_provider_events_event_id CHECK (provider_event_id ~ '^[A-Za-z0-9_.:-]{1,128}$'),
    CONSTRAINT ck_call_media_provider_events_kind CHECK (event_kind IN (
        'RESOURCE_STARTED', 'RESOURCE_FINISHED', 'PARTICIPANT_JOINED', 'PARTICIPANT_LEFT',
        'PARTICIPANT_CONNECTION_ABORTED', 'TRACK_PUBLISHED', 'TRACK_UNPUBLISHED', 'OTHER')),
    CONSTRAINT ck_call_media_provider_events_scope CHECK (
        (call_id IS NULL) = (tenant_id IS NULL)
        AND (tenant_id IS NULL) = (application_id IS NULL)
        AND (application_id IS NULL) = (environment_id IS NULL)
        AND (session_id IS NULL OR call_id IS NOT NULL)
        AND (media_session_id IS NULL OR call_id IS NOT NULL)),
    CONSTRAINT ck_call_media_provider_events_identity CHECK (identity_status IN ('NONE', 'KNOWN', 'UNKNOWN')),
    CONSTRAINT ck_call_media_provider_events_outcome CHECK (outcome IS NULL OR outcome IN (
        'APPLIED', 'NO_OP', 'IGNORED_TERMINAL', 'QUARANTINED', 'EVICTED')),
    CONSTRAINT ck_call_media_provider_events_processed CHECK ((outcome IS NULL) = (processed_at IS NULL)),
    CONSTRAINT ck_call_media_provider_events_timestamps CHECK (processed_at IS NULL OR processed_at >= received_at)
);
CREATE INDEX idx_call_media_provider_events_call ON call_media_provider_events (call_id);
CREATE INDEX idx_call_media_provider_events_received ON call_media_provider_events (received_at);
-- The claimed record is completed once with the processing result; nothing else changes.
CREATE TRIGGER trg_call_media_provider_events_immutable
    BEFORE UPDATE ON call_media_provider_events
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'provider_adapter', 'provider_event_id', 'event_kind', 'occurred_at', 'received_at');
CREATE TRIGGER trg_call_media_provider_events_set_once
    BEFORE UPDATE ON call_media_provider_events
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'call_id', 'media_session_id', 'session_id', 'tenant_id', 'application_id',
        'environment_id', 'outcome', 'processed_at');

-- ---------------------------------------------------------------------------
-- Event vocabulary of §20–§25.
-- ---------------------------------------------------------------------------
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
        'CALL_MEDIA_PARTICIPANT_EVICTED', 'CALL_MEDIA_ENFORCEMENT_PENDING', 'CALL_MEDIA_RECONCILED'));

-- ---------------------------------------------------------------------------
-- Endpoint actors.
-- ---------------------------------------------------------------------------
ALTER TABLE call_events
    ADD COLUMN actor_endpoint_id UUID,
    ADD CONSTRAINT fk_call_events_actor_endpoint
        FOREIGN KEY (tenant_id, application_id, environment_id, actor_endpoint_id)
        REFERENCES call_endpoints (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    DROP CONSTRAINT ck_call_events_actor,
    ADD CONSTRAINT ck_call_events_actor CHECK (
        (actor_kind = 'CUSTOMER_APPLICATION' AND actor_credential_id IS NOT NULL
            AND actor_service IS NULL AND actor_endpoint_id IS NULL)
        OR (actor_kind = 'APPLICATION_ENDPOINT' AND actor_credential_id IS NOT NULL
            AND actor_service IS NULL AND actor_endpoint_id IS NOT NULL)
        OR (actor_kind = 'INTERNAL_SERVICE' AND actor_credential_id IS NULL AND actor_endpoint_id IS NULL
            AND actor_service ~ '^cfmeet\.[a-z][a-z0-9]*(-[a-z0-9]+)*$'));
"#;

const DOWN: &str = r#"
ALTER TABLE call_events
    DROP CONSTRAINT IF EXISTS ck_call_events_actor,
    DROP CONSTRAINT IF EXISTS fk_call_events_actor_endpoint,
    DROP COLUMN IF EXISTS actor_endpoint_id,
    ADD CONSTRAINT ck_call_events_actor CHECK (
        (actor_kind = 'CUSTOMER_APPLICATION' AND actor_credential_id IS NOT NULL AND actor_service IS NULL)
        OR (actor_kind = 'INTERNAL_SERVICE' AND actor_credential_id IS NULL
            AND actor_service ~ '^cfmeet\.[a-z][a-z0-9]*(-[a-z0-9]+)*$'));
ALTER TABLE call_events
    DROP CONSTRAINT IF EXISTS ck_call_events_type,
    ADD CONSTRAINT ck_call_events_type CHECK (event_type IN (
        'CALL_REQUESTED', 'CALL_AUTHORIZATION_RECORDED', 'CALL_STATE_CHANGED',
        'CALL_PARTICIPANT_ADDED', 'CALL_PARTICIPANT_UPDATED',
        'CALL_SESSION_CREATED', 'CALL_SESSION_CONNECTED', 'CALL_SESSION_DISCONNECTED',
        'CALL_SESSION_RECONNECTED', 'CALL_SESSION_ENDED',
        'CALL_ROUTE_RECORDED', 'CALL_ROUTE_STATE_CHANGED',
        'CALL_LEG_RECORDED', 'CALL_LEG_STATE_CHANGED', 'CALL_LEG_BINDING_ASSIGNED',
        'CALL_ATTEMPT_RECORDED', 'CALL_ATTEMPT_FINISHED',
        'CALL_QUALITY_RECORDED', 'CALL_USAGE_RECORDED', 'CALL_USAGE_FINALIZED'));
DROP TABLE IF EXISTS call_media_provider_events;
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

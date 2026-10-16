//! Voicemail (Part F §60, §100–§113).
//!
//! * `voicemail_mailboxes` — a mailbox owned by a principal, an extension, a queue or a
//!   ring group of the scope (guarded destination), with its maximum message length and
//!   retention. At most one ACTIVE mailbox per owner.
//! * `voicemail_messages` — one message of ONE originating canonical Call (never a
//!   second Call): the caller's principal (never a number), the capture source, the
//!   protected artifact's opaque object key and integrity metadata (content type, size,
//!   duration, SHA-256, encryption), the processing state RECORDING → PROCESSING →
//!   READY (or FAILED) → DELETING → DELETED, the read state and the retention deadline.
//!   Rows are history: never deleted; DELETED clears the object reference.
//! * `call_telephony_commands` gains CAPTURE (provider-neutral edge media capture).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE voicemail_mailboxes (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    owner_type          TEXT         NOT NULL,
    owner_id            UUID         NOT NULL,
    display_label       VARCHAR(100) NOT NULL,
    max_message_seconds INTEGER      NOT NULL,
    retention_days      INTEGER      NOT NULL,
    status              TEXT         NOT NULL,
    version             BIGINT       NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    disabled_at         TIMESTAMPTZ,
    CONSTRAINT pk_voicemail_mailboxes PRIMARY KEY (id),
    CONSTRAINT uq_voicemail_mailboxes_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_voicemail_mailboxes_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_voicemail_mailboxes_owner_type
        CHECK (owner_type IN ('PRINCIPAL', 'EXTENSION', 'QUEUE', 'RING_GROUP')),
    CONSTRAINT ck_voicemail_mailboxes_label CHECK (length(btrim(display_label)) BETWEEN 1 AND 100),
    CONSTRAINT ck_voicemail_mailboxes_max_seconds CHECK (max_message_seconds BETWEEN 5 AND 600),
    CONSTRAINT ck_voicemail_mailboxes_retention CHECK (retention_days BETWEEN 1 AND 3650),
    CONSTRAINT ck_voicemail_mailboxes_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_voicemail_mailboxes_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_voicemail_mailboxes_version CHECK (version >= 1),
    CONSTRAINT ck_voicemail_mailboxes_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_voicemail_mailboxes_active_owner
    ON voicemail_mailboxes (owner_type, owner_id) WHERE status = 'ACTIVE';
CREATE INDEX idx_voicemail_mailboxes_scope
    ON voicemail_mailboxes (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_voicemail_mailboxes_immutable
    BEFORE UPDATE ON voicemail_mailboxes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'owner_type', 'owner_id', 'created_at');
CREATE TRIGGER trg_voicemail_mailboxes_version
    BEFORE UPDATE ON voicemail_mailboxes
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_voicemail_mailboxes_owner
    BEFORE INSERT OR UPDATE ON voicemail_mailboxes
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('owner_type', 'owner_id');

CREATE TABLE voicemail_messages (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    mailbox_id          UUID         NOT NULL,
    call_id             UUID         NOT NULL,
    caller_principal_id UUID         NOT NULL,
    capture_source      TEXT         NOT NULL,
    call_leg_id         UUID,
    state               TEXT         NOT NULL,
    read_state          TEXT         NOT NULL,
    object_key          VARCHAR(512),
    content_type        VARCHAR(64),
    size_bytes          BIGINT,
    duration_ms         INTEGER,
    sha256              CHAR(64),
    encrypted           BOOLEAN,
    received_at         TIMESTAMPTZ  NOT NULL,
    recording_deadline  TIMESTAMPTZ  NOT NULL,
    ready_at            TIMESTAMPTZ,
    read_at             TIMESTAMPTZ,
    failed_at           TIMESTAMPTZ,
    failure_code        VARCHAR(64),
    retention_until     TIMESTAMPTZ,
    deleting_at         TIMESTAMPTZ,
    deleted_at          TIMESTAMPTZ,
    delete_reason       TEXT,
    delete_attempts     INTEGER      NOT NULL,
    next_attempt_at     TIMESTAMPTZ,
    version             BIGINT       NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_voicemail_messages PRIMARY KEY (id),
    CONSTRAINT uq_voicemail_messages_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    -- One originating Call reaches voicemail at most once.
    CONSTRAINT uq_voicemail_messages_call UNIQUE (call_id),
    CONSTRAINT fk_voicemail_messages_mailbox
        FOREIGN KEY (tenant_id, application_id, environment_id, mailbox_id)
        REFERENCES voicemail_mailboxes (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_voicemail_messages_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_voicemail_messages_caller
        FOREIGN KEY (tenant_id, application_id, environment_id, caller_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_voicemail_messages_source CHECK (capture_source IN ('SIP', 'APPLICATION')),
    CONSTRAINT ck_voicemail_messages_state CHECK (state IN (
        'RECORDING', 'PROCESSING', 'READY', 'FAILED', 'DELETING', 'DELETED')),
    CONSTRAINT ck_voicemail_messages_read CHECK (read_state IN ('UNREAD', 'READ')),
    CONSTRAINT ck_voicemail_messages_read_at CHECK ((read_state = 'READ') = (read_at IS NOT NULL)),
    CONSTRAINT ck_voicemail_messages_ready CHECK (state <> 'READY' OR (
        object_key IS NOT NULL AND content_type IS NOT NULL AND size_bytes IS NOT NULL
        AND sha256 IS NOT NULL AND ready_at IS NOT NULL AND retention_until IS NOT NULL)),
    CONSTRAINT ck_voicemail_messages_deleted CHECK (state <> 'DELETED' OR (
        deleted_at IS NOT NULL AND object_key IS NULL)),
    CONSTRAINT ck_voicemail_messages_deleting CHECK (
        (state IN ('DELETING', 'DELETED')) = (deleting_at IS NOT NULL AND delete_reason IS NOT NULL)),
    CONSTRAINT ck_voicemail_messages_delete_reason
        CHECK (delete_reason IS NULL OR delete_reason IN ('USER', 'ADMIN', 'RETENTION')),
    CONSTRAINT ck_voicemail_messages_failed CHECK ((state = 'FAILED') = (failed_at IS NOT NULL)
        OR state IN ('DELETING', 'DELETED')),
    CONSTRAINT ck_voicemail_messages_key CHECK (object_key IS NULL
        OR (length(object_key) <= 512 AND object_key ~ '^[A-Za-z0-9][A-Za-z0-9/_.-]*$')),
    CONSTRAINT ck_voicemail_messages_content_type CHECK (content_type IS NULL
        OR content_type IN ('audio/wav', 'audio/ogg', 'audio/mpeg')),
    CONSTRAINT ck_voicemail_messages_size CHECK (size_bytes IS NULL OR size_bytes BETWEEN 1 AND 134217728),
    CONSTRAINT ck_voicemail_messages_duration CHECK (duration_ms IS NULL OR duration_ms BETWEEN 0 AND 3600000),
    CONSTRAINT ck_voicemail_messages_sha256 CHECK (sha256 IS NULL OR sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_voicemail_messages_failure CHECK (failure_code IS NULL OR failure_code ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_voicemail_messages_attempts CHECK (delete_attempts >= 0),
    CONSTRAINT ck_voicemail_messages_version CHECK (version >= 1),
    CONSTRAINT ck_voicemail_messages_timestamps CHECK (updated_at >= created_at
        AND recording_deadline > received_at)
);
CREATE INDEX idx_voicemail_messages_mailbox
    ON voicemail_messages (mailbox_id, received_at DESC, id DESC);
CREATE INDEX idx_voicemail_messages_recording
    ON voicemail_messages (recording_deadline) WHERE state IN ('RECORDING', 'PROCESSING');
CREATE INDEX idx_voicemail_messages_retention
    ON voicemail_messages (retention_until) WHERE state = 'READY';
CREATE INDEX idx_voicemail_messages_deleting
    ON voicemail_messages (next_attempt_at) WHERE state = 'DELETING';

-- The processing lifecycle (also enforced by the service): DELETED is final.
CREATE FUNCTION voicemail_guard_transition() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state IS DISTINCT FROM OLD.state AND NOT (
        (OLD.state = 'RECORDING' AND NEW.state IN ('PROCESSING', 'READY', 'FAILED', 'DELETING'))
        OR (OLD.state = 'PROCESSING' AND NEW.state IN ('READY', 'FAILED', 'DELETING'))
        OR (OLD.state = 'READY' AND NEW.state = 'DELETING')
        OR (OLD.state = 'FAILED' AND NEW.state IN ('READY', 'DELETING'))
        OR (OLD.state = 'DELETING' AND NEW.state = 'DELETED')) THEN
        RAISE EXCEPTION 'voicemail invariant violated: % -> % is not allowed', OLD.state, NEW.state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_voicemail_messages_transition
    BEFORE UPDATE ON voicemail_messages
    FOR EACH ROW EXECUTE FUNCTION voicemail_guard_transition();
CREATE TRIGGER trg_voicemail_messages_immutable
    BEFORE UPDATE ON voicemail_messages
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'mailbox_id', 'call_id',
        'caller_principal_id', 'capture_source', 'received_at', 'created_at');
CREATE TRIGGER trg_voicemail_messages_version
    BEFORE UPDATE ON voicemail_messages
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_voicemail_messages_no_delete
    BEFORE DELETE ON voicemail_messages
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

CREATE OR REPLACE FUNCTION enterprise_destination_exists(
    t UUID, a UUID, e UUID, kind TEXT, target UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'PRINCIPAL' THEN EXISTS (SELECT 1 FROM platform_principals p
            WHERE p.tenant_id = t AND p.application_id = a AND p.environment_id = e AND p.id = target)
        WHEN 'ENTERPRISE_MEMBER' THEN EXISTS (SELECT 1 FROM platform_principals p
            WHERE p.tenant_id = t AND p.application_id = a AND p.environment_id = e AND p.id = target
              AND p.kind = 'ENTERPRISE_MEMBER')
        WHEN 'EXTENSION' THEN EXISTS (SELECT 1 FROM enterprise_extensions x
            WHERE x.tenant_id = t AND x.application_id = a AND x.environment_id = e AND x.id = target)
        WHEN 'RING_GROUP' THEN EXISTS (SELECT 1 FROM enterprise_ring_groups g
            WHERE g.tenant_id = t AND g.application_id = a AND g.environment_id = e AND g.id = target)
        WHEN 'QUEUE' THEN EXISTS (SELECT 1 FROM enterprise_call_queues q
            WHERE q.tenant_id = t AND q.application_id = a AND q.environment_id = e AND q.id = target)
        WHEN 'IVR' THEN EXISTS (SELECT 1 FROM enterprise_ivr_flows f
            WHERE f.tenant_id = t AND f.application_id = a AND f.environment_id = e AND f.id = target)
        WHEN 'SCHEDULE' THEN EXISTS (SELECT 1 FROM enterprise_business_hours h
            WHERE h.tenant_id = t AND h.application_id = a AND h.environment_id = e AND h.id = target)
        WHEN 'PUBLIC_CONTACT' THEN EXISTS (SELECT 1 FROM enterprise_public_contacts c
            WHERE c.tenant_id = t AND c.application_id = a AND c.environment_id = e AND c.id = target)
        WHEN 'VOICEMAIL' THEN EXISTS (SELECT 1 FROM voicemail_mailboxes m
            WHERE m.tenant_id = t AND m.application_id = a AND m.environment_id = e AND m.id = target)
        ELSE FALSE
    END
$$;

ALTER TABLE call_telephony_commands
    DROP CONSTRAINT ck_call_telephony_commands_kind,
    DROP CONSTRAINT ck_call_telephony_commands_target,
    ADD CONSTRAINT ck_call_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'ANSWER', 'PROMPT',
        'CAPTURE')),
    ADD CONSTRAINT ck_call_telephony_commands_target CHECK (
        (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'ANSWER', 'PROMPT', 'CAPTURE')
            AND call_leg_id IS NOT NULL)
        OR (kind IN ('BRIDGE', 'UNBRIDGE') AND bridge_id IS NOT NULL));

-- Part F §111: voicemail facts of the originating Call (published through the outbox).
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
        'VOICEMAIL_RECEIVED', 'VOICEMAIL_READY', 'VOICEMAIL_DELETED'));
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
        'CALL_MEDIA_PARTICIPANT_EVICTED', 'CALL_MEDIA_ENFORCEMENT_PENDING', 'CALL_MEDIA_RECONCILED'));
ALTER TABLE call_telephony_commands
    DROP CONSTRAINT ck_call_telephony_commands_kind,
    DROP CONSTRAINT ck_call_telephony_commands_target,
    ADD CONSTRAINT ck_call_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'ANSWER', 'PROMPT')),
    ADD CONSTRAINT ck_call_telephony_commands_target CHECK (
        (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'ANSWER', 'PROMPT')
            AND call_leg_id IS NOT NULL)
        OR (kind IN ('BRIDGE', 'UNBRIDGE') AND bridge_id IS NOT NULL));
CREATE OR REPLACE FUNCTION enterprise_destination_exists(
    t UUID, a UUID, e UUID, kind TEXT, target UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'PRINCIPAL' THEN EXISTS (SELECT 1 FROM platform_principals p
            WHERE p.tenant_id = t AND p.application_id = a AND p.environment_id = e AND p.id = target)
        WHEN 'ENTERPRISE_MEMBER' THEN EXISTS (SELECT 1 FROM platform_principals p
            WHERE p.tenant_id = t AND p.application_id = a AND p.environment_id = e AND p.id = target
              AND p.kind = 'ENTERPRISE_MEMBER')
        WHEN 'EXTENSION' THEN EXISTS (SELECT 1 FROM enterprise_extensions x
            WHERE x.tenant_id = t AND x.application_id = a AND x.environment_id = e AND x.id = target)
        WHEN 'RING_GROUP' THEN EXISTS (SELECT 1 FROM enterprise_ring_groups g
            WHERE g.tenant_id = t AND g.application_id = a AND g.environment_id = e AND g.id = target)
        WHEN 'QUEUE' THEN EXISTS (SELECT 1 FROM enterprise_call_queues q
            WHERE q.tenant_id = t AND q.application_id = a AND q.environment_id = e AND q.id = target)
        WHEN 'IVR' THEN EXISTS (SELECT 1 FROM enterprise_ivr_flows f
            WHERE f.tenant_id = t AND f.application_id = a AND f.environment_id = e AND f.id = target)
        WHEN 'SCHEDULE' THEN EXISTS (SELECT 1 FROM enterprise_business_hours h
            WHERE h.tenant_id = t AND h.application_id = a AND h.environment_id = e AND h.id = target)
        WHEN 'PUBLIC_CONTACT' THEN EXISTS (SELECT 1 FROM enterprise_public_contacts c
            WHERE c.tenant_id = t AND c.application_id = a AND c.environment_id = e AND c.id = target)
        ELSE FALSE
    END
$$;
DROP TABLE IF EXISTS voicemail_messages;
DROP FUNCTION IF EXISTS voicemail_guard_transition();
DROP TABLE IF EXISTS voicemail_mailboxes;
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

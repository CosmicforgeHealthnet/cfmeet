//! Call recording policy (Part F §61, §115–§129; ADR 0024).
//!
//! Recording authority is SEPARATE from the right to place a Call, from enterprise
//! membership, from Privacy Relay authorization and from any meeting recording policy.
//!
//! * `call_recording_policies` — a typed, versioned organization policy of the scope:
//!   mode (DISABLED / MANUAL_ALLOWED / AUTO_WHEN_AUTHORIZED — never assumed), notice
//!   requirement, consent requirement and which participants must consent, an opaque
//!   jurisdiction reference (compliance configuration is authoritative input; nothing is
//!   inferred from a country), retention and a maximum length. One default per scope.
//! * `call_recording_consents` — one participant's consent decision for ONE Call, taken
//!   through that participant's own endpoint (never a client "everyone consented" flag).
//!   Append-only; the latest decision per participant counts.
//! * `call_recording_authorizations` — the immutable, server-authoritative decision taken
//!   before any capture: Call, policy version, actor, required participants, notice and
//!   consent evidence, ALLOWED / DENIED and a bounded reason.
//! * `call_recordings` — the recording of a Call: capture source, protected artifact
//!   reference + integrity metadata (never a URL), STARTING → RECORDING → STOPPING →
//!   PROCESSING → READY (or FAILED) → DELETING → DELETED, the stop reason, retention.
//!   At most one active recording per Call. Rows are never deleted.
//! * `call_recording_access_audit` — every retrieval / mutation attempt on a recording
//!   (actor, operation, outcome incl. denials, request / correlation), append-only.
//! * `call_telephony_commands` gains CAPTURE_STOP; `call_events` the RECORDING_* facts.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE call_recording_policies (
    id                    UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    name                  VARCHAR(100) NOT NULL,
    mode                  TEXT         NOT NULL,
    notice_required       BOOLEAN      NOT NULL,
    consent_required      BOOLEAN      NOT NULL,
    consent_participants  TEXT         NOT NULL,
    jurisdiction_ref      VARCHAR(64)  NOT NULL,
    retention_days        INTEGER      NOT NULL,
    max_recording_seconds INTEGER      NOT NULL,
    is_default            BOOLEAN      NOT NULL,
    status                TEXT         NOT NULL,
    version               BIGINT       NOT NULL,
    created_at            TIMESTAMPTZ  NOT NULL,
    updated_at            TIMESTAMPTZ  NOT NULL,
    disabled_at           TIMESTAMPTZ,
    CONSTRAINT pk_call_recording_policies PRIMARY KEY (id),
    CONSTRAINT uq_call_recording_policies_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_call_recording_policies_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_call_recording_policies_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_call_recording_policies_mode
        CHECK (mode IN ('DISABLED', 'MANUAL_ALLOWED', 'AUTO_WHEN_AUTHORIZED')),
    CONSTRAINT ck_call_recording_policies_consent_participants
        CHECK (consent_participants IN ('NONE', 'ALL_PARTICIPANTS', 'OTHER_PARTICIPANTS')),
    -- Consent required <=> somebody must consent.
    CONSTRAINT ck_call_recording_policies_consent
        CHECK (consent_required = (consent_participants <> 'NONE')),
    CONSTRAINT ck_call_recording_policies_jurisdiction
        CHECK (jurisdiction_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:/-]*$' AND length(jurisdiction_ref) <= 64),
    CONSTRAINT ck_call_recording_policies_retention CHECK (retention_days BETWEEN 1 AND 3650),
    CONSTRAINT ck_call_recording_policies_max CHECK (max_recording_seconds BETWEEN 60 AND 1440),
    CONSTRAINT ck_call_recording_policies_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_call_recording_policies_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_call_recording_policies_version CHECK (version >= 1),
    CONSTRAINT ck_call_recording_policies_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_call_recording_policies_default
    ON call_recording_policies (tenant_id, application_id, environment_id)
    WHERE is_default AND status = 'ACTIVE';
CREATE UNIQUE INDEX uq_call_recording_policies_active_name
    ON call_recording_policies (tenant_id, application_id, environment_id, lower(name))
    WHERE status = 'ACTIVE';
CREATE INDEX idx_call_recording_policies_scope
    ON call_recording_policies (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_call_recording_policies_immutable
    BEFORE UPDATE ON call_recording_policies
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_call_recording_policies_version
    BEFORE UPDATE ON call_recording_policies
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_call_recording_policies_no_delete
    BEFORE DELETE ON call_recording_policies
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

CREATE TABLE call_recording_consents (
    id             UUID        NOT NULL,
    seq            BIGSERIAL   NOT NULL,
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    call_id        UUID        NOT NULL,
    participant_id UUID        NOT NULL,
    principal_id   UUID        NOT NULL,
    endpoint_id    UUID        NOT NULL,
    decision       TEXT        NOT NULL,
    request_id     VARCHAR(36) NOT NULL,
    correlation_id VARCHAR(37) NOT NULL,
    recorded_at    TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_call_recording_consents PRIMARY KEY (id),
    CONSTRAINT fk_call_recording_consents_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_call_recording_consents_decision CHECK (decision IN ('GRANTED', 'DECLINED'))
);
-- The latest decision per participant is the one with the highest sequence.
CREATE INDEX idx_call_recording_consents_call
    ON call_recording_consents (call_id, participant_id, seq DESC);
CREATE TRIGGER trg_call_recording_consents_append_only
    BEFORE UPDATE OR DELETE ON call_recording_consents
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

CREATE TABLE call_recording_authorizations (
    id                          UUID        NOT NULL,
    seq                         BIGSERIAL   NOT NULL,
    tenant_id                   UUID        NOT NULL,
    application_id              UUID        NOT NULL,
    environment_id              UUID        NOT NULL,
    call_id                     UUID        NOT NULL,
    policy_id                   UUID,
    policy_version              BIGINT,
    mode                        TEXT        NOT NULL,
    jurisdiction_ref            VARCHAR(64),
    actor_kind                  TEXT        NOT NULL,
    actor_principal_id          UUID,
    actor_credential_id         UUID,
    required_participant_ids    UUID[]      NOT NULL,
    notice_evidence             JSONB       NOT NULL,
    consent_evidence            JSONB       NOT NULL,
    -- The highest consent sequence of the Call this decision considered.
    consent_watermark           BIGINT      NOT NULL,
    decision                    TEXT        NOT NULL,
    reason                      VARCHAR(64) NOT NULL,
    request_id                  VARCHAR(36) NOT NULL,
    correlation_id              VARCHAR(37) NOT NULL,
    created_at                  TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_call_recording_authorizations PRIMARY KEY (id),
    CONSTRAINT uq_call_recording_authorizations_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_call_recording_authorizations_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_recording_authorizations_policy
        FOREIGN KEY (tenant_id, application_id, environment_id, policy_id)
        REFERENCES call_recording_policies (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_call_recording_authorizations_mode
        CHECK (mode IN ('NONE', 'DISABLED', 'MANUAL_ALLOWED', 'AUTO_WHEN_AUTHORIZED')),
    CONSTRAINT ck_call_recording_authorizations_actor CHECK (
        (actor_kind = 'PRINCIPAL' AND actor_principal_id IS NOT NULL)
        OR (actor_kind = 'ADMINISTRATOR' AND actor_principal_id IS NULL AND actor_credential_id IS NOT NULL)
        OR (actor_kind = 'SYSTEM' AND actor_principal_id IS NULL)),
    CONSTRAINT ck_call_recording_authorizations_decision CHECK (decision IN ('ALLOWED', 'DENIED')),
    CONSTRAINT ck_call_recording_authorizations_reason CHECK (reason ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_call_recording_authorizations_allowed CHECK (
        decision = 'DENIED' OR (policy_id IS NOT NULL AND mode IN ('MANUAL_ALLOWED', 'AUTO_WHEN_AUTHORIZED'))),
    CONSTRAINT ck_call_recording_authorizations_evidence CHECK (
        jsonb_typeof(notice_evidence) = 'object' AND jsonb_typeof(consent_evidence) = 'object')
);
CREATE INDEX idx_call_recording_authorizations_call
    ON call_recording_authorizations (call_id, seq);
CREATE TRIGGER trg_call_recording_authorizations_immutable
    BEFORE UPDATE OR DELETE ON call_recording_authorizations
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

CREATE TABLE call_recordings (
    id                 UUID         NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    call_id            UUID         NOT NULL,
    authorization_id   UUID         NOT NULL,
    policy_id          UUID         NOT NULL,
    capture_source     TEXT         NOT NULL,
    call_leg_id        UUID,
    state              TEXT         NOT NULL,
    started_at         TIMESTAMPTZ  NOT NULL,
    recording_deadline TIMESTAMPTZ  NOT NULL,
    stop_requested_at  TIMESTAMPTZ,
    stopped_at         TIMESTAMPTZ,
    stop_reason        TEXT,
    object_key         VARCHAR(512),
    content_type       VARCHAR(64),
    size_bytes         BIGINT,
    duration_ms        INTEGER,
    sha256             CHAR(64),
    encrypted          BOOLEAN,
    ready_at           TIMESTAMPTZ,
    failed_at          TIMESTAMPTZ,
    failure_code       VARCHAR(64),
    retention_until    TIMESTAMPTZ,
    deleting_at        TIMESTAMPTZ,
    deleted_at         TIMESTAMPTZ,
    delete_reason      TEXT,
    delete_attempts    INTEGER      NOT NULL,
    next_attempt_at    TIMESTAMPTZ,
    version            BIGINT       NOT NULL,
    created_at         TIMESTAMPTZ  NOT NULL,
    updated_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_call_recordings PRIMARY KEY (id),
    CONSTRAINT uq_call_recordings_owner_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_call_recordings_authorization UNIQUE (authorization_id),
    CONSTRAINT fk_call_recordings_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_recordings_authorization
        FOREIGN KEY (tenant_id, application_id, environment_id, authorization_id)
        REFERENCES call_recording_authorizations (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_recordings_policy
        FOREIGN KEY (tenant_id, application_id, environment_id, policy_id)
        REFERENCES call_recording_policies (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_call_recordings_source CHECK (capture_source IN ('SIP', 'APPLICATION')),
    CONSTRAINT ck_call_recordings_state CHECK (state IN (
        'STARTING', 'RECORDING', 'STOPPING', 'PROCESSING', 'READY', 'FAILED', 'DELETING', 'DELETED')),
    CONSTRAINT ck_call_recordings_stop_reason CHECK (stop_reason IS NULL OR stop_reason IN (
        'EXPLICIT', 'CALL_ENDED', 'POLICY_REVOKED', 'CONSENT_WITHDRAWN', 'CAPTURE_FAILED',
        'MAX_DURATION', 'KILL_SWITCH')),
    CONSTRAINT ck_call_recordings_key CHECK (object_key IS NULL
        OR (length(object_key) <= 512 AND object_key ~ '^[A-Za-z0-9][A-Za-z0-9/_.-]*$')),
    CONSTRAINT ck_call_recordings_content_type CHECK (content_type IS NULL
        OR content_type IN ('audio/wav', 'audio/ogg', 'audio/mpeg')),
    CONSTRAINT ck_call_recordings_size CHECK (size_bytes IS NULL OR size_bytes BETWEEN 1 AND 1073741824),
    CONSTRAINT ck_call_recordings_duration CHECK (duration_ms IS NULL OR duration_ms >= 0),
    CONSTRAINT ck_call_recordings_sha256 CHECK (sha256 IS NULL OR sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_call_recordings_failure CHECK (failure_code IS NULL OR failure_code ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_call_recordings_delete_reason
        CHECK (delete_reason IS NULL OR delete_reason IN ('USER', 'ADMIN', 'RETENTION')),
    CONSTRAINT ck_call_recordings_ready CHECK (state <> 'READY' OR (
        object_key IS NOT NULL AND content_type IS NOT NULL AND size_bytes IS NOT NULL
        AND sha256 IS NOT NULL AND ready_at IS NOT NULL AND retention_until IS NOT NULL)),
    CONSTRAINT ck_call_recordings_deleted CHECK (state <> 'DELETED' OR (
        deleted_at IS NOT NULL AND object_key IS NULL)),
    CONSTRAINT ck_call_recordings_failed CHECK ((state = 'FAILED') = (failed_at IS NOT NULL)
        OR state IN ('DELETING', 'DELETED')),
    CONSTRAINT ck_call_recordings_attempts CHECK (delete_attempts >= 0),
    CONSTRAINT ck_call_recordings_version CHECK (version >= 1),
    CONSTRAINT ck_call_recordings_timestamps CHECK (updated_at >= created_at)
);
-- At most one active recording per Call.
CREATE UNIQUE INDEX uq_call_recordings_active
    ON call_recordings (call_id) WHERE state IN ('STARTING', 'RECORDING', 'STOPPING');
CREATE INDEX idx_call_recordings_call ON call_recordings (call_id, created_at);
CREATE INDEX idx_call_recordings_scope
    ON call_recordings (tenant_id, application_id, environment_id, created_at DESC, id DESC);
CREATE INDEX idx_call_recordings_work ON call_recordings (state, recording_deadline)
    WHERE state IN ('STARTING', 'RECORDING', 'STOPPING', 'PROCESSING');
CREATE INDEX idx_call_recordings_retention ON call_recordings (retention_until) WHERE state = 'READY';
CREATE INDEX idx_call_recordings_deleting ON call_recordings (next_attempt_at) WHERE state = 'DELETING';

CREATE FUNCTION call_recordings_guard_transition() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state IS DISTINCT FROM OLD.state AND NOT (
        (OLD.state = 'STARTING' AND NEW.state IN ('RECORDING', 'STOPPING', 'PROCESSING', 'READY', 'FAILED'))
        OR (OLD.state = 'RECORDING' AND NEW.state IN ('STOPPING', 'PROCESSING', 'READY', 'FAILED'))
        OR (OLD.state = 'STOPPING' AND NEW.state IN ('PROCESSING', 'READY', 'FAILED'))
        OR (OLD.state = 'PROCESSING' AND NEW.state IN ('READY', 'FAILED'))
        OR (OLD.state = 'READY' AND NEW.state = 'DELETING')
        OR (OLD.state = 'FAILED' AND NEW.state IN ('READY', 'DELETING'))
        OR (OLD.state = 'DELETING' AND NEW.state = 'DELETED')) THEN
        RAISE EXCEPTION 'recording invariant violated: % -> % is not allowed', OLD.state, NEW.state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_recordings_transition
    BEFORE UPDATE ON call_recordings
    FOR EACH ROW EXECUTE FUNCTION call_recordings_guard_transition();
CREATE TRIGGER trg_call_recordings_immutable
    BEFORE UPDATE ON call_recordings
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'call_id', 'authorization_id',
        'policy_id', 'capture_source', 'started_at', 'created_at');
CREATE TRIGGER trg_call_recordings_version
    BEFORE UPDATE ON call_recordings
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_call_recordings_no_delete
    BEFORE DELETE ON call_recordings
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

CREATE TABLE call_recording_access_audit (
    id                        UUID        NOT NULL,
    seq                       BIGSERIAL   NOT NULL,
    tenant_id                 UUID        NOT NULL,
    application_id            UUID        NOT NULL,
    environment_id            UUID        NOT NULL,
    recording_id              UUID        NOT NULL,
    call_id                   UUID        NOT NULL,
    actor_kind                TEXT        NOT NULL,
    actor_service_account_id  UUID,
    actor_credential_id       UUID,
    on_behalf_of_principal_id UUID,
    operation                 TEXT        NOT NULL,
    outcome                   TEXT        NOT NULL,
    reason                    VARCHAR(64),
    request_id                VARCHAR(36) NOT NULL,
    correlation_id            VARCHAR(37) NOT NULL,
    occurred_at               TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_call_recording_access_audit PRIMARY KEY (id),
    CONSTRAINT fk_call_recording_access_audit_recording
        FOREIGN KEY (tenant_id, application_id, environment_id, recording_id)
        REFERENCES call_recordings (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_call_recording_access_audit_actor
        CHECK (actor_kind IN ('APPLICATION_CREDENTIAL', 'SYSTEM')),
    CONSTRAINT ck_call_recording_access_audit_operation CHECK (operation IN (
        'READ_METADATA', 'MEDIA_ACCESS', 'DELETE', 'RETENTION_DELETE', 'STOP')),
    CONSTRAINT ck_call_recording_access_audit_outcome CHECK (outcome IN ('GRANTED', 'DENIED')),
    CONSTRAINT ck_call_recording_access_audit_reason
        CHECK (reason IS NULL OR reason ~ '^[A-Z][A-Z_]{1,63}$')
);
CREATE INDEX idx_call_recording_access_audit_recording
    ON call_recording_access_audit (recording_id, seq);
CREATE TRIGGER trg_call_recording_access_audit_append_only
    BEFORE UPDATE OR DELETE ON call_recording_access_audit
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

ALTER TABLE call_telephony_commands
    DROP CONSTRAINT ck_call_telephony_commands_kind,
    DROP CONSTRAINT ck_call_telephony_commands_target,
    ADD CONSTRAINT ck_call_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'ANSWER', 'PROMPT',
        'CAPTURE', 'CAPTURE_STOP')),
    ADD CONSTRAINT ck_call_telephony_commands_target CHECK (
        (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'ANSWER', 'PROMPT', 'CAPTURE',
                  'CAPTURE_STOP')
            AND call_leg_id IS NOT NULL)
        OR (kind IN ('BRIDGE', 'UNBRIDGE') AND bridge_id IS NOT NULL));

-- Part F §61: recording facts of the Call (published through the outbox).
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
        'VOICEMAIL_RECEIVED', 'VOICEMAIL_READY', 'VOICEMAIL_DELETED'));
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
DROP TABLE IF EXISTS call_recording_access_audit;
DROP TABLE IF EXISTS call_recordings;
DROP FUNCTION IF EXISTS call_recordings_guard_transition();
DROP TABLE IF EXISTS call_recording_authorizations;
DROP TABLE IF EXISTS call_recording_consents;
DROP TABLE IF EXISTS call_recording_policies;
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

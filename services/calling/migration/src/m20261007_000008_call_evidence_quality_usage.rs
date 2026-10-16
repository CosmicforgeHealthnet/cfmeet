//! Call authorizations, immutable call events, quality records and usage records
//! (Part C §16, §18).
//!
//! * `call_authorizations` persists each authorization decision (APPROVED / DENIED) with
//!   its policy version, reason code, bounded policy and limits snapshots and safe actor /
//!   request evidence. Recording a new decision supersedes the previous one; exactly one
//!   decision per call is effective (partial unique index). The Call may enter AUTHORIZED
//!   only with an effective APPROVED decision (checked in Rust under the call row lock).
//! * `call_events` is append-only evidence: `UPDATE`, `DELETE` and `TRUNCATE` are refused,
//!   sequences are gap-free per call, and every state-changing event matches the call's
//!   version and state at insertion. Conversely, a call row cannot commit a new version
//!   without its state event (deferred constraint trigger): no state mutation without
//!   event evidence.
//! * `call_quality_records` holds provider-independent, bounded numeric summaries.
//! * `call_usage_records` holds provider-independent consumption facts with a per-call
//!   idempotency key and set-once finalization. This is not billing or invoicing.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Call authorization: persisted decision evidence.
-- ---------------------------------------------------------------------------
CREATE TABLE call_authorizations (
    id                             UUID PRIMARY KEY,
    call_id                        UUID        NOT NULL,
    tenant_id                      UUID        NOT NULL,
    application_id                 UUID        NOT NULL,
    environment_id                 UUID        NOT NULL,
    decision                       TEXT        NOT NULL,
    reason_code                    TEXT        NOT NULL,
    policy_version                 VARCHAR(64) NOT NULL,
    policy_snapshot                JSONB       NOT NULL,
    limits_snapshot                JSONB       NOT NULL,
    actor_kind                     TEXT        NOT NULL,
    evidence_service_account_id    UUID        NOT NULL,
    evidence_credential_id         UUID,
    evidence_service               VARCHAR(64),
    evidence_principal_id          UUID        NOT NULL,
    request_id                     VARCHAR(36) NOT NULL,
    correlation_id                 VARCHAR(37) NOT NULL,
    decided_at                     TIMESTAMPTZ NOT NULL,
    superseded_at                  TIMESTAMPTZ,
    superseded_by_authorization_id UUID,
    created_at                     TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_authorizations_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_authorizations_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, evidence_service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_authorizations_credential
        FOREIGN KEY (tenant_id, application_id, environment_id, evidence_service_account_id, evidence_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_authorizations_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, evidence_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    -- The superseding decision is inserted after its predecessor is marked superseded.
    CONSTRAINT fk_call_authorizations_superseded_by
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, superseded_by_authorization_id)
        REFERENCES call_authorizations (tenant_id, application_id, environment_id, call_id, id)
        ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT uq_call_authorizations_owner_id UNIQUE (tenant_id, application_id, environment_id, call_id, id),
    CONSTRAINT ck_call_authorizations_decision CHECK (decision IN ('APPROVED', 'DENIED')),
    CONSTRAINT ck_call_authorizations_reason CHECK (reason_code IN (
        'POLICY_SATISFIED', 'POLICY_DENIED', 'LIMIT_EXCEEDED', 'ENVIRONMENT_RESTRICTED',
        'PRINCIPAL_INELIGIBLE', 'REVOKED')),
    CONSTRAINT ck_call_authorizations_reason_matches_decision
        CHECK ((decision = 'APPROVED') = (reason_code = 'POLICY_SATISFIED')),
    CONSTRAINT ck_call_authorizations_policy_version
        CHECK (policy_version ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    CONSTRAINT ck_call_authorizations_snapshots CHECK (
        jsonb_typeof(policy_snapshot) = 'object' AND octet_length(policy_snapshot::text) <= 4096
        AND jsonb_typeof(limits_snapshot) = 'object' AND octet_length(limits_snapshot::text) <= 4096),
    CONSTRAINT ck_call_authorizations_actor CHECK (
        (actor_kind = 'CUSTOMER_APPLICATION' AND evidence_credential_id IS NOT NULL AND evidence_service IS NULL)
        OR (actor_kind = 'INTERNAL_SERVICE' AND evidence_credential_id IS NULL
            AND evidence_service ~ '^cfmeet\.[a-z][a-z0-9]*(-[a-z0-9]+)*$')),
    CONSTRAINT ck_call_authorizations_request_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_call_authorizations_superseded CHECK (
        (superseded_at IS NULL) = (superseded_by_authorization_id IS NULL)
        AND superseded_by_authorization_id IS DISTINCT FROM id
        AND (superseded_at IS NULL OR superseded_at >= decided_at))
);
-- Exactly one effective (non-superseded) decision per call.
CREATE UNIQUE INDEX uq_call_authorizations_one_effective
    ON call_authorizations (call_id) WHERE superseded_at IS NULL;
CREATE INDEX idx_call_authorizations_call_decided ON call_authorizations (call_id, decided_at);
CREATE INDEX idx_call_authorizations_decision ON call_authorizations (decision, decided_at);
CREATE TRIGGER trg_call_authorizations_immutable
    BEFORE UPDATE ON call_authorizations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'tenant_id', 'application_id', 'environment_id', 'decision',
        'reason_code', 'policy_version', 'policy_snapshot', 'limits_snapshot', 'actor_kind',
        'evidence_service_account_id', 'evidence_credential_id', 'evidence_service',
        'evidence_principal_id', 'request_id', 'correlation_id', 'decided_at', 'created_at');
CREATE TRIGGER trg_call_authorizations_set_once
    BEFORE UPDATE ON call_authorizations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('superseded_at', 'superseded_by_authorization_id');
CREATE TRIGGER trg_call_authorizations_no_delete
    BEFORE DELETE ON call_authorizations
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Call event: immutable, append-only, gap-free per call.
-- ---------------------------------------------------------------------------

-- Sequences are gap-free; an event carries the call's current version, and a
-- state-changing event the call's current state.
CREATE FUNCTION calling_call_events_guard_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    last_sequence bigint;
    call_version  bigint;
    call_state    text;
BEGIN
    SELECT max(sequence) INTO last_sequence FROM call_events WHERE call_id = NEW.call_id;
    IF NEW.sequence IS DISTINCT FROM COALESCE(last_sequence, 0) + 1 THEN
        RAISE EXCEPTION 'calling invariant violated: call event sequences increase by one'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    SELECT version, current_state INTO call_version, call_state FROM calls WHERE id = NEW.call_id;
    IF NEW.call_version IS DISTINCT FROM call_version
       OR (NEW.to_state IS NOT NULL AND NEW.to_state IS DISTINCT FROM call_state) THEN
        RAISE EXCEPTION 'calling invariant violated: a call event must describe the call as persisted'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE call_events (
    event_id                 UUID PRIMARY KEY,
    call_id                  UUID        NOT NULL,
    tenant_id                UUID        NOT NULL,
    application_id           UUID        NOT NULL,
    environment_id           UUID        NOT NULL,
    sequence                 BIGINT      NOT NULL,
    event_type               TEXT        NOT NULL,
    schema_version           INTEGER     NOT NULL,
    from_state               TEXT,
    to_state                 TEXT,
    call_version             BIGINT      NOT NULL,
    actor_kind               TEXT        NOT NULL,
    actor_service_account_id UUID        NOT NULL,
    actor_credential_id      UUID,
    actor_service            VARCHAR(64),
    request_id               VARCHAR(36) NOT NULL,
    correlation_id           VARCHAR(37) NOT NULL,
    payload                  JSONB       NOT NULL,
    occurred_at              TIMESTAMPTZ NOT NULL,
    created_at               TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_events_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_events_actor_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, actor_service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_events_actor_credential
        FOREIGN KEY (tenant_id, application_id, environment_id, actor_service_account_id, actor_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT uq_call_events_call_sequence UNIQUE (call_id, sequence),
    CONSTRAINT ck_call_events_sequence CHECK (sequence >= 1),
    CONSTRAINT ck_call_events_call_version CHECK (call_version >= 1),
    CONSTRAINT ck_call_events_schema_version CHECK (schema_version = 1),
    CONSTRAINT ck_call_events_type CHECK (event_type IN (
        'CALL_REQUESTED', 'CALL_AUTHORIZATION_RECORDED', 'CALL_STATE_CHANGED',
        'CALL_PARTICIPANT_ADDED', 'CALL_PARTICIPANT_UPDATED',
        'CALL_SESSION_CREATED', 'CALL_SESSION_CONNECTED', 'CALL_SESSION_DISCONNECTED',
        'CALL_SESSION_RECONNECTED', 'CALL_SESSION_ENDED',
        'CALL_ROUTE_RECORDED', 'CALL_ROUTE_STATE_CHANGED',
        'CALL_LEG_RECORDED', 'CALL_LEG_STATE_CHANGED', 'CALL_LEG_BINDING_ASSIGNED',
        'CALL_ATTEMPT_RECORDED', 'CALL_ATTEMPT_FINISHED',
        'CALL_QUALITY_RECORDED', 'CALL_USAGE_RECORDED', 'CALL_USAGE_FINALIZED')),
    CONSTRAINT ck_call_events_states CHECK (
        (from_state IS NULL OR from_state IN (
            'REQUESTED', 'AUTHORIZED', 'RESOLVING', 'ROUTING', 'DIALING', 'RINGING', 'ANSWERED',
            'CONNECTED', 'HELD', 'TRANSFERRING', 'ENDING',
            'ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED'))
        AND (to_state IS NULL OR to_state IN (
            'REQUESTED', 'AUTHORIZED', 'RESOLVING', 'ROUTING', 'DIALING', 'RINGING', 'ANSWERED',
            'CONNECTED', 'HELD', 'TRANSFERRING', 'ENDING',
            'ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED'))),
    -- Only lifecycle events carry states, and they always do.
    CONSTRAINT ck_call_events_state_fields CHECK (
        (event_type IN ('CALL_REQUESTED', 'CALL_STATE_CHANGED')) = (to_state IS NOT NULL)
        AND (event_type = 'CALL_STATE_CHANGED') = (from_state IS NOT NULL)),
    CONSTRAINT ck_call_events_requested CHECK (
        event_type <> 'CALL_REQUESTED'
        OR (to_state = 'REQUESTED' AND call_version = 1 AND sequence = 1)),
    CONSTRAINT ck_call_events_state_changed CHECK (
        event_type <> 'CALL_STATE_CHANGED' OR (from_state <> to_state AND call_version >= 2)),
    CONSTRAINT ck_call_events_actor CHECK (
        (actor_kind = 'CUSTOMER_APPLICATION' AND actor_credential_id IS NOT NULL AND actor_service IS NULL)
        OR (actor_kind = 'INTERNAL_SERVICE' AND actor_credential_id IS NULL
            AND actor_service ~ '^cfmeet\.[a-z][a-z0-9]*(-[a-z0-9]+)*$')),
    CONSTRAINT ck_call_events_request_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_call_events_payload
        CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 8192)
);
-- One state-changing event per call version.
CREATE UNIQUE INDEX uq_call_events_state_change_per_version
    ON call_events (call_id, call_version) WHERE to_state IS NOT NULL;
CREATE TRIGGER trg_call_events_guard_insert
    BEFORE INSERT ON call_events
    FOR EACH ROW EXECUTE FUNCTION calling_call_events_guard_insert();
CREATE TRIGGER trg_call_events_immutable
    BEFORE UPDATE OR DELETE ON call_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
CREATE TRIGGER trg_call_events_no_truncate
    BEFORE TRUNCATE ON call_events
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

-- A call row cannot commit a new version without the state event describing it.
CREATE FUNCTION calling_calls_require_state_event() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM call_events e
         WHERE e.call_id = NEW.id AND e.call_version = NEW.version AND e.to_state = NEW.current_state
    ) THEN
        RAISE EXCEPTION 'calling invariant violated: call % version % (%) has no state event',
            NEW.id, NEW.version, NEW.current_state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER trg_calls_require_state_event_on_insert
    AFTER INSERT ON calls
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION calling_calls_require_state_event();
CREATE CONSTRAINT TRIGGER trg_calls_require_state_event_on_transition
    AFTER UPDATE ON calls
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW WHEN (OLD.version IS DISTINCT FROM NEW.version)
    EXECUTE FUNCTION calling_calls_require_state_event();

-- ---------------------------------------------------------------------------
-- Call quality record: bounded, provider-independent numeric summaries.
-- ---------------------------------------------------------------------------
CREATE TABLE call_quality_records (
    id             UUID PRIMARY KEY,
    call_id        UUID        NOT NULL,
    session_id     UUID,
    leg_id         UUID,
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    observed_from  TIMESTAMPTZ NOT NULL,
    observed_to    TIMESTAMPTZ NOT NULL,
    metrics        JSONB       NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_quality_records_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_quality_records_session
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, session_id)
        REFERENCES call_sessions (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_quality_records_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_call_quality_records_window CHECK (observed_to >= observed_from),
    CONSTRAINT ck_call_quality_records_metrics
        CHECK (jsonb_typeof(metrics) = 'object' AND octet_length(metrics::text) <= 4096)
);
CREATE INDEX idx_call_quality_records_call ON call_quality_records (call_id);
CREATE INDEX idx_call_quality_records_session ON call_quality_records (session_id);
CREATE INDEX idx_call_quality_records_leg ON call_quality_records (leg_id);
CREATE TRIGGER trg_call_quality_records_immutable
    BEFORE UPDATE OR DELETE ON call_quality_records
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Call usage record: provider-independent consumption facts (not billing).
-- ---------------------------------------------------------------------------
CREATE TABLE call_usage_records (
    id               UUID PRIMARY KEY,
    call_id          UUID         NOT NULL,
    participant_id   UUID,
    session_id       UUID,
    leg_id           UUID,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    usage_type       TEXT         NOT NULL,
    quantity         BIGINT       NOT NULL,
    unit             TEXT         NOT NULL,
    usage_started_at TIMESTAMPTZ,
    usage_ended_at   TIMESTAMPTZ,
    idempotency_key  VARCHAR(128) NOT NULL,
    finalized_at     TIMESTAMPTZ,
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT fk_call_usage_records_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_usage_records_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_usage_records_session
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, session_id)
        REFERENCES call_sessions (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_usage_records_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_usage_records_idempotency UNIQUE (call_id, idempotency_key),
    CONSTRAINT ck_call_usage_records_type CHECK (usage_type IN (
        'CALL_CONNECTED_TIME', 'PARTICIPANT_CONNECTED_TIME', 'SESSION_CONNECTED_TIME', 'LEG_CONNECTED_TIME')),
    CONSTRAINT ck_call_usage_records_unit CHECK (unit IN ('MILLISECONDS')),
    CONSTRAINT ck_call_usage_records_quantity CHECK (quantity >= 0),
    CONSTRAINT ck_call_usage_records_subject CHECK (
        (usage_type <> 'PARTICIPANT_CONNECTED_TIME' OR participant_id IS NOT NULL)
        AND (usage_type <> 'SESSION_CONNECTED_TIME' OR session_id IS NOT NULL)
        AND (usage_type <> 'LEG_CONNECTED_TIME' OR leg_id IS NOT NULL)),
    CONSTRAINT ck_call_usage_records_idempotency_key
        CHECK (idempotency_key ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_call_usage_records_window CHECK (
        (usage_started_at IS NULL OR usage_ended_at IS NULL OR usage_ended_at >= usage_started_at)
        AND (finalized_at IS NULL OR finalized_at >= created_at)
        AND updated_at >= created_at)
);
CREATE TRIGGER trg_call_usage_records_immutable
    BEFORE UPDATE ON call_usage_records
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'participant_id', 'session_id', 'leg_id', 'tenant_id', 'application_id',
        'environment_id', 'usage_type', 'quantity', 'unit', 'usage_started_at', 'usage_ended_at',
        'idempotency_key', 'created_at');
CREATE TRIGGER trg_call_usage_records_set_once
    BEFORE UPDATE ON call_usage_records
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('finalized_at');
CREATE TRIGGER trg_call_usage_records_no_delete
    BEFORE DELETE ON call_usage_records
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_usage_records;
DROP TABLE IF EXISTS call_quality_records;
DROP TRIGGER IF EXISTS trg_calls_require_state_event_on_transition ON calls;
DROP TRIGGER IF EXISTS trg_calls_require_state_event_on_insert ON calls;
DROP FUNCTION IF EXISTS calling_calls_require_state_event();
DROP TABLE IF EXISTS call_events;
DROP FUNCTION IF EXISTS calling_call_events_guard_insert();
DROP TABLE IF EXISTS call_authorizations;
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

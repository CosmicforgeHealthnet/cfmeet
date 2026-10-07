//! Call routes, call legs and call attempts (Part C §16): the canonical, provider-
//! independent Calling-side records. This is a domain foundation only — no routing
//! algorithm, carrier selection, failover or provider execution exists yet.
//!
//! * `call_routes` records a logical routing decision: a neutral route class (which kind of
//!   network the media path uses), a state, the policy version and a bounded decision
//!   snapshot. No vendor names and no provider business logic.
//! * `call_legs` is the canonical Calling-side leg: parent Call, optional route and
//!   participant, direction, neutral leg kind, normalized state and timing. Execution
//!   detail (provider / carrier ids, SIP dialogs, provider callbacks, reconciliation state)
//!   is owned by the future Telephony service in its own database; Calling stores at most
//!   an opaque UUID `execution_binding_ref`.
//! * `call_attempts` records each attempt with a gap-free per-call attempt number, a link
//!   to the previous attempt (future retry / failover evidence) and an explicit unknown
//!   outcome — an unknown outcome is not a failure; it can later be resolved.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Call route: the provider-independent logical routing decision.
-- ---------------------------------------------------------------------------
CREATE TABLE call_routes (
    id                UUID PRIMARY KEY,
    call_id           UUID        NOT NULL,
    tenant_id         UUID        NOT NULL,
    application_id    UUID        NOT NULL,
    environment_id    UUID        NOT NULL,
    route_class       TEXT        NOT NULL,
    state             TEXT        NOT NULL,
    policy_version    VARCHAR(64) NOT NULL,
    decision_snapshot JSONB       NOT NULL,
    selected_at       TIMESTAMPTZ NOT NULL,
    ended_at          TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL,
    updated_at        TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_routes_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_routes_owner_id UNIQUE (tenant_id, application_id, environment_id, call_id, id),
    CONSTRAINT ck_call_routes_class
        CHECK (route_class IN ('APPLICATION', 'PUBLIC_NETWORK', 'MIXED', 'PRIVATE_NETWORK')),
    CONSTRAINT ck_call_routes_state CHECK (state IN ('SELECTED', 'ACTIVE', 'ENDED', 'FAILED', 'SUPERSEDED')),
    CONSTRAINT ck_call_routes_ended CHECK ((state IN ('ENDED', 'FAILED', 'SUPERSEDED')) = (ended_at IS NOT NULL)),
    CONSTRAINT ck_call_routes_policy_version CHECK (policy_version ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    CONSTRAINT ck_call_routes_snapshot
        CHECK (jsonb_typeof(decision_snapshot) = 'object' AND octet_length(decision_snapshot::text) <= 4096),
    CONSTRAINT ck_call_routes_timestamps
        CHECK ((ended_at IS NULL OR ended_at >= selected_at) AND updated_at >= created_at)
);
CREATE INDEX idx_call_routes_call ON call_routes (call_id);
CREATE TRIGGER trg_call_routes_immutable
    BEFORE UPDATE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'tenant_id', 'application_id', 'environment_id', 'route_class',
        'policy_version', 'decision_snapshot', 'selected_at', 'created_at');
CREATE TRIGGER trg_call_routes_set_once
    BEFORE UPDATE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('ended_at');
CREATE TRIGGER trg_call_routes_state_transition
    BEFORE UPDATE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('state',
        'SELECTED>ACTIVE', 'SELECTED>ENDED', 'SELECTED>FAILED', 'SELECTED>SUPERSEDED',
        'ACTIVE>ENDED', 'ACTIVE>FAILED', 'ACTIVE>SUPERSEDED');
CREATE TRIGGER trg_call_routes_no_delete
    BEFORE DELETE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Call leg: canonical Calling-side leg (execution detail belongs to Telephony).
-- ---------------------------------------------------------------------------
CREATE TABLE call_legs (
    id                    UUID PRIMARY KEY,
    call_id               UUID        NOT NULL,
    route_id              UUID,
    participant_id        UUID,
    tenant_id             UUID        NOT NULL,
    application_id        UUID        NOT NULL,
    environment_id        UUID        NOT NULL,
    direction             TEXT        NOT NULL,
    leg_kind              TEXT        NOT NULL,
    state                 TEXT        NOT NULL,
    failure_category      TEXT,
    -- Opaque reference to an execution binding owned by another service (never a FK).
    execution_binding_ref UUID,
    created_at            TIMESTAMPTZ NOT NULL,
    dialing_at            TIMESTAMPTZ,
    ringing_at            TIMESTAMPTZ,
    answered_at           TIMESTAMPTZ,
    connected_at          TIMESTAMPTZ,
    ended_at              TIMESTAMPTZ,
    updated_at            TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_legs_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_legs_route
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, route_id)
        REFERENCES call_routes (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_legs_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_legs_owner_id UNIQUE (tenant_id, application_id, environment_id, call_id, id),
    CONSTRAINT uq_call_legs_execution_binding_ref UNIQUE (execution_binding_ref),
    CONSTRAINT ck_call_legs_direction CHECK (direction IN ('OUTBOUND', 'INBOUND')),
    CONSTRAINT ck_call_legs_kind CHECK (leg_kind IN ('APPLICATION', 'PUBLIC_NETWORK', 'PRIVATE_NETWORK')),
    CONSTRAINT ck_call_legs_state CHECK (state IN (
        'CREATED', 'DIALING', 'RINGING', 'ANSWERED', 'CONNECTED',
        'ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')),
    CONSTRAINT ck_call_legs_failure_category CHECK (failure_category IS NULL OR failure_category IN (
        'POLICY', 'TARGET_UNAVAILABLE', 'ROUTING', 'TIMEOUT', 'MEDIA', 'NETWORK', 'EXECUTION',
        'INTERNAL', 'UNKNOWN')),
    CONSTRAINT ck_call_legs_ended
        CHECK ((state IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')) = (ended_at IS NOT NULL)),
    CONSTRAINT ck_call_legs_failed CHECK ((state = 'FAILED') = (failure_category IS NOT NULL)),
    CONSTRAINT ck_call_legs_timestamps CHECK (
        (dialing_at IS NULL OR dialing_at >= created_at)
        AND (ringing_at IS NULL OR ringing_at >= COALESCE(dialing_at, created_at))
        AND (answered_at IS NULL OR answered_at >= GREATEST(created_at, dialing_at, ringing_at))
        AND (connected_at IS NULL OR (answered_at IS NOT NULL AND connected_at >= answered_at))
        AND (ended_at IS NULL OR ended_at >= GREATEST(created_at, dialing_at, ringing_at, answered_at, connected_at))
        AND updated_at >= created_at)
);
CREATE INDEX idx_call_legs_call ON call_legs (call_id);
CREATE INDEX idx_call_legs_route ON call_legs (route_id);
CREATE TRIGGER trg_call_legs_immutable
    BEFORE UPDATE ON call_legs
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'route_id', 'participant_id', 'tenant_id', 'application_id',
        'environment_id', 'direction', 'leg_kind', 'created_at');
CREATE TRIGGER trg_call_legs_set_once
    BEFORE UPDATE ON call_legs
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'failure_category', 'execution_binding_ref', 'dialing_at', 'ringing_at', 'answered_at',
        'connected_at', 'ended_at');
CREATE TRIGGER trg_call_legs_state_transition
    BEFORE UPDATE ON call_legs
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('state',
        'CREATED>DIALING', 'CREATED>RINGING', 'CREATED>FAILED', 'CREATED>CANCELLED',
        'DIALING>RINGING', 'DIALING>ANSWERED', 'DIALING>DECLINED', 'DIALING>BUSY',
        'DIALING>NO_ANSWER', 'DIALING>FAILED', 'DIALING>CANCELLED',
        'RINGING>ANSWERED', 'RINGING>DECLINED', 'RINGING>BUSY', 'RINGING>NO_ANSWER',
        'RINGING>FAILED', 'RINGING>CANCELLED',
        'ANSWERED>CONNECTED', 'ANSWERED>ENDED', 'ANSWERED>FAILED',
        'CONNECTED>ENDED', 'CONNECTED>FAILED');
CREATE TRIGGER trg_call_legs_no_delete
    BEFORE DELETE ON call_legs
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Call attempt: per-call numbered attempts with explicit unknown outcome.
-- ---------------------------------------------------------------------------

-- Attempt numbers are gap-free per call; a previous attempt has a lower number; attempts
-- are recorded PENDING.
CREATE FUNCTION calling_call_attempts_guard_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    last_number     integer;
    previous_number integer;
BEGIN
    SELECT max(attempt_number) INTO last_number FROM call_attempts WHERE call_id = NEW.call_id;
    IF NEW.attempt_number IS DISTINCT FROM COALESCE(last_number, 0) + 1 THEN
        RAISE EXCEPTION 'calling invariant violated: attempt numbers increase by one per call'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.previous_attempt_id IS NOT NULL THEN
        SELECT attempt_number INTO previous_number FROM call_attempts WHERE id = NEW.previous_attempt_id;
        IF previous_number IS NULL OR previous_number >= NEW.attempt_number THEN
            RAISE EXCEPTION 'calling invariant violated: a previous attempt precedes its successor'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END IF;
    IF NEW.outcome <> 'PENDING' OR NEW.unknown_outcome THEN
        RAISE EXCEPTION 'calling invariant violated: attempts are recorded PENDING'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- unknown_outcome records that the outcome was unknown at some point; it is never cleared.
CREATE FUNCTION calling_call_attempts_guard_unknown() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.unknown_outcome AND NOT NEW.unknown_outcome THEN
        RAISE EXCEPTION 'calling invariant violated: call_attempts.unknown_outcome cannot be cleared'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE call_attempts (
    id                  UUID PRIMARY KEY,
    call_id             UUID        NOT NULL,
    route_id            UUID,
    leg_id              UUID,
    tenant_id           UUID        NOT NULL,
    application_id      UUID        NOT NULL,
    environment_id      UUID        NOT NULL,
    attempt_number      INTEGER     NOT NULL,
    previous_attempt_id UUID,
    outcome             TEXT        NOT NULL,
    unknown_outcome     BOOLEAN     NOT NULL,
    failure_category    TEXT,
    started_at          TIMESTAMPTZ NOT NULL,
    finished_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL,
    updated_at          TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_attempts_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_attempts_route
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, route_id)
        REFERENCES call_routes (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_attempts_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_attempts_previous
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, previous_attempt_id)
        REFERENCES call_attempts (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_attempts_call_number UNIQUE (call_id, attempt_number),
    CONSTRAINT uq_call_attempts_owner_id UNIQUE (tenant_id, application_id, environment_id, call_id, id),
    -- Attempts form a chain: an attempt is followed by at most one retry.
    CONSTRAINT uq_call_attempts_previous UNIQUE (previous_attempt_id),
    CONSTRAINT ck_call_attempts_number CHECK (attempt_number >= 1),
    CONSTRAINT ck_call_attempts_previous
        CHECK (previous_attempt_id IS DISTINCT FROM id AND (previous_attempt_id IS NULL OR attempt_number > 1)),
    CONSTRAINT ck_call_attempts_outcome
        CHECK (outcome IN ('PENDING', 'SUCCEEDED', 'FAILED', 'UNKNOWN', 'ABANDONED')),
    CONSTRAINT ck_call_attempts_finished CHECK ((outcome = 'PENDING') = (finished_at IS NULL)),
    CONSTRAINT ck_call_attempts_unknown CHECK (outcome <> 'UNKNOWN' OR unknown_outcome),
    CONSTRAINT ck_call_attempts_failure_category CHECK (failure_category IS NULL OR failure_category IN (
        'POLICY', 'TARGET_UNAVAILABLE', 'ROUTING', 'TIMEOUT', 'MEDIA', 'NETWORK', 'EXECUTION',
        'INTERNAL', 'UNKNOWN')),
    CONSTRAINT ck_call_attempts_failed CHECK ((outcome = 'FAILED') = (failure_category IS NOT NULL)),
    CONSTRAINT ck_call_attempts_timestamps
        CHECK ((finished_at IS NULL OR finished_at >= started_at) AND updated_at >= created_at)
);
CREATE INDEX idx_call_attempts_route ON call_attempts (route_id);
CREATE INDEX idx_call_attempts_leg ON call_attempts (leg_id);
CREATE TRIGGER trg_call_attempts_guard_insert
    BEFORE INSERT ON call_attempts
    FOR EACH ROW EXECUTE FUNCTION calling_call_attempts_guard_insert();
CREATE TRIGGER trg_call_attempts_immutable
    BEFORE UPDATE ON call_attempts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'route_id', 'leg_id', 'tenant_id', 'application_id', 'environment_id',
        'attempt_number', 'previous_attempt_id', 'started_at', 'created_at');
CREATE TRIGGER trg_call_attempts_set_once
    BEFORE UPDATE ON call_attempts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('finished_at', 'failure_category');
CREATE TRIGGER trg_call_attempts_outcome_transition
    BEFORE UPDATE ON call_attempts
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('outcome',
        'PENDING>SUCCEEDED', 'PENDING>FAILED', 'PENDING>UNKNOWN', 'PENDING>ABANDONED',
        'UNKNOWN>SUCCEEDED', 'UNKNOWN>FAILED');
CREATE TRIGGER trg_call_attempts_unknown_kept
    BEFORE UPDATE ON call_attempts
    FOR EACH ROW EXECUTE FUNCTION calling_call_attempts_guard_unknown();
CREATE TRIGGER trg_call_attempts_no_delete
    BEFORE DELETE ON call_attempts
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_attempts;
DROP FUNCTION IF EXISTS calling_call_attempts_guard_unknown();
DROP FUNCTION IF EXISTS calling_call_attempts_guard_insert();
DROP TABLE IF EXISTS call_legs;
DROP TABLE IF EXISTS call_routes;
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

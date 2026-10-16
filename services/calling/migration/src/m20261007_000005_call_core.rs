//! Call core (Part C §15–§18): the canonical, provider-independent Call aggregate root.
//!
//! * `calls.id` is a CF Meet-generated UUID. No provider room / dialog / carrier / request
//!   identifier is ever a Call identity; future execution bindings live behind their own
//!   boundaries and reach this table only as opaque UUID references.
//! * Ownership (tenant / application / environment), the creating service account and
//!   credential, and the initiating principal are pinned by composite foreign keys, so a
//!   Call can never point at a credential or principal of another scope.
//! * `current_state` holds exactly the 17 canonical business states. The transition graph
//!   is decided in Rust (`src/calls/state.rs`); the database repeats it as
//!   defence in depth (`calling_call_transition_allowed`) so no direct write can skip
//!   states or resurrect a terminal Call.
//! * `version` is the optimistic-concurrency counter: 1 at creation, +1 exactly once per
//!   state transition, unchanged otherwise (trigger-enforced).
//! * Lifecycle timestamps are written once (first entry into the state) and are ordered by
//!   `CHECK`.
//!
//! This migration also adds composite uniqueness targets on `platform_principals` so call
//! rows can prove principal ownership (and the principal-kind snapshot) by foreign key.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Composite targets: (scope, id) and (scope, id, kind) of a principal.
-- ---------------------------------------------------------------------------
ALTER TABLE platform_principals
    ADD CONSTRAINT uq_platform_principals_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    ADD CONSTRAINT uq_platform_principals_owner_kind_id
        UNIQUE (tenant_id, application_id, environment_id, id, kind);

-- ---------------------------------------------------------------------------
-- Shared call-domain invariant guards
-- ---------------------------------------------------------------------------

-- Append-only evidence: refuses the triggering operation (UPDATE / DELETE / TRUNCATE).
CREATE FUNCTION calling_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'calling invariant violated: % rows are immutable evidence (% refused)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

-- Numeric columns named in the trigger arguments never decrease.
CREATE FUNCTION calling_forbid_decrease() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col text;
BEGIN
    FOREACH col IN ARRAY TG_ARGV LOOP
        IF (to_jsonb(NEW) ->> col)::numeric < (to_jsonb(OLD) ->> col)::numeric THEN
            RAISE EXCEPTION 'calling invariant violated: %.% must not decrease', TG_TABLE_NAME, col
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

-- State column TG_ARGV[0] may change only along the edges 'FROM>TO' listed in
-- TG_ARGV[1..]. A state with no outgoing edge is terminal.
CREATE FUNCTION calling_guard_state_transition() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col       text := TG_ARGV[0];
    old_value text := to_jsonb(OLD) ->> col;
    new_value text := to_jsonb(NEW) ->> col;
BEGIN
    IF old_value IS DISTINCT FROM new_value
       AND NOT ((old_value || '>' || new_value) = ANY (TG_ARGV[1:TG_NARGS - 1])) THEN
        RAISE EXCEPTION 'calling invariant violated: %.% transition % -> % is not allowed',
            TG_TABLE_NAME, col, old_value, new_value
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- The canonical Call transition graph (Part C §17). Mirrors CallState::allowed_targets in
-- Rust; services/calling/tests/call_state_machine.rs proves both agree on all 17 x 17 pairs.
CREATE FUNCTION calling_call_transition_allowed(from_state text, to_state text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
SELECT COALESCE(CASE from_state
    WHEN 'REQUESTED'    THEN to_state IN ('AUTHORIZED', 'FAILED', 'CANCELLED')
    WHEN 'AUTHORIZED'   THEN to_state IN ('RESOLVING', 'FAILED', 'CANCELLED')
    WHEN 'RESOLVING'    THEN to_state IN ('ROUTING', 'FAILED', 'CANCELLED')
    WHEN 'ROUTING'      THEN to_state IN ('DIALING', 'FAILED', 'CANCELLED')
    WHEN 'DIALING'      THEN to_state IN ('RINGING', 'ANSWERED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')
    WHEN 'RINGING'      THEN to_state IN ('ANSWERED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')
    WHEN 'ANSWERED'     THEN to_state IN ('CONNECTED', 'ENDING', 'FAILED', 'CANCELLED')
    WHEN 'CONNECTED'    THEN to_state IN ('HELD', 'TRANSFERRING', 'ENDING', 'FAILED')
    WHEN 'HELD'         THEN to_state IN ('CONNECTED', 'TRANSFERRING', 'ENDING', 'FAILED')
    WHEN 'TRANSFERRING' THEN to_state IN ('CONNECTED', 'HELD', 'ENDING', 'FAILED')
    WHEN 'ENDING'       THEN to_state IN ('ENDED', 'FAILED')
    ELSE FALSE
END, FALSE)
$$;

-- A Call is created REQUESTED at version 1 with no later lifecycle facts.
CREATE FUNCTION calling_calls_guard_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.current_state <> 'REQUESTED' OR NEW.version <> 1
       OR num_nonnulls(NEW.authorized_at, NEW.resolving_at, NEW.routing_at, NEW.dialing_at,
                       NEW.ringing_at, NEW.answered_at, NEW.connected_at, NEW.ending_at,
                       NEW.ended_at, NEW.terminal_reason, NEW.failure_category) > 0 THEN
        RAISE EXCEPTION 'calling invariant violated: a call is created REQUESTED at version 1'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- Terminal states are final; a state change follows the graph and increments version by
-- exactly one; version never changes without a state change.
CREATE FUNCTION calling_calls_guard_transition() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.current_state IS DISTINCT FROM OLD.current_state THEN
        IF OLD.current_state IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED') THEN
            RAISE EXCEPTION 'calling invariant violated: call % is terminal (%)', OLD.id, OLD.current_state
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
        IF NOT calling_call_transition_allowed(OLD.current_state, NEW.current_state) THEN
            RAISE EXCEPTION 'calling invariant violated: call transition % -> % is not allowed',
                OLD.current_state, NEW.current_state
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
        IF NEW.version IS DISTINCT FROM OLD.version + 1 THEN
            RAISE EXCEPTION 'calling invariant violated: a call transition increments version exactly once'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    ELSIF NEW.version IS DISTINCT FROM OLD.version THEN
        RAISE EXCEPTION 'calling invariant violated: call version changes only with a state transition'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.updated_at < OLD.updated_at THEN
        RAISE EXCEPTION 'calling invariant violated: calls.updated_at must not move backwards'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------------------
-- Call: the aggregate root.
-- ---------------------------------------------------------------------------
CREATE TABLE calls (
    id                            UUID PRIMARY KEY,
    tenant_id                     UUID        NOT NULL,
    application_id                UUID        NOT NULL,
    environment_id                UUID        NOT NULL,
    created_by_service_account_id UUID        NOT NULL,
    created_by_credential_id      UUID        NOT NULL,
    initiator_principal_id        UUID        NOT NULL,
    call_kind                     TEXT        NOT NULL,
    current_state                 TEXT        NOT NULL,
    version                       BIGINT      NOT NULL,
    -- Opaque references to records owned by other services (never foreign keys).
    communication_context_ref     UUID,
    privacy_relay_ref             UUID,
    terminal_reason               TEXT,
    failure_category              TEXT,
    requested_at                  TIMESTAMPTZ NOT NULL,
    authorized_at                 TIMESTAMPTZ,
    resolving_at                  TIMESTAMPTZ,
    routing_at                    TIMESTAMPTZ,
    dialing_at                    TIMESTAMPTZ,
    ringing_at                    TIMESTAMPTZ,
    answered_at                   TIMESTAMPTZ,
    connected_at                  TIMESTAMPTZ,
    ending_at                     TIMESTAMPTZ,
    ended_at                      TIMESTAMPTZ,
    created_at                    TIMESTAMPTZ NOT NULL,
    updated_at                    TIMESTAMPTZ NOT NULL,
    -- The creating credential belongs to the creating service account, in the same scope.
    CONSTRAINT fk_calls_credential
        FOREIGN KEY (tenant_id, application_id, environment_id, created_by_service_account_id, created_by_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_calls_initiator_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, initiator_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_calls_owner_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_calls_kind CHECK (call_kind IN ('VOICE', 'VIDEO')),
    CONSTRAINT ck_calls_state CHECK (current_state IN (
        'REQUESTED', 'AUTHORIZED', 'RESOLVING', 'ROUTING', 'DIALING', 'RINGING', 'ANSWERED',
        'CONNECTED', 'HELD', 'TRANSFERRING', 'ENDING',
        'ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')),
    CONSTRAINT ck_calls_version CHECK (version >= 1),
    CONSTRAINT ck_calls_terminal_reason CHECK (terminal_reason IS NULL OR terminal_reason IN (
        'CLEANUP_CONFIRMED', 'PARTICIPATION_ENDED', 'RECONCILIATION_TIMEOUT',
        'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')),
    CONSTRAINT ck_calls_failure_category CHECK (failure_category IS NULL OR failure_category IN (
        'POLICY', 'TARGET_UNAVAILABLE', 'ROUTING', 'TIMEOUT', 'MEDIA', 'NETWORK', 'EXECUTION',
        'INTERNAL', 'UNKNOWN')),
    -- Terminal <=> ended_at <=> terminal_reason.
    CONSTRAINT ck_calls_terminal_complete CHECK (
        (current_state IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')) = (ended_at IS NOT NULL)
        AND (ended_at IS NOT NULL) = (terminal_reason IS NOT NULL)),
    CONSTRAINT ck_calls_terminal_reason_matches_state CHECK (
        terminal_reason IS NULL
        OR (current_state = 'ENDED'
            AND terminal_reason IN ('CLEANUP_CONFIRMED', 'PARTICIPATION_ENDED', 'RECONCILIATION_TIMEOUT'))
        OR (current_state <> 'ENDED' AND terminal_reason = current_state)),
    CONSTRAINT ck_calls_failure_only_when_failed CHECK ((current_state = 'FAILED') = (failure_category IS NOT NULL)),
    -- First-entry timestamps follow the transition graph and never run backwards.
    CONSTRAINT ck_calls_timestamps_ordered CHECK (
        (authorized_at IS NULL OR authorized_at >= requested_at)
        AND (resolving_at IS NULL OR (authorized_at IS NOT NULL AND resolving_at >= authorized_at))
        AND (routing_at IS NULL OR (resolving_at IS NOT NULL AND routing_at >= resolving_at))
        AND (dialing_at IS NULL OR (routing_at IS NOT NULL AND dialing_at >= routing_at))
        AND (ringing_at IS NULL OR (dialing_at IS NOT NULL AND ringing_at >= dialing_at))
        AND (answered_at IS NULL OR (dialing_at IS NOT NULL AND answered_at >= dialing_at
                                     AND (ringing_at IS NULL OR answered_at >= ringing_at)))
        AND (connected_at IS NULL OR (answered_at IS NOT NULL AND connected_at >= answered_at))
        AND (ending_at IS NULL OR (answered_at IS NOT NULL AND ending_at >= answered_at))
        AND (ended_at IS NULL OR ended_at >= GREATEST(requested_at, authorized_at, resolving_at, routing_at,
                                                      dialing_at, ringing_at, answered_at, connected_at, ending_at))
        AND updated_at >= created_at)
);
CREATE INDEX idx_calls_scope_created ON calls (tenant_id, application_id, environment_id, created_at DESC, id DESC);
CREATE INDEX idx_calls_scope_state ON calls (tenant_id, application_id, environment_id, current_state);
CREATE INDEX idx_calls_initiator_principal ON calls (initiator_principal_id);
-- Active (non-terminal) calls: concurrent-call limit enforcement.
CREATE INDEX idx_calls_active_by_credential ON calls (created_by_credential_id)
    WHERE current_state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED');
CREATE INDEX idx_calls_active_by_application ON calls (application_id)
    WHERE current_state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED');

CREATE TRIGGER trg_calls_guard_insert
    BEFORE INSERT ON calls
    FOR EACH ROW EXECUTE FUNCTION calling_calls_guard_insert();
CREATE TRIGGER trg_calls_guard_transition
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION calling_calls_guard_transition();
CREATE TRIGGER trg_calls_immutable
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_by_service_account_id',
        'created_by_credential_id', 'initiator_principal_id', 'call_kind', 'requested_at', 'created_at');
CREATE TRIGGER trg_calls_set_once
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'communication_context_ref', 'privacy_relay_ref', 'terminal_reason', 'failure_category',
        'authorized_at', 'resolving_at', 'routing_at', 'dialing_at', 'ringing_at', 'answered_at',
        'connected_at', 'ending_at', 'ended_at');
CREATE TRIGGER trg_calls_no_delete
    BEFORE DELETE ON calls
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS calls;
DROP FUNCTION IF EXISTS calling_calls_guard_transition();
DROP FUNCTION IF EXISTS calling_calls_guard_insert();
DROP FUNCTION IF EXISTS calling_call_transition_allowed(text, text);
DROP FUNCTION IF EXISTS calling_guard_state_transition();
DROP FUNCTION IF EXISTS calling_forbid_decrease();
DROP FUNCTION IF EXISTS calling_forbid_mutation();
ALTER TABLE platform_principals
    DROP CONSTRAINT IF EXISTS uq_platform_principals_owner_kind_id,
    DROP CONSTRAINT IF EXISTS uq_platform_principals_owner_id;
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

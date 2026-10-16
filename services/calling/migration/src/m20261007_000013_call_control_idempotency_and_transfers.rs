//! Command idempotency and application transfers (Part C §25).
//!
//! * `call_commands` is the generic idempotency record of Calling commands. The unique
//!   key is (tenant, application, environment, operation, Idempotency-Key): a concurrent
//!   replay of the same key blocks on the unique index until the first execution commits
//!   and then reads its record, so one key has exactly one domain effect. The record is
//!   written in the transaction that performs the command's authoritative effect (or, for
//!   Call creation, the creating transaction) and completed with the response snapshot
//!   returned to every replay. The request fingerprint (SHA-256 of the canonical request)
//!   detects a different request under the same key (409). Snapshots are bounded and may
//!   never contain credentials (CHECK).
//! * `call_transfers` records an application-to-application transfer of one Call: the
//!   transferring participant, the requested target principal (same scope), the target
//!   participant (pinned to that principal), the transfer route / leg / attempt evidence
//!   and the pre-transfer Call state it falls back to on failure. Its own state machine
//!   keeps transfer sub-states out of the Call state; at most one transfer is in flight
//!   per Call.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Command idempotency records.
-- ---------------------------------------------------------------------------
CREATE TABLE call_commands (
    id                       UUID PRIMARY KEY,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    call_id                  UUID,
    operation                TEXT         NOT NULL,
    idempotency_key          VARCHAR(255) NOT NULL,
    request_fingerprint      CHAR(64)     NOT NULL,
    status                   TEXT         NOT NULL,
    result_call_version      BIGINT,
    result_resource_id       UUID,
    response_status          SMALLINT,
    response_snapshot        JSONB,
    actor_kind               TEXT         NOT NULL,
    actor_service_account_id UUID         NOT NULL,
    actor_credential_id      UUID,
    actor_service            VARCHAR(64),
    actor_endpoint_id        UUID,
    request_id               VARCHAR(36)  NOT NULL,
    correlation_id           VARCHAR(37)  NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    completed_at             TIMESTAMPTZ,
    CONSTRAINT fk_call_commands_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_commands_actor_credential
        FOREIGN KEY (tenant_id, application_id, environment_id, actor_service_account_id, actor_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_commands_actor_endpoint
        FOREIGN KEY (tenant_id, application_id, environment_id, actor_endpoint_id)
        REFERENCES call_endpoints (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_commands_idempotency
        UNIQUE (tenant_id, application_id, environment_id, operation, idempotency_key),
    CONSTRAINT ck_call_commands_operation CHECK (operation IN (
        'CREATE_CALL', 'ACCEPT', 'DECLINE', 'CANCEL', 'END', 'HOLD', 'RESUME', 'TRANSFER',
        'REMOVE_SESSION')),
    CONSTRAINT ck_call_commands_key CHECK (idempotency_key ~ '^[!-~]{1,255}$'),
    CONSTRAINT ck_call_commands_fingerprint CHECK (request_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_call_commands_status CHECK (status IN ('IN_PROGRESS', 'COMPLETED')),
    CONSTRAINT ck_call_commands_completed CHECK (
        (status = 'COMPLETED') = (completed_at IS NOT NULL)
        AND (status = 'COMPLETED') = (response_snapshot IS NOT NULL)
        AND (status = 'COMPLETED') = (response_status IS NOT NULL)),
    CONSTRAINT ck_call_commands_call CHECK (operation = 'CREATE_CALL' OR call_id IS NOT NULL),
    CONSTRAINT ck_call_commands_response_status CHECK (response_status IS NULL OR response_status BETWEEN 200 AND 299),
    -- Snapshots are bounded and never hold credentials.
    CONSTRAINT ck_call_commands_snapshot CHECK (
        response_snapshot IS NULL OR (
            jsonb_typeof(response_snapshot) = 'object'
            AND octet_length(response_snapshot::text) <= 16384
            AND response_snapshot::text !~* '"[a-z_]*(token|secret|password)[a-z_]*"\s*:')),
    CONSTRAINT ck_call_commands_actor CHECK (
        (actor_kind = 'CUSTOMER_APPLICATION' AND actor_credential_id IS NOT NULL
            AND actor_service IS NULL AND actor_endpoint_id IS NULL)
        OR (actor_kind = 'APPLICATION_ENDPOINT' AND actor_credential_id IS NOT NULL
            AND actor_service IS NULL AND actor_endpoint_id IS NOT NULL)
        OR (actor_kind = 'INTERNAL_SERVICE' AND actor_credential_id IS NULL AND actor_endpoint_id IS NULL
            AND actor_service ~ '^cfmeet\.[a-z][a-z0-9]*(-[a-z0-9]+)*$')),
    CONSTRAINT ck_call_commands_request_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_call_commands_timestamps CHECK (completed_at IS NULL OR completed_at >= created_at)
);
CREATE INDEX idx_call_commands_call ON call_commands (call_id);
CREATE TRIGGER trg_call_commands_immutable
    BEFORE UPDATE ON call_commands
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'call_id', 'operation',
        'idempotency_key', 'request_fingerprint', 'actor_kind', 'actor_service_account_id',
        'actor_credential_id', 'actor_service', 'actor_endpoint_id', 'request_id',
        'correlation_id', 'created_at');
CREATE TRIGGER trg_call_commands_set_once
    BEFORE UPDATE ON call_commands
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'result_call_version', 'result_resource_id', 'response_status', 'response_snapshot',
        'completed_at');
CREATE TRIGGER trg_call_commands_status_transition
    BEFORE UPDATE ON call_commands
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status', 'IN_PROGRESS>COMPLETED');
CREATE TRIGGER trg_call_commands_no_delete
    BEFORE DELETE ON call_commands
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Application-to-application transfer of one Call.
-- ---------------------------------------------------------------------------
CREATE TABLE call_transfers (
    id                            UUID PRIMARY KEY,
    call_id                       UUID        NOT NULL,
    tenant_id                     UUID        NOT NULL,
    application_id                UUID        NOT NULL,
    environment_id                UUID        NOT NULL,
    transferring_participant_id   UUID        NOT NULL,
    requested_target_principal_id UUID        NOT NULL,
    target_participant_id         UUID,
    new_route_id                  UUID,
    new_leg_id                    UUID,
    attempt_id                    UUID,
    command_id                    UUID        NOT NULL,
    state                         TEXT        NOT NULL,
    pre_transfer_state            TEXT        NOT NULL,
    expected_call_version         BIGINT      NOT NULL,
    failure_category              TEXT,
    failure_reason                TEXT,
    initiated_at                  TIMESTAMPTZ NOT NULL,
    ringing_at                    TIMESTAMPTZ,
    accepted_at                   TIMESTAMPTZ,
    completed_at                  TIMESTAMPTZ,
    failed_at                     TIMESTAMPTZ,
    cancelled_at                  TIMESTAMPTZ,
    created_at                    TIMESTAMPTZ NOT NULL,
    updated_at                    TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_transfers_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_transfers_transferring_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, transferring_participant_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    -- The target is a principal of the Call's own scope …
    CONSTRAINT fk_call_transfers_target_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, requested_target_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    -- … and the target participant represents exactly that principal.
    CONSTRAINT fk_call_transfers_target_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, target_participant_id, requested_target_principal_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id, principal_id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_transfers_route
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, new_route_id)
        REFERENCES call_routes (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_transfers_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, new_leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_transfers_attempt
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, attempt_id)
        REFERENCES call_attempts (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_transfers_command FOREIGN KEY (command_id) REFERENCES call_commands (id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_transfers_owner_id UNIQUE (tenant_id, application_id, environment_id, call_id, id),
    CONSTRAINT uq_call_transfers_command UNIQUE (command_id),
    CONSTRAINT ck_call_transfers_distinct_parties
        CHECK (target_participant_id IS DISTINCT FROM transferring_participant_id),
    CONSTRAINT ck_call_transfers_state CHECK (state IN (
        'REQUESTED', 'RINGING_TARGET', 'ACCEPTED', 'COMPLETED', 'FAILED', 'CANCELLED')),
    CONSTRAINT ck_call_transfers_pre_state CHECK (pre_transfer_state IN ('CONNECTED', 'HELD')),
    CONSTRAINT ck_call_transfers_expected_version CHECK (expected_call_version >= 1),
    CONSTRAINT ck_call_transfers_target CHECK (state IN ('REQUESTED', 'FAILED', 'CANCELLED') OR target_participant_id IS NOT NULL),
    CONSTRAINT ck_call_transfers_completed CHECK ((state = 'COMPLETED') = (completed_at IS NOT NULL)),
    CONSTRAINT ck_call_transfers_failed CHECK (
        (state = 'FAILED') = (failed_at IS NOT NULL)
        AND (state = 'FAILED') = (failure_category IS NOT NULL)),
    CONSTRAINT ck_call_transfers_cancelled CHECK ((state = 'CANCELLED') = (cancelled_at IS NOT NULL)),
    CONSTRAINT ck_call_transfers_reason CHECK (
        (state IN ('FAILED', 'CANCELLED')) = (failure_reason IS NOT NULL)
        AND (failure_reason IS NULL OR failure_reason IN (
            'TARGET_DECLINED', 'TARGET_NO_ANSWER', 'TARGET_UNAVAILABLE', 'MEDIA_PATH_LOST',
            'CALL_ENDED', 'CANCELLED'))),
    CONSTRAINT ck_call_transfers_failure_category CHECK (failure_category IS NULL OR failure_category IN (
        'POLICY', 'TARGET_UNAVAILABLE', 'ROUTING', 'TIMEOUT', 'MEDIA', 'NETWORK', 'EXECUTION',
        'INTERNAL', 'UNKNOWN')),
    CONSTRAINT ck_call_transfers_timestamps CHECK (
        (ringing_at IS NULL OR ringing_at >= initiated_at)
        AND (accepted_at IS NULL OR accepted_at >= initiated_at)
        AND (completed_at IS NULL OR (accepted_at IS NOT NULL AND completed_at >= accepted_at))
        AND (failed_at IS NULL OR failed_at >= initiated_at)
        AND (cancelled_at IS NULL OR cancelled_at >= initiated_at)
        AND updated_at >= created_at)
);
-- At most one transfer in flight per Call.
CREATE UNIQUE INDEX uq_call_transfers_one_active
    ON call_transfers (call_id) WHERE state IN ('REQUESTED', 'RINGING_TARGET', 'ACCEPTED');
CREATE TRIGGER trg_call_transfers_immutable
    BEFORE UPDATE ON call_transfers
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'tenant_id', 'application_id', 'environment_id',
        'transferring_participant_id', 'requested_target_principal_id', 'command_id',
        'pre_transfer_state', 'expected_call_version', 'initiated_at', 'created_at');
CREATE TRIGGER trg_call_transfers_set_once
    BEFORE UPDATE ON call_transfers
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'target_participant_id', 'new_route_id', 'new_leg_id', 'attempt_id', 'failure_category',
        'failure_reason', 'ringing_at', 'accepted_at', 'completed_at', 'failed_at', 'cancelled_at');
CREATE TRIGGER trg_call_transfers_state_transition
    BEFORE UPDATE ON call_transfers
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('state',
        'REQUESTED>RINGING_TARGET', 'REQUESTED>FAILED', 'REQUESTED>CANCELLED',
        'RINGING_TARGET>ACCEPTED', 'RINGING_TARGET>FAILED', 'RINGING_TARGET>CANCELLED',
        'ACCEPTED>COMPLETED', 'ACCEPTED>FAILED', 'ACCEPTED>CANCELLED');
CREATE TRIGGER trg_call_transfers_no_delete
    BEFORE DELETE ON call_transfers
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- Transfer ring offers belong to a transfer of the same Call.
ALTER TABLE call_invitations
    ADD CONSTRAINT fk_call_invitations_transfer
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, transfer_id)
        REFERENCES call_transfers (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT;
"#;

const DOWN: &str = r#"
ALTER TABLE call_invitations DROP CONSTRAINT IF EXISTS fk_call_invitations_transfer;
DROP TABLE IF EXISTS call_transfers;
DROP TABLE IF EXISTS call_commands;
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

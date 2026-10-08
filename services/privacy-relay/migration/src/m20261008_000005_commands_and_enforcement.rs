//! Command idempotency and durable active-call enforcement (Part D §29).
//!
//! * `privacy_relay_commands`: idempotency records of every Privacy Relay mutation. The
//!   namespace is (tenant, application, environment, operation, Idempotency-Key); the
//!   canonical request fingerprint detects a different request under the same key (409).
//!   The record is written in the transaction that performs the command's authoritative
//!   effect, so one key has exactly one domain effect.
//! * `privacy_relay_enforcement_jobs`: a durable enforcement command for Calling,
//!   persisted in the SAME transaction as the authority change that requires it
//!   (revocation, participant removal, suspension, completion without grace, final
//!   expiry). Delivery is synchronous-first with durable, leased (`FOR UPDATE SKIP
//!   LOCKED`) retries; at most one job exists per authority change. Enforcement is
//!   restrictive only: it can terminate protected calls, never grant authority.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE privacy_relay_commands (
    id                       UUID PRIMARY KEY,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    resource_id              UUID,
    operation                TEXT         NOT NULL,
    idempotency_key          VARCHAR(255) NOT NULL,
    request_fingerprint      CHAR(64)     NOT NULL,
    state                    TEXT         NOT NULL,
    response_status          SMALLINT,
    response_snapshot        JSONB,
    result_version           BIGINT,
    result_resource_id       UUID,
    actor_service_account_id UUID         NOT NULL,
    actor_service            VARCHAR(64)  NOT NULL,
    request_id               VARCHAR(36)  NOT NULL,
    correlation_id           VARCHAR(37)  NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    completed_at             TIMESTAMPTZ,
    CONSTRAINT uq_relay_commands_key UNIQUE (tenant_id, application_id, environment_id, operation, idempotency_key),
    CONSTRAINT ck_relay_commands_operation CHECK (operation IN (
        'CREATE_CONTEXT', 'UPDATE_CONTEXT', 'SUSPEND_CONTEXT', 'RESUME_CONTEXT', 'COMPLETE_CONTEXT',
        'REVOKE_CONTEXT', 'CREATE_SESSION', 'SUSPEND_SESSION', 'RESUME_SESSION', 'REVOKE_SESSION')),
    CONSTRAINT ck_relay_commands_key CHECK (idempotency_key ~ '^[\x21-\x7e]{1,255}$'),
    CONSTRAINT ck_relay_commands_fingerprint CHECK (request_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_relay_commands_state CHECK (state IN ('IN_PROGRESS', 'COMPLETED')),
    CONSTRAINT ck_relay_commands_completed CHECK (
        (state = 'COMPLETED') = (completed_at IS NOT NULL)
        AND (state = 'COMPLETED') = (response_snapshot IS NOT NULL)
        AND (state = 'COMPLETED') = (response_status IS NOT NULL)),
    CONSTRAINT ck_relay_commands_snapshot CHECK (
        response_snapshot IS NULL OR privacy_relay_safe_document(response_snapshot)),
    CONSTRAINT ck_relay_commands_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$')
);
CREATE INDEX idx_relay_commands_resource ON privacy_relay_commands (resource_id) WHERE resource_id IS NOT NULL;
CREATE TRIGGER trg_relay_commands_immutable
    BEFORE UPDATE ON privacy_relay_commands
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'operation', 'idempotency_key',
        'request_fingerprint', 'actor_service_account_id', 'actor_service', 'request_id',
        'correlation_id', 'created_at');
CREATE TRIGGER trg_relay_commands_set_once
    BEFORE UPDATE ON privacy_relay_commands
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment(
        'resource_id', 'response_status', 'response_snapshot', 'result_version', 'result_resource_id', 'completed_at');
CREATE TRIGGER trg_relay_commands_no_delete
    BEFORE DELETE ON privacy_relay_commands
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Durable enforcement jobs.
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_enforcement_jobs (
    id                              UUID PRIMARY KEY,
    enforcement_event_id            UUID        NOT NULL,
    tenant_id                       UUID        NOT NULL,
    application_id                  UUID        NOT NULL,
    environment_id                  UUID        NOT NULL,
    environment_kind                TEXT        NOT NULL,
    on_behalf_of_service_account_id UUID        NOT NULL,
    context_id                      UUID        NOT NULL,
    context_version                 BIGINT      NOT NULL,
    relay_session_id                UUID,
    session_version                 BIGINT,
    reason                          TEXT        NOT NULL,
    affected_principal_refs         UUID[]      NOT NULL,
    terminate_active_calls          BOOLEAN     NOT NULL,
    payload                         JSONB       NOT NULL,
    status                          TEXT        NOT NULL,
    attempt_count                   INTEGER     NOT NULL,
    next_attempt_at                 TIMESTAMPTZ NOT NULL,
    lease_owner                     VARCHAR(64),
    lease_expires_at                TIMESTAMPTZ,
    last_error_category             TEXT,
    result_snapshot                 JSONB,
    request_id                      VARCHAR(36) NOT NULL,
    correlation_id                  VARCHAR(37) NOT NULL,
    created_at                      TIMESTAMPTZ NOT NULL,
    updated_at                      TIMESTAMPTZ NOT NULL,
    completed_at                    TIMESTAMPTZ,
    CONSTRAINT fk_enforcement_jobs_context
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id)
        REFERENCES communication_contexts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_enforcement_jobs_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, relay_session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_enforcement_jobs_event UNIQUE (enforcement_event_id),
    CONSTRAINT ck_enforcement_jobs_environment_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_enforcement_jobs_versions CHECK (
        context_version >= 1 AND (relay_session_id IS NULL) = (session_version IS NULL)),
    CONSTRAINT ck_enforcement_jobs_reason CHECK (reason IN (
        'CONTEXT_REVOKED', 'CONTEXT_SUSPENDED', 'CONTEXT_COMPLETED', 'CONTEXT_EXPIRED',
        'PARTICIPANT_REMOVED', 'SESSION_REVOKED', 'SESSION_SUSPENDED', 'SESSION_EXPIRED')),
    CONSTRAINT ck_enforcement_jobs_principals CHECK (cardinality(affected_principal_refs) <= 256),
    CONSTRAINT ck_enforcement_jobs_status CHECK (status IN ('PENDING', 'APPLIED')),
    CONSTRAINT ck_enforcement_jobs_attempts CHECK (attempt_count >= 0),
    CONSTRAINT ck_enforcement_jobs_applied CHECK ((status = 'APPLIED') = (completed_at IS NOT NULL)),
    CONSTRAINT ck_enforcement_jobs_lease CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CONSTRAINT ck_enforcement_jobs_error CHECK (last_error_category IS NULL OR last_error_category IN (
        'TIMEOUT', 'UNAVAILABLE', 'REJECTED', 'INVALID_RESPONSE', 'NOT_CONFIGURED')),
    CONSTRAINT ck_enforcement_jobs_payload CHECK (privacy_relay_safe_document(payload)),
    CONSTRAINT ck_enforcement_jobs_result CHECK (result_snapshot IS NULL OR privacy_relay_safe_document(result_snapshot)),
    CONSTRAINT ck_enforcement_jobs_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$')
);
-- One job per authority change: per context version for context-level changes, per
-- session version for session-level changes.
CREATE UNIQUE INDEX uq_enforcement_jobs_context_change
    ON privacy_relay_enforcement_jobs (context_id, context_version, reason) WHERE relay_session_id IS NULL;
CREATE UNIQUE INDEX uq_enforcement_jobs_session_change
    ON privacy_relay_enforcement_jobs (relay_session_id, session_version, reason) WHERE relay_session_id IS NOT NULL;
CREATE INDEX idx_enforcement_jobs_due ON privacy_relay_enforcement_jobs (next_attempt_at) WHERE status = 'PENDING';
CREATE INDEX idx_enforcement_jobs_context ON privacy_relay_enforcement_jobs (context_id, created_at);

-- APPLIED is terminal; the command itself never changes.
CREATE FUNCTION privacy_relay_guard_enforcement_job() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'APPLIED' AND NEW.status <> 'APPLIED' THEN
        RAISE EXCEPTION 'privacy relay invariant violated: an applied enforcement job is final'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.attempt_count < OLD.attempt_count THEN
        RAISE EXCEPTION 'privacy relay invariant violated: enforcement attempt counts never decrease'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_enforcement_jobs_guard
    BEFORE UPDATE ON privacy_relay_enforcement_jobs
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_enforcement_job();
CREATE TRIGGER trg_enforcement_jobs_immutable
    BEFORE UPDATE ON privacy_relay_enforcement_jobs
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'enforcement_event_id', 'tenant_id', 'application_id', 'environment_id', 'environment_kind',
        'on_behalf_of_service_account_id', 'context_id', 'context_version', 'relay_session_id',
        'session_version', 'reason', 'affected_principal_refs', 'terminate_active_calls', 'payload',
        'request_id', 'correlation_id', 'created_at');
CREATE TRIGGER trg_enforcement_jobs_no_delete
    BEFORE DELETE ON privacy_relay_enforcement_jobs
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

ALTER TABLE relay_audit_events
    ADD CONSTRAINT fk_relay_audit_enforcement_job FOREIGN KEY (enforcement_job_id)
        REFERENCES privacy_relay_enforcement_jobs (id) ON DELETE RESTRICT;
"#;

const DOWN: &str = r#"
ALTER TABLE relay_audit_events DROP CONSTRAINT IF EXISTS fk_relay_audit_enforcement_job;
DROP TABLE IF EXISTS privacy_relay_enforcement_jobs;
DROP FUNCTION IF EXISTS privacy_relay_guard_enforcement_job();
DROP TABLE IF EXISTS privacy_relay_commands;
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

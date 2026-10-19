//! Part V H §103 — durable pushes of communication-context authority to Messaging.
//!
//! Messaging gates conversations of a protected-communication context on the
//! authority Privacy Relay announces (ACTIVE / SUSPENDED / REVOKED, fenced by the context
//! version). Every context version that matters to Messaging gets exactly one job,
//! enqueued by a DEFERRED constraint trigger at commit of the transaction that created
//! or changed the context — so no code path (create, update, lifecycle, worker expiry)
//! can forget it, and the channel set is read in its final committed form.
//!
//! A job is created when the context has the MESSAGING channel enabled, or when an earlier
//! job exists for it (so a disabled channel or a terminal state still reaches Messaging).
//! Jobs carry opaque ids and states only — never a contact value (`privacy_relay_safe_document`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE privacy_relay_messaging_authority_jobs (
    id                               UUID         PRIMARY KEY,
    tenant_id                        UUID         NOT NULL,
    application_id                   UUID         NOT NULL,
    environment_id                   UUID         NOT NULL,
    environment_kind                 TEXT         NOT NULL,
    on_behalf_of_service_account_id  UUID         NOT NULL,
    context_id                       UUID         NOT NULL,
    context_version                  BIGINT       NOT NULL,
    authority_state                  TEXT         NOT NULL,
    reason                           TEXT         NOT NULL,
    status                           TEXT         NOT NULL DEFAULT 'PENDING',
    attempt_count                    INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at                  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    lease_owner                      TEXT,
    lease_expires_at                 TIMESTAMPTZ,
    last_error_category              TEXT,
    result_snapshot                  JSONB,
    created_at                       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at                       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    completed_at                     TIMESTAMPTZ,
    CONSTRAINT uq_messaging_authority_jobs_version UNIQUE (context_id, context_version),
    CONSTRAINT fk_messaging_authority_jobs_context
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id)
        REFERENCES communication_contexts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_messaging_authority_jobs_state CHECK (authority_state IN ('ACTIVE', 'SUSPENDED', 'REVOKED')),
    CONSTRAINT ck_messaging_authority_jobs_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_authority_jobs_status CHECK (status IN ('PENDING', 'DELIVERING', 'APPLIED', 'DEAD_LETTER')),
    CONSTRAINT ck_messaging_authority_jobs_attempts CHECK (attempt_count >= 0),
    CONSTRAINT ck_messaging_authority_jobs_error CHECK (last_error_category IS NULL OR last_error_category IN
        ('TIMEOUT', 'UNAVAILABLE', 'REJECTED', 'INVALID_RESPONSE', 'NOT_CONFIGURED')),
    CONSTRAINT ck_messaging_authority_jobs_result CHECK (result_snapshot IS NULL OR privacy_relay_safe_document(result_snapshot))
);
CREATE INDEX idx_messaging_authority_jobs_due ON privacy_relay_messaging_authority_jobs (next_attempt_at)
    WHERE status IN ('PENDING', 'DELIVERING');

CREATE FUNCTION privacy_relay_guard_messaging_authority_job() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        -- Only the retention worker removes them, with their purged context.
        IF current_setting('privacy_relay.retention_worker', true) = 'on' THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'privacy relay: messaging authority jobs are deleted by retention only';
    END IF;
    IF OLD.status IN ('APPLIED', 'DEAD_LETTER') AND NEW.status <> OLD.status THEN
        RAISE EXCEPTION 'privacy relay: a terminal messaging authority job is final';
    END IF;
    IF NEW.attempt_count < OLD.attempt_count OR NEW.context_version <> OLD.context_version
       OR NEW.authority_state <> OLD.authority_state OR NEW.context_id <> OLD.context_id THEN
        RAISE EXCEPTION 'privacy relay: messaging authority job facts are immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_privacy_relay_guard_messaging_authority_job
    BEFORE UPDATE OR DELETE ON privacy_relay_messaging_authority_jobs
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_messaging_authority_job();

-- At COMMIT: one job per context version (channels read in their final form).
CREATE FUNCTION privacy_relay_enqueue_messaging_authority() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    c communication_contexts%ROWTYPE;
    messaging BOOLEAN;
    known BOOLEAN;
    target TEXT;
    why TEXT;
BEGIN
    SELECT * INTO c FROM communication_contexts WHERE id = NEW.id;
    SELECT EXISTS (SELECT 1 FROM communication_context_channels
                    WHERE context_id = c.id AND channel = 'MESSAGING' AND enabled) INTO messaging;
    SELECT EXISTS (SELECT 1 FROM privacy_relay_messaging_authority_jobs WHERE context_id = c.id) INTO known;
    IF NOT messaging AND NOT known THEN
        RETURN NULL;
    END IF;
    IF c.status IN ('REVOKED', 'COMPLETED', 'EXPIRED') THEN
        target := 'REVOKED';
        why := 'CONTEXT_' || c.status;
    ELSIF c.status = 'SUSPENDED' THEN
        target := 'SUSPENDED';
        why := 'CONTEXT_SUSPENDED';
    ELSIF NOT messaging THEN
        target := 'SUSPENDED';
        why := 'MESSAGING_CHANNEL_DISABLED';
    ELSE
        target := 'ACTIVE';
        why := 'CONTEXT_ACTIVE';
    END IF;
    INSERT INTO privacy_relay_messaging_authority_jobs (id, tenant_id, application_id, environment_id, environment_kind,
        on_behalf_of_service_account_id, context_id, context_version, authority_state, reason)
    VALUES (gen_random_uuid(), c.tenant_id, c.application_id, c.environment_id, c.environment_kind,
        c.created_by_service_account_id, c.id, c.version, target, why)
    ON CONFLICT (context_id, context_version) DO NOTHING;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER trg_privacy_relay_enqueue_messaging_authority
    AFTER INSERT OR UPDATE OF status, version ON communication_contexts
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_enqueue_messaging_authority();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_privacy_relay_enqueue_messaging_authority ON communication_contexts;
DROP FUNCTION IF EXISTS privacy_relay_enqueue_messaging_authority();
DROP TABLE IF EXISTS privacy_relay_messaging_authority_jobs;
DROP FUNCTION IF EXISTS privacy_relay_guard_messaging_authority_job();
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

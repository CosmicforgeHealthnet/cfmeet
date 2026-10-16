//! Operational incidents, operator audit and bounded enforcement retries (§81, §86).
//!
//! * `privacy_relay_incidents`: conditions Privacy Relay refuses to guess about
//!   (dead-lettered outbox events / enforcement jobs, a contact key version the keyring
//!   cannot read, more than one ACTIVE relay mapping for one number). Deduplicated while
//!   OPEN by a stable key (a repeat bumps `occurrences` and `last_seen_at`); OPEN ->
//!   RESOLVED only, RESOLVED is final. The summary is a bounded, safe sentence — never a
//!   telephone number, a ContactReference, a secret or key material (the CHECK refuses
//!   digit runs, `+<digit>`, `@` and SIP URIs as a backstop). Rows are never deleted
//!   outside a retention transaction and never truncated.
//! * `privacy_relay_operator_audit`: append-only evidence of every mutating operator
//!   action (`privacy-relay-admin incidents resolve`), written in the SAME transaction as
//!   its effect, naming the operator (`--operator` / `CF_OPERATOR_ID`).
//! * `privacy_relay_enforcement_jobs` gains DEAD_LETTER: after a capped number of failed
//!   delivery attempts a job stops retrying (it stays visible and raises an incident).
//!   APPLIED and DEAD_LETTER are final. Additive: existing rows are untouched.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE privacy_relay_incidents (
    id             UUID PRIMARY KEY,
    dedupe_key     VARCHAR(160) NOT NULL,
    category       TEXT         NOT NULL,
    severity       TEXT         NOT NULL,
    status         TEXT         NOT NULL,
    summary        VARCHAR(240) NOT NULL,
    occurrences    INTEGER      NOT NULL DEFAULT 1,
    first_seen_at  TIMESTAMPTZ  NOT NULL,
    last_seen_at   TIMESTAMPTZ  NOT NULL,
    resolved_at    TIMESTAMPTZ,
    resolved_by    VARCHAR(64),
    CONSTRAINT ck_relay_incidents_category CHECK (category IN (
        'OUTBOX_DEAD_LETTER', 'ENFORCEMENT_DEAD_LETTER', 'CONTACT_KEY_UNAVAILABLE',
        'RELAY_MAPPING_AMBIGUOUS')),
    CONSTRAINT ck_relay_incidents_severity CHECK (severity IN ('LOW', 'MEDIUM', 'HIGH', 'CRITICAL')),
    CONSTRAINT ck_relay_incidents_status CHECK (status IN ('OPEN', 'RESOLVED')),
    CONSTRAINT ck_relay_incidents_dedupe CHECK (
        dedupe_key ~ '^[A-Z][A-Z_]{1,63}(:[A-Za-z0-9._-]{1,64}){0,3}$'),
    CONSTRAINT ck_relay_incidents_summary CHECK (
        length(summary) BETWEEN 1 AND 240
        AND summary !~ '[0-9]{6,}' AND summary !~ '\+[0-9]' AND summary !~ '@'
        AND summary !~* 'sip:'),
    CONSTRAINT ck_relay_incidents_counts CHECK (occurrences >= 1),
    CONSTRAINT ck_relay_incidents_seen CHECK (last_seen_at >= first_seen_at),
    CONSTRAINT ck_relay_incidents_resolved CHECK (
        (status = 'RESOLVED') = (resolved_at IS NOT NULL)
        AND (status = 'RESOLVED') = (resolved_by IS NOT NULL)),
    CONSTRAINT ck_relay_incidents_resolved_by CHECK (
        resolved_by IS NULL OR resolved_by ~ '^[A-Za-z0-9][A-Za-z0-9._@:-]{1,63}$')
);
CREATE UNIQUE INDEX uq_relay_incidents_open ON privacy_relay_incidents (dedupe_key) WHERE status = 'OPEN';
CREATE INDEX idx_relay_incidents_status ON privacy_relay_incidents (status, last_seen_at);

-- OPEN -> RESOLVED only; RESOLVED is final.
CREATE FUNCTION privacy_relay_guard_incident() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'RESOLVED' THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a resolved incident is final'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.occurrences < OLD.occurrences THEN
        RAISE EXCEPTION 'privacy relay invariant violated: incident occurrences never decrease'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_relay_incidents_guard
    BEFORE UPDATE ON privacy_relay_incidents
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_incident();
CREATE TRIGGER trg_relay_incidents_immutable
    BEFORE UPDATE ON privacy_relay_incidents
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'dedupe_key', 'category', 'first_seen_at');
CREATE TRIGGER trg_relay_incidents_no_delete
    BEFORE DELETE ON privacy_relay_incidents
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
CREATE TRIGGER trg_relay_incidents_no_truncate
    BEFORE TRUNCATE ON privacy_relay_incidents
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();

-- ---------------------------------------------------------------------------
-- Operator audit (append-only for everyone).
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_operator_audit (
    id           UUID PRIMARY KEY,
    actor        VARCHAR(64)  NOT NULL,
    action       TEXT         NOT NULL,
    target_kind  TEXT         NOT NULL,
    target_id    UUID         NOT NULL,
    detail       JSONB        NOT NULL,
    occurred_at  TIMESTAMPTZ  NOT NULL,
    CONSTRAINT ck_relay_operator_audit_actor CHECK (actor ~ '^[A-Za-z0-9][A-Za-z0-9._@:-]{1,63}$'),
    CONSTRAINT ck_relay_operator_audit_action CHECK (action IN ('INCIDENT_RESOLVE')),
    CONSTRAINT ck_relay_operator_audit_target CHECK (target_kind IN ('INCIDENT')),
    CONSTRAINT ck_relay_operator_audit_detail CHECK (
        jsonb_typeof(detail) = 'object' AND octet_length(detail::text) <= 2048
        AND privacy_relay_safe_document(detail))
);
CREATE INDEX idx_relay_operator_audit_target ON privacy_relay_operator_audit (target_id, occurred_at);
CREATE TRIGGER trg_relay_operator_audit_append_only
    BEFORE UPDATE OR DELETE ON privacy_relay_operator_audit
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation_always();
CREATE TRIGGER trg_relay_operator_audit_no_truncate
    BEFORE TRUNCATE ON privacy_relay_operator_audit
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();

-- ---------------------------------------------------------------------------
-- Enforcement dead letter.
-- ---------------------------------------------------------------------------
ALTER TABLE privacy_relay_enforcement_jobs
    ADD COLUMN dead_lettered_at TIMESTAMPTZ,
    DROP CONSTRAINT ck_enforcement_jobs_status,
    ADD CONSTRAINT ck_enforcement_jobs_status CHECK (status IN ('PENDING', 'APPLIED', 'DEAD_LETTER')),
    ADD CONSTRAINT ck_enforcement_jobs_dead_letter CHECK (
        (status = 'DEAD_LETTER') = (dead_lettered_at IS NOT NULL)),
    DROP CONSTRAINT ck_enforcement_jobs_error,
    ADD CONSTRAINT ck_enforcement_jobs_error CHECK (last_error_category IS NULL OR last_error_category IN (
        'TIMEOUT', 'UNAVAILABLE', 'REJECTED', 'INVALID_RESPONSE', 'NOT_CONFIGURED', 'MALFORMED_JOB'));
CREATE INDEX idx_enforcement_jobs_dead_letter ON privacy_relay_enforcement_jobs (dead_lettered_at)
    WHERE status = 'DEAD_LETTER';

-- APPLIED and DEAD_LETTER are final; the command itself never changes.
CREATE OR REPLACE FUNCTION privacy_relay_guard_enforcement_job() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status IN ('APPLIED', 'DEAD_LETTER') AND NEW.status <> OLD.status THEN
        RAISE EXCEPTION 'privacy relay invariant violated: an applied or dead-lettered enforcement job is final'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.attempt_count < OLD.attempt_count THEN
        RAISE EXCEPTION 'privacy relay invariant violated: enforcement attempt counts never decrease'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
"#;

const DOWN: &str = r#"
-- The previous schema has no dead letter: dead-lettered jobs become due PENDING jobs
-- again (enforcement is restrictive; retrying it is always safe).
CREATE OR REPLACE FUNCTION privacy_relay_guard_enforcement_job() RETURNS trigger
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
DROP INDEX IF EXISTS idx_enforcement_jobs_dead_letter;
ALTER TABLE privacy_relay_enforcement_jobs
    DROP CONSTRAINT ck_enforcement_jobs_dead_letter,
    DROP CONSTRAINT ck_enforcement_jobs_status,
    DROP CONSTRAINT ck_enforcement_jobs_error;
UPDATE privacy_relay_enforcement_jobs
   SET status = 'PENDING', lease_owner = NULL, lease_expires_at = NULL,
       last_error_category = CASE WHEN last_error_category = 'MALFORMED_JOB' THEN 'INVALID_RESPONSE'
                                  ELSE last_error_category END
 WHERE status = 'DEAD_LETTER' OR last_error_category = 'MALFORMED_JOB';
ALTER TABLE privacy_relay_enforcement_jobs
    DROP COLUMN dead_lettered_at,
    ADD CONSTRAINT ck_enforcement_jobs_status CHECK (status IN ('PENDING', 'APPLIED')),
    ADD CONSTRAINT ck_enforcement_jobs_error CHECK (last_error_category IS NULL OR last_error_category IN (
        'TIMEOUT', 'UNAVAILABLE', 'REJECTED', 'INVALID_RESPONSE', 'NOT_CONFIGURED'));
DROP TABLE IF EXISTS privacy_relay_operator_audit;
DROP TABLE IF EXISTS privacy_relay_incidents;
DROP FUNCTION IF EXISTS privacy_relay_guard_incident();
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

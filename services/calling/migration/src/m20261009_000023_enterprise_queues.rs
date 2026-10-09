//! Call queues (Part F §56, §47–§55).
//!
//! * `enterprise_call_queues` / `enterprise_queue_members` — a queue of agents
//!   (principals) with a maximum wait, a per-offer timeout, a capacity and an optional
//!   overflow destination. Agents may be PAUSED without leaving the queue.
//! * `enterprise_queue_entries` — one waiting canonical Call per row (the Call itself
//!   stays in `calls`). FIFO order is `(entered_at, seq)`; a Call holds at most ONE
//!   active entry (WAITING / OFFERING). Terminal entry states are final.
//! * `enterprise_queue_offers` — each offer of a waiting Call to one agent. At most one
//!   PENDING offer per entry and per agent (an agent is never offered two Calls at once,
//!   and two workers can never offer one entry twice), at most one ACCEPTED offer per
//!   entry. Offer outcomes are final.
//!
//! The answer itself is arbitrated by the existing invitation / answer-claim machinery:
//! every offer of an entry carries the entry's `answer_group_id`, so the group claim of
//! `call_answer_group_claims` admits exactly one answering agent per queued Call.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE enterprise_call_queues (
    id                    UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    name                  VARCHAR(100) NOT NULL,
    max_wait_seconds      INTEGER      NOT NULL,
    offer_timeout_seconds INTEGER      NOT NULL,
    capacity              INTEGER      NOT NULL,
    overflow_type         TEXT,
    overflow_id           UUID,
    status                TEXT         NOT NULL,
    version               BIGINT       NOT NULL,
    created_at            TIMESTAMPTZ  NOT NULL,
    updated_at            TIMESTAMPTZ  NOT NULL,
    disabled_at           TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_call_queues PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_call_queues_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_call_queues_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_call_queues_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_call_queues_max_wait CHECK (max_wait_seconds BETWEEN 10 AND 3600),
    CONSTRAINT ck_enterprise_call_queues_offer_timeout
        CHECK (offer_timeout_seconds BETWEEN 5 AND 120 AND offer_timeout_seconds <= max_wait_seconds),
    CONSTRAINT ck_enterprise_call_queues_capacity CHECK (capacity BETWEEN 1 AND 1000),
    CONSTRAINT ck_enterprise_call_queues_overflow CHECK ((overflow_type IS NULL) = (overflow_id IS NULL)
        AND (overflow_type IS NULL
             OR overflow_type IN ('EXTENSION', 'RING_GROUP', 'QUEUE', 'IVR', 'VOICEMAIL'))
        AND (overflow_id IS NULL OR overflow_id <> id)),
    CONSTRAINT ck_enterprise_call_queues_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_call_queues_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_call_queues_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_call_queues_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_call_queues_active_name
    ON enterprise_call_queues (environment_id, lower(name)) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_call_queues_scope
    ON enterprise_call_queues (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_call_queues_immutable
    BEFORE UPDATE ON enterprise_call_queues
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_enterprise_call_queues_version
    BEFORE UPDATE ON enterprise_call_queues
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_enterprise_call_queues_overflow
    BEFORE INSERT OR UPDATE ON enterprise_call_queues
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('overflow_type', 'overflow_id');

CREATE TABLE enterprise_queue_members (
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    queue_id       UUID        NOT NULL,
    position       SMALLINT    NOT NULL,
    principal_id   UUID        NOT NULL,
    status         TEXT        NOT NULL,
    added_at       TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_enterprise_queue_members PRIMARY KEY (queue_id, position),
    CONSTRAINT uq_enterprise_queue_members_principal UNIQUE (queue_id, principal_id),
    CONSTRAINT fk_enterprise_queue_members_queue
        FOREIGN KEY (tenant_id, application_id, environment_id, queue_id)
        REFERENCES enterprise_call_queues (tenant_id, application_id, environment_id, id)
        ON DELETE CASCADE,
    CONSTRAINT fk_enterprise_queue_members_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_queue_members_position CHECK (position BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_queue_members_status CHECK (status IN ('ACTIVE', 'PAUSED'))
);
CREATE INDEX idx_enterprise_queue_members_principal
    ON enterprise_queue_members (tenant_id, application_id, environment_id, principal_id);

CREATE TABLE enterprise_queue_entries (
    id                    UUID        NOT NULL,
    tenant_id             UUID        NOT NULL,
    application_id        UUID        NOT NULL,
    environment_id        UUID        NOT NULL,
    queue_id              UUID        NOT NULL,
    call_id               UUID        NOT NULL,
    seq                   BIGINT      GENERATED ALWAYS AS IDENTITY,
    state                 TEXT        NOT NULL,
    entered_at            TIMESTAMPTZ NOT NULL,
    deadline_at           TIMESTAMPTZ NOT NULL,
    answer_group_id       UUID        NOT NULL,
    offer_count           INTEGER     NOT NULL,
    assigned_principal_id UUID,
    exited_at             TIMESTAMPTZ,
    version               BIGINT      NOT NULL,
    CONSTRAINT pk_enterprise_queue_entries PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_queue_entries_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_enterprise_queue_entries_answer_group UNIQUE (answer_group_id),
    CONSTRAINT fk_enterprise_queue_entries_queue
        FOREIGN KEY (tenant_id, application_id, environment_id, queue_id)
        REFERENCES enterprise_call_queues (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_queue_entries_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_queue_entries_assigned
        FOREIGN KEY (tenant_id, application_id, environment_id, assigned_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_queue_entries_state CHECK (state IN (
        'WAITING', 'OFFERING', 'ANSWERED', 'ABANDONED', 'TIMED_OUT', 'OVERFLOWED', 'CANCELLED')),
    CONSTRAINT ck_enterprise_queue_entries_exit
        CHECK ((state IN ('WAITING', 'OFFERING')) = (exited_at IS NULL)),
    CONSTRAINT ck_enterprise_queue_entries_answered
        CHECK ((state = 'ANSWERED') = (assigned_principal_id IS NOT NULL)),
    CONSTRAINT ck_enterprise_queue_entries_deadline CHECK (deadline_at > entered_at),
    CONSTRAINT ck_enterprise_queue_entries_offers CHECK (offer_count >= 0),
    CONSTRAINT ck_enterprise_queue_entries_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_queue_entries_exited CHECK (exited_at IS NULL OR exited_at >= entered_at)
);
-- One active entry per Call; FIFO order of the active entries of a queue.
CREATE UNIQUE INDEX uq_enterprise_queue_entries_active_call
    ON enterprise_queue_entries (call_id) WHERE state IN ('WAITING', 'OFFERING');
CREATE INDEX idx_enterprise_queue_entries_fifo
    ON enterprise_queue_entries (queue_id, entered_at, seq) WHERE state IN ('WAITING', 'OFFERING');
CREATE INDEX idx_enterprise_queue_entries_deadline
    ON enterprise_queue_entries (deadline_at) WHERE state IN ('WAITING', 'OFFERING');
CREATE INDEX idx_enterprise_queue_entries_call ON enterprise_queue_entries (call_id);
CREATE TRIGGER trg_enterprise_queue_entries_immutable
    BEFORE UPDATE ON enterprise_queue_entries
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'queue_id', 'call_id', 'seq',
        'entered_at', 'deadline_at', 'answer_group_id');
CREATE TRIGGER trg_enterprise_queue_entries_version
    BEFORE UPDATE ON enterprise_queue_entries
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

-- A terminal entry (or offer) never changes again.
CREATE FUNCTION enterprise_queue_final_state() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    old_state TEXT := to_jsonb(OLD) ->> TG_ARGV[0];
    new_state TEXT := to_jsonb(NEW) ->> TG_ARGV[0];
BEGIN
    IF old_state <> ALL (string_to_array(TG_ARGV[1], ',')) AND old_state IS DISTINCT FROM new_state THEN
        RAISE EXCEPTION 'enterprise invariant violated: %.% % is final', TG_TABLE_NAME, TG_ARGV[0], old_state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_enterprise_queue_entries_final
    BEFORE UPDATE ON enterprise_queue_entries
    FOR EACH ROW EXECUTE FUNCTION enterprise_queue_final_state('state', 'WAITING,OFFERING');

CREATE TABLE enterprise_queue_offers (
    id             UUID        NOT NULL,
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    entry_id       UUID        NOT NULL,
    queue_id       UUID        NOT NULL,
    call_id        UUID        NOT NULL,
    principal_id   UUID        NOT NULL,
    generation     INTEGER     NOT NULL,
    outcome        TEXT        NOT NULL,
    offered_at     TIMESTAMPTZ NOT NULL,
    expires_at     TIMESTAMPTZ NOT NULL,
    resolved_at    TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_queue_offers PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_queue_offers_generation UNIQUE (entry_id, generation),
    CONSTRAINT fk_enterprise_queue_offers_entry
        FOREIGN KEY (tenant_id, application_id, environment_id, entry_id)
        REFERENCES enterprise_queue_entries (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_queue_offers_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_queue_offers_generation CHECK (generation >= 1),
    CONSTRAINT ck_enterprise_queue_offers_outcome
        CHECK (outcome IN ('PENDING', 'ACCEPTED', 'EXPIRED', 'DECLINED', 'CANCELLED')),
    CONSTRAINT ck_enterprise_queue_offers_resolved
        CHECK ((outcome = 'PENDING') = (resolved_at IS NULL)),
    CONSTRAINT ck_enterprise_queue_offers_expiry CHECK (expires_at > offered_at)
);
CREATE UNIQUE INDEX uq_enterprise_queue_offers_entry_pending
    ON enterprise_queue_offers (entry_id) WHERE outcome = 'PENDING';
CREATE UNIQUE INDEX uq_enterprise_queue_offers_agent_pending
    ON enterprise_queue_offers (tenant_id, application_id, environment_id, principal_id)
    WHERE outcome = 'PENDING';
CREATE UNIQUE INDEX uq_enterprise_queue_offers_entry_accepted
    ON enterprise_queue_offers (entry_id) WHERE outcome = 'ACCEPTED';
CREATE INDEX idx_enterprise_queue_offers_agent_recent
    ON enterprise_queue_offers (queue_id, principal_id, offered_at DESC);
CREATE TRIGGER trg_enterprise_queue_offers_immutable
    BEFORE UPDATE ON enterprise_queue_offers
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'entry_id', 'queue_id', 'call_id',
        'principal_id', 'generation', 'offered_at', 'expires_at');
CREATE TRIGGER trg_enterprise_queue_offers_final
    BEFORE UPDATE ON enterprise_queue_offers
    FOR EACH ROW EXECUTE FUNCTION enterprise_queue_final_state('outcome', 'PENDING');
CREATE TRIGGER trg_enterprise_queue_offers_no_delete
    BEFORE DELETE ON enterprise_queue_offers
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

-- A full queue without overflow ends the Call BUSY.
ALTER TABLE enterprise_call_contexts
    DROP CONSTRAINT ck_enterprise_call_contexts_outcome,
    ADD CONSTRAINT ck_enterprise_call_contexts_outcome CHECK (outcome IS NULL OR outcome IN (
        'ANSWERED', 'VOICEMAIL', 'NO_ANSWER', 'DECLINED', 'ABANDONED', 'FAILED', 'FORWARDED_PUBLIC',
        'BUSY')),
    ADD COLUMN queue_entry_id UUID,
    ADD CONSTRAINT fk_enterprise_call_contexts_queue_entry
        FOREIGN KEY (tenant_id, application_id, environment_id, queue_entry_id)
        REFERENCES enterprise_queue_entries (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT;

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
        ELSE FALSE
    END
$$;
"#;

const DOWN: &str = r#"
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
        ELSE FALSE
    END
$$;
ALTER TABLE enterprise_call_contexts
    DROP CONSTRAINT IF EXISTS fk_enterprise_call_contexts_queue_entry,
    DROP COLUMN IF EXISTS queue_entry_id,
    DROP CONSTRAINT ck_enterprise_call_contexts_outcome,
    ADD CONSTRAINT ck_enterprise_call_contexts_outcome CHECK (outcome IS NULL OR outcome IN (
        'ANSWERED', 'VOICEMAIL', 'NO_ANSWER', 'DECLINED', 'ABANDONED', 'FAILED', 'FORWARDED_PUBLIC'));
DROP TABLE IF EXISTS enterprise_queue_offers;
DROP TABLE IF EXISTS enterprise_queue_entries;
DROP FUNCTION IF EXISTS enterprise_queue_final_state();
DROP TABLE IF EXISTS enterprise_queue_members;
DROP TABLE IF EXISTS enterprise_call_queues;
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

//! Ring groups and group-level single-winner arbitration (Part F §55, §37–§46).
//!
//! * `enterprise_ring_groups` / `enterprise_ring_group_members` — a group of principals
//!   (directly, or through an extension that routes to a principal) rung SIMULTANEOUSLY
//!   or in ORDER, with a per-member ring time, an overall group timeout and an optional
//!   fallback destination. Groups are not nestable (a member never resolves to another
//!   group, queue or IVR), so a ring-group cycle cannot exist.
//! * A ring-group step rings several participants of ONE Call. The per-participant
//!   answer claim (`call_answer_claims`) admits one device per participant; the GROUP
//!   claim `call_answer_group_claims` (PRIMARY KEY (call_id, answer_group_id)) admits one
//!   answering participant per group: a second member can never win, even from a buggy
//!   writer. `call_invitations.answer_group_id` marks the offers of a group.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE enterprise_ring_groups (
    id                    UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    name                  VARCHAR(100) NOT NULL,
    strategy              TEXT         NOT NULL,
    member_ring_seconds   INTEGER      NOT NULL,
    group_timeout_seconds INTEGER      NOT NULL,
    fallback_type         TEXT,
    fallback_id           UUID,
    status                TEXT         NOT NULL,
    version               BIGINT       NOT NULL,
    created_at            TIMESTAMPTZ  NOT NULL,
    updated_at            TIMESTAMPTZ  NOT NULL,
    disabled_at           TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_ring_groups PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_ring_groups_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_ring_groups_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_ring_groups_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_ring_groups_strategy CHECK (strategy IN ('SIMULTANEOUS', 'ORDERED')),
    CONSTRAINT ck_enterprise_ring_groups_member_ring CHECK (member_ring_seconds BETWEEN 5 AND 120),
    CONSTRAINT ck_enterprise_ring_groups_timeout
        CHECK (group_timeout_seconds BETWEEN 10 AND 600 AND group_timeout_seconds >= member_ring_seconds),
    CONSTRAINT ck_enterprise_ring_groups_fallback CHECK ((fallback_type IS NULL) = (fallback_id IS NULL)
        AND (fallback_type IS NULL OR fallback_type IN ('EXTENSION', 'QUEUE', 'VOICEMAIL'))),
    CONSTRAINT ck_enterprise_ring_groups_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_ring_groups_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_ring_groups_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_ring_groups_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_ring_groups_active_name
    ON enterprise_ring_groups (environment_id, lower(name)) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_ring_groups_scope
    ON enterprise_ring_groups (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_ring_groups_immutable
    BEFORE UPDATE ON enterprise_ring_groups
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_enterprise_ring_groups_version
    BEFORE UPDATE ON enterprise_ring_groups
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_enterprise_ring_groups_fallback
    BEFORE INSERT OR UPDATE ON enterprise_ring_groups
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('fallback_type', 'fallback_id');

CREATE TABLE enterprise_ring_group_members (
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    ring_group_id  UUID        NOT NULL,
    position       SMALLINT    NOT NULL,
    member_type    TEXT        NOT NULL,
    member_id      UUID        NOT NULL,
    added_at       TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_enterprise_ring_group_members PRIMARY KEY (ring_group_id, position),
    CONSTRAINT uq_enterprise_ring_group_members_member UNIQUE (ring_group_id, member_type, member_id),
    CONSTRAINT fk_enterprise_ring_group_members_group
        FOREIGN KEY (tenant_id, application_id, environment_id, ring_group_id)
        REFERENCES enterprise_ring_groups (tenant_id, application_id, environment_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_enterprise_ring_group_members_position CHECK (position BETWEEN 1 AND 50),
    CONSTRAINT ck_enterprise_ring_group_members_type
        CHECK (member_type IN ('PRINCIPAL', 'ENTERPRISE_MEMBER', 'EXTENSION'))
);
CREATE TRIGGER trg_enterprise_ring_group_members_destination
    BEFORE INSERT OR UPDATE ON enterprise_ring_group_members
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('member_type', 'member_id');

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

-- Group-level single winner.
ALTER TABLE call_invitations ADD COLUMN answer_group_id UUID;
CREATE INDEX idx_call_invitations_answer_group ON call_invitations (call_id, answer_group_id)
    WHERE answer_group_id IS NOT NULL;
CREATE TRIGGER trg_call_invitations_answer_group_once
    BEFORE UPDATE ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('answer_group_id');

CREATE TABLE call_answer_group_claims (
    call_id         UUID        NOT NULL,
    answer_group_id UUID        NOT NULL,
    tenant_id       UUID        NOT NULL,
    application_id  UUID        NOT NULL,
    environment_id  UUID        NOT NULL,
    participant_id  UUID        NOT NULL,
    invitation_id   UUID        NOT NULL,
    claimed_at      TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_call_answer_group_claims PRIMARY KEY (call_id, answer_group_id),
    CONSTRAINT fk_call_answer_group_claims_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id)
        ON DELETE RESTRICT
);
CREATE TRIGGER trg_call_answer_group_claims_append_only
    BEFORE UPDATE OR DELETE ON call_answer_group_claims
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

-- Only the group's winning invitation can be ACCEPTED (defence in depth: a second
-- member's answer cannot commit even if application code skipped the group claim).
CREATE FUNCTION enterprise_invitations_require_group_claim() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.status = 'ACCEPTED' AND OLD.status IS DISTINCT FROM 'ACCEPTED'
       AND NEW.answer_group_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM call_answer_group_claims c
         WHERE c.call_id = NEW.call_id AND c.answer_group_id = NEW.answer_group_id
           AND c.invitation_id = NEW.id
    ) THEN
        RAISE EXCEPTION 'enterprise invariant violated: only the group winner is accepted'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_invitations_require_group_claim
    BEFORE UPDATE ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION enterprise_invitations_require_group_claim();

ALTER TABLE enterprise_call_contexts
    ADD COLUMN ring_group_id     UUID,
    ADD COLUMN ring_cursor       SMALLINT,
    ADD COLUMN answer_group_id   UUID,
    ADD COLUMN ring_deadline     TIMESTAMPTZ,
    ADD CONSTRAINT fk_enterprise_call_contexts_ring_group
        FOREIGN KEY (tenant_id, application_id, environment_id, ring_group_id)
        REFERENCES enterprise_ring_groups (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    ADD CONSTRAINT ck_enterprise_call_contexts_ring_cursor
        CHECK (ring_cursor IS NULL OR ring_cursor BETWEEN 1 AND 50);
"#;

const DOWN: &str = r#"
ALTER TABLE enterprise_call_contexts
    DROP CONSTRAINT IF EXISTS ck_enterprise_call_contexts_ring_cursor,
    DROP CONSTRAINT IF EXISTS fk_enterprise_call_contexts_ring_group,
    DROP COLUMN IF EXISTS ring_deadline,
    DROP COLUMN IF EXISTS answer_group_id,
    DROP COLUMN IF EXISTS ring_cursor,
    DROP COLUMN IF EXISTS ring_group_id;
DROP TRIGGER IF EXISTS trg_call_invitations_require_group_claim ON call_invitations;
DROP FUNCTION IF EXISTS enterprise_invitations_require_group_claim();
DROP TABLE IF EXISTS call_answer_group_claims;
DROP TRIGGER IF EXISTS trg_call_invitations_answer_group_once ON call_invitations;
DROP INDEX IF EXISTS idx_call_invitations_answer_group;
ALTER TABLE call_invitations DROP COLUMN IF EXISTS answer_group_id;
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
        ELSE FALSE
    END
$$;
DROP TABLE IF EXISTS enterprise_ring_group_members;
DROP TABLE IF EXISTS enterprise_ring_groups;
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

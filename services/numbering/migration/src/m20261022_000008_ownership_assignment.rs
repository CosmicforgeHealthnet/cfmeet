//! Workstream J: ownership and assignment.
//!
//! * **Internal transfers.** An ACTIVE assignment may be CLOSED only by an internal
//!   transfer to a successor assignment of the SAME legal owner (same owner kind and
//!   reference, same scope and number), recorded in the same transaction
//!   (`close_reason = 'TRANSFERRED_INTERNALLY'`, `successor_assignment_id`; the successor
//!   names its predecessor). Ownership stays immutable per record and the history keeps
//!   both. A different legal owner is never a transfer (release / quarantine are
//!   Workstream Q).
//! * **Routing targets.** Opaque target kinds for professional identities, shared teams,
//!   voicemail and approved forwarding (voice) and shared-team / department inboxes (SMS).
//! * **Organizational pools.** `number_pools` / `number_pool_members`: an organization's
//!   numbers grouped with usage / subscription policy references; a member is an
//!   organization-owned assignment of the pool's own scope, in at most one pool.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261022_000008_ownership_assignment"
    }
}

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Internal transfers: ACTIVE -> CLOSED only to a successor of the same legal owner
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION dcn_assignment_transition_allowed(from_status TEXT, to_status TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_status, to_status) IN (
        ('REQUESTED', 'PENDING_ELIGIBILITY'), ('REQUESTED', 'CLOSED'),
        ('PENDING_ELIGIBILITY', 'ELIGIBLE'), ('PENDING_ELIGIBILITY', 'CLOSED'),
        ('ELIGIBLE', 'RESERVED'), ('ELIGIBLE', 'CLOSED'),
        ('RESERVED', 'ACTIVATING'), ('RESERVED', 'CLOSED'),
        ('ACTIVATING', 'ACTIVE'), ('ACTIVATING', 'RESERVED'), ('ACTIVATING', 'CLOSED'),
        ('ACTIVE', 'SUSPENDED'), ('ACTIVE', 'RELEASE_REQUESTED'), ('ACTIVE', 'CLOSED'),
        ('SUSPENDED', 'ACTIVE'), ('SUSPENDED', 'RELEASE_REQUESTED'),
        ('RELEASE_REQUESTED', 'ROUTING_REMOVED'),
        ('ROUTING_REMOVED', 'QUARANTINED'), ('ROUTING_REMOVED', 'CLOSED'),
        ('QUARANTINED', 'CLOSED')
    )
$$;

ALTER TABLE dcn_assignments
    ADD COLUMN predecessor_assignment_id UUID,
    ADD COLUMN successor_assignment_id   UUID;
ALTER TABLE dcn_assignments
    ADD CONSTRAINT fk_dcn_assignments_predecessor FOREIGN KEY (predecessor_assignment_id) REFERENCES dcn_assignments (id),
    ADD CONSTRAINT fk_dcn_assignments_successor FOREIGN KEY (successor_assignment_id) REFERENCES dcn_assignments (id)
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT uq_dcn_assignments_successor UNIQUE (successor_assignment_id),
    ADD CONSTRAINT uq_dcn_assignments_predecessor UNIQUE (predecessor_assignment_id),
    ADD CONSTRAINT ck_dcn_assignments_transfer CHECK (
        (close_reason = 'TRANSFERRED_INTERNALLY') = (successor_assignment_id IS NOT NULL));

CREATE OR REPLACE FUNCTION dcn_guard_assignment() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    resource_state TEXT;
    predecessor RECORD;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'assignments are never deleted (reassignment creates a new record)' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM number_guard_environment_inventory(NEW.number_resource_id, NEW.environment_kind);
        IF NEW.status <> 'REQUESTED' OR NEW.revision <> 1 OR NEW.successor_assignment_id IS NOT NULL THEN
            RAISE EXCEPTION 'an assignment starts REQUESTED at revision 1' USING ERRCODE = '23514';
        END IF;
        IF NEW.predecessor_assignment_id IS NOT NULL THEN
            SELECT * INTO predecessor FROM dcn_assignments WHERE id = NEW.predecessor_assignment_id;
            IF NOT FOUND OR predecessor.status <> 'CLOSED' OR predecessor.close_reason <> 'TRANSFERRED_INTERNALLY'
               OR predecessor.successor_assignment_id IS DISTINCT FROM NEW.id
               OR predecessor.number_resource_id <> NEW.number_resource_id
               OR (predecessor.tenant_id, predecessor.application_id, predecessor.environment_id)
                  IS DISTINCT FROM (NEW.tenant_id, NEW.application_id, NEW.environment_id)
               OR predecessor.owner_kind <> NEW.owner_kind OR predecessor.owner_ref <> NEW.owner_ref THEN
                RAISE EXCEPTION 'an internal transfer keeps the legal owner, scope and number of its predecessor'
                    USING ERRCODE = '23514';
            END IF;
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id OR NEW.environment_kind IS DISTINCT FROM OLD.environment_kind
       OR NEW.owner_kind IS DISTINCT FROM OLD.owner_kind OR NEW.owner_ref IS DISTINCT FROM OLD.owner_ref
       OR NEW.ownership_class IS DISTINCT FROM OLD.ownership_class OR NEW.requested_at IS DISTINCT FROM OLD.requested_at
       OR NEW.created_by IS DISTINCT FROM OLD.created_by
       OR NEW.predecessor_assignment_id IS DISTINCT FROM OLD.predecessor_assignment_id THEN
        RAISE EXCEPTION 'assignment ownership is immutable; a new owner is a new assignment' USING ERRCODE = '42501';
    END IF;
    IF OLD.successor_assignment_id IS NOT NULL AND NEW.successor_assignment_id IS DISTINCT FROM OLD.successor_assignment_id THEN
        RAISE EXCEPTION 'a transfer is final' USING ERRCODE = '42501';
    END IF;
    IF OLD.reservation_id IS NOT NULL AND NEW.reservation_id IS DISTINCT FROM OLD.reservation_id THEN
        RAISE EXCEPTION 'an assignment keeps its reservation' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'assignment updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status THEN
        IF current_setting('numbering.assignment', true) IS DISTINCT FROM 'transition' THEN
            RAISE EXCEPTION 'assignment status changes only through a domain transition' USING ERRCODE = '42501';
        END IF;
        IF NOT dcn_assignment_transition_allowed(OLD.status, NEW.status) THEN
            RAISE EXCEPTION 'assignment transition % -> % is not allowed', OLD.status, NEW.status USING ERRCODE = '23514';
        END IF;
        IF OLD.status = 'ACTIVE' AND NEW.status = 'CLOSED'
           AND (NEW.close_reason IS DISTINCT FROM 'TRANSFERRED_INTERNALLY' OR NEW.successor_assignment_id IS NULL) THEN
            RAISE EXCEPTION 'an ACTIVE assignment closes only by an internal transfer to its successor' USING ERRCODE = '23514';
        END IF;
        IF NEW.status = 'ACTIVE' THEN
            SELECT state INTO resource_state FROM number_resources WHERE id = NEW.number_resource_id;
            IF resource_state <> 'ACTIVE' THEN
                RAISE EXCEPTION 'an assignment becomes ACTIVE only on an ACTIVE number resource' USING ERRCODE = '23514';
            END IF;
        END IF;
    ELSIF OLD.status = 'CLOSED' THEN
        RAISE EXCEPTION 'a CLOSED assignment is history' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;

-- ---------------------------------------------------------------------------
-- Routing target kinds
-- ---------------------------------------------------------------------------
ALTER TABLE number_routing_profiles DROP CONSTRAINT ck_number_routing_profiles_voice;
ALTER TABLE number_routing_profiles ADD CONSTRAINT ck_number_routing_profiles_voice CHECK (voice_target_kind IN
    ('NONE', 'PRINCIPAL', 'PROFESSIONAL_IDENTITY', 'CALLING_ENDPOINT', 'ENTERPRISE_EXTENSION', 'ENTERPRISE_RING_GROUP',
     'ENTERPRISE_QUEUE', 'ENTERPRISE_IVR', 'ENTERPRISE_DEPARTMENT', 'SHARED_TEAM', 'VOICEMAIL', 'APPROVED_FORWARDING')
    AND (voice_target_kind = 'NONE') = (voice_target_ref IS NULL));
ALTER TABLE number_routing_profiles DROP CONSTRAINT ck_number_routing_profiles_sms;
ALTER TABLE number_routing_profiles ADD CONSTRAINT ck_number_routing_profiles_sms CHECK (sms_target_kind IN
    ('NONE', 'PRINCIPAL', 'APPLICATION', 'SHARED_TEAM', 'DEPARTMENT')
    AND (sms_target_kind = 'NONE') = (sms_target_ref IS NULL));

-- ---------------------------------------------------------------------------
-- Organizational number pools
-- ---------------------------------------------------------------------------
CREATE TABLE number_pools (
    id                      UUID         NOT NULL,
    tenant_id               UUID         NOT NULL,
    application_id          UUID         NOT NULL,
    environment_id          UUID         NOT NULL,
    name                    VARCHAR(100) NOT NULL,
    status                  VARCHAR(16)  NOT NULL,
    usage_policy_ref        UUID,
    subscription_policy_ref UUID,
    revision                BIGINT       NOT NULL DEFAULT 1,
    created_by              VARCHAR(200) NOT NULL,
    created_at              TIMESTAMPTZ  NOT NULL,
    updated_at              TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_pools PRIMARY KEY (id),
    CONSTRAINT uq_number_pools_scope UNIQUE (id, tenant_id, application_id, environment_id),
    CONSTRAINT ck_number_pools_status CHECK (status IN ('ACTIVE', 'ARCHIVED')),
    CONSTRAINT ck_number_pools_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_number_pools_revision CHECK (revision >= 1),
    CONSTRAINT ck_number_pools_created_by CHECK (created_by ~ '^[A-Za-z0-9._:@-]{1,200}$')
);
CREATE UNIQUE INDEX uq_number_pools_name ON number_pools (tenant_id, application_id, environment_id, lower(name))
    WHERE status = 'ACTIVE';

CREATE TABLE number_pool_members (
    id                 UUID         NOT NULL,
    pool_id            UUID         NOT NULL,
    assignment_id      UUID         NOT NULL,
    number_resource_id UUID         NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    added_by           VARCHAR(200) NOT NULL,
    added_at           TIMESTAMPTZ  NOT NULL,
    removed_by         VARCHAR(200),
    removed_at         TIMESTAMPTZ,
    CONSTRAINT pk_number_pool_members PRIMARY KEY (id),
    CONSTRAINT fk_number_pool_members_pool FOREIGN KEY (pool_id, tenant_id, application_id, environment_id)
        REFERENCES number_pools (id, tenant_id, application_id, environment_id),
    CONSTRAINT fk_number_pool_members_assignment FOREIGN KEY (assignment_id, tenant_id, application_id, environment_id, number_resource_id)
        REFERENCES dcn_assignments (id, tenant_id, application_id, environment_id, number_resource_id),
    CONSTRAINT ck_number_pool_members_removed CHECK ((removed_at IS NULL) = (removed_by IS NULL)),
    CONSTRAINT ck_number_pool_members_actor CHECK (added_by ~ '^[A-Za-z0-9._:@-]{1,200}$')
);
-- An assignment is in at most one pool at a time.
CREATE UNIQUE INDEX uq_number_pool_members_current ON number_pool_members (assignment_id) WHERE removed_at IS NULL;
CREATE INDEX ix_number_pool_members_pool ON number_pool_members (pool_id) WHERE removed_at IS NULL;

CREATE FUNCTION number_guard_pool_member() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    owner TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'pool membership history is never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        SELECT owner_kind INTO owner FROM dcn_assignments WHERE id = NEW.assignment_id;
        IF owner IS DISTINCT FROM 'ORGANIZATION' THEN
            RAISE EXCEPTION 'only organization-owned numbers join a pool' USING ERRCODE = '23514';
        END IF;
        IF NEW.removed_at IS NOT NULL THEN
            RAISE EXCEPTION 'a membership starts current' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.removed_at IS NOT NULL
       OR (to_jsonb(NEW) - 'removed_at' - 'removed_by') IS DISTINCT FROM (to_jsonb(OLD) - 'removed_at' - 'removed_by') THEN
        RAISE EXCEPTION 'a pool membership is only ever ended' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE FUNCTION number_guard_pool() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'number pools are never deleted (they are archived)' USING ERRCODE = '42501';
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
       OR NEW.application_id IS DISTINCT FROM OLD.application_id OR NEW.environment_id IS DISTINCT FROM OLD.environment_id
       OR NEW.created_by IS DISTINCT FROM OLD.created_by OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'a pool keeps its identity and scope' USING ERRCODE = '42501';
    END IF;
    IF OLD.status = 'ARCHIVED' AND NEW.status <> 'ARCHIVED' THEN
        RAISE EXCEPTION 'an archived pool stays archived' USING ERRCODE = '23514';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'pool updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_pools_guard
    BEFORE UPDATE OR DELETE ON number_pools
    FOR EACH ROW EXECUTE FUNCTION number_guard_pool();
CREATE TRIGGER trg_number_pool_members_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_pool_members
    FOR EACH ROW EXECUTE FUNCTION number_guard_pool_member();
CREATE TRIGGER trg_number_pool_members_no_truncate
    BEFORE TRUNCATE ON number_pool_members
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_pools_no_truncate
    BEFORE TRUNCATE ON number_pools
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM dcn_assignments WHERE successor_assignment_id IS NOT NULL OR predecessor_assignment_id IS NOT NULL)
       OR EXISTS (SELECT 1 FROM number_pools)
       OR EXISTS (SELECT 1 FROM number_routing_profiles WHERE voice_target_kind IN
           ('PROFESSIONAL_IDENTITY', 'SHARED_TEAM', 'VOICEMAIL', 'APPROVED_FORWARDING')
           OR sms_target_kind IN ('SHARED_TEAM', 'DEPARTMENT')) THEN
        RAISE EXCEPTION 'transfers, pools or Workstream J routing targets are recorded; they cannot be represented before Workstream J';
    END IF;
END $$;
DROP TABLE IF EXISTS number_pool_members;
DROP FUNCTION IF EXISTS number_guard_pool_member();
DROP TABLE IF EXISTS number_pools;
DROP FUNCTION IF EXISTS number_guard_pool();
ALTER TABLE number_routing_profiles DROP CONSTRAINT ck_number_routing_profiles_sms;
ALTER TABLE number_routing_profiles ADD CONSTRAINT ck_number_routing_profiles_sms CHECK (sms_target_kind IN ('NONE', 'PRINCIPAL', 'APPLICATION')
    AND (sms_target_kind = 'NONE') = (sms_target_ref IS NULL));
ALTER TABLE number_routing_profiles DROP CONSTRAINT ck_number_routing_profiles_voice;
ALTER TABLE number_routing_profiles ADD CONSTRAINT ck_number_routing_profiles_voice CHECK (voice_target_kind IN
    ('NONE', 'PRINCIPAL', 'CALLING_ENDPOINT', 'ENTERPRISE_EXTENSION', 'ENTERPRISE_RING_GROUP',
     'ENTERPRISE_QUEUE', 'ENTERPRISE_IVR', 'ENTERPRISE_DEPARTMENT')
    AND (voice_target_kind = 'NONE') = (voice_target_ref IS NULL));
ALTER TABLE dcn_assignments
    DROP CONSTRAINT ck_dcn_assignments_transfer,
    DROP CONSTRAINT uq_dcn_assignments_predecessor,
    DROP CONSTRAINT uq_dcn_assignments_successor,
    DROP CONSTRAINT fk_dcn_assignments_successor,
    DROP CONSTRAINT fk_dcn_assignments_predecessor,
    DROP COLUMN successor_assignment_id,
    DROP COLUMN predecessor_assignment_id;
CREATE OR REPLACE FUNCTION dcn_guard_assignment() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    resource_state TEXT;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'assignments are never deleted (reassignment creates a new record)' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM number_guard_environment_inventory(NEW.number_resource_id, NEW.environment_kind);
        IF NEW.status <> 'REQUESTED' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'an assignment starts REQUESTED at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id OR NEW.environment_kind IS DISTINCT FROM OLD.environment_kind
       OR NEW.owner_kind IS DISTINCT FROM OLD.owner_kind OR NEW.owner_ref IS DISTINCT FROM OLD.owner_ref
       OR NEW.ownership_class IS DISTINCT FROM OLD.ownership_class OR NEW.requested_at IS DISTINCT FROM OLD.requested_at
       OR NEW.created_by IS DISTINCT FROM OLD.created_by THEN
        RAISE EXCEPTION 'assignment ownership is immutable; a new owner is a new assignment' USING ERRCODE = '42501';
    END IF;
    IF OLD.reservation_id IS NOT NULL AND NEW.reservation_id IS DISTINCT FROM OLD.reservation_id THEN
        RAISE EXCEPTION 'an assignment keeps its reservation' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'assignment updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status THEN
        IF current_setting('numbering.assignment', true) IS DISTINCT FROM 'transition' THEN
            RAISE EXCEPTION 'assignment status changes only through a domain transition' USING ERRCODE = '42501';
        END IF;
        IF NOT dcn_assignment_transition_allowed(OLD.status, NEW.status) THEN
            RAISE EXCEPTION 'assignment transition % -> % is not allowed', OLD.status, NEW.status USING ERRCODE = '23514';
        END IF;
        IF NEW.status = 'ACTIVE' THEN
            SELECT state INTO resource_state FROM number_resources WHERE id = NEW.number_resource_id;
            IF resource_state <> 'ACTIVE' THEN
                RAISE EXCEPTION 'an assignment becomes ACTIVE only on an ACTIVE number resource' USING ERRCODE = '23514';
            END IF;
        END IF;
    ELSIF OLD.status = 'CLOSED' THEN
        RAISE EXCEPTION 'a CLOSED assignment is history' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE OR REPLACE FUNCTION dcn_assignment_transition_allowed(from_status TEXT, to_status TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_status, to_status) IN (
        ('REQUESTED', 'PENDING_ELIGIBILITY'), ('REQUESTED', 'CLOSED'),
        ('PENDING_ELIGIBILITY', 'ELIGIBLE'), ('PENDING_ELIGIBILITY', 'CLOSED'),
        ('ELIGIBLE', 'RESERVED'), ('ELIGIBLE', 'CLOSED'),
        ('RESERVED', 'ACTIVATING'), ('RESERVED', 'CLOSED'),
        ('ACTIVATING', 'ACTIVE'), ('ACTIVATING', 'RESERVED'), ('ACTIVATING', 'CLOSED'),
        ('ACTIVE', 'SUSPENDED'), ('ACTIVE', 'RELEASE_REQUESTED'),
        ('SUSPENDED', 'ACTIVE'), ('SUSPENDED', 'RELEASE_REQUESTED'),
        ('RELEASE_REQUESTED', 'ROUTING_REMOVED'),
        ('ROUTING_REMOVED', 'QUARANTINED'), ('ROUTING_REMOVED', 'CLOSED'),
        ('QUARANTINED', 'CLOSED')
    )
$$;
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

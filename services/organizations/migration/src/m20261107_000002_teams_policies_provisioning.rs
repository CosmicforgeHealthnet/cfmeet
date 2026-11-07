//! Blueprint v1.0 Sections 6–7: teams, organization policies and the provisioning saga.
//!
//! * `organization_teams` — organization-scoped teams; optional parent in the SAME
//!   organization (composite foreign key; cycles and depth are refused by the API, a
//!   self-parent by CHECK). At most one ACTIVE team per organization and case-folded name.
//! * `organization_team_members` — users (through their organization membership, same
//!   organization by composite foreign key) in teams with TEAM_MANAGER / TEAM_MEMBER; at
//!   most one ACTIVE row per team and membership; removal keeps the row.
//! * `organization_team_member_history` — append-only (trigger).
//! * `organization_policies` — one typed, validated policy document per organization with
//!   a monotonic `revision` (compare-and-swap). An absent row is revision 0 = defaults.
//! * `organization_platform_scopes` gains the tenant-status synchronization columns
//!   (desired / applied status, bounded retry state) and the verification evidence.
//! * `organization_provisioning` gains `capability` and an aggregate `version`.
//! * The outbox accepts the `team_member` aggregate.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE organization_memberships
    ADD CONSTRAINT uq_organization_memberships_owner UNIQUE (organization_id, id);

CREATE TABLE organization_teams (
    id                  UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    name                VARCHAR(120) NOT NULL,
    description         VARCHAR(500),
    parent_team_id      UUID,
    status              VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    created_by          UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    archived_at         TIMESTAMPTZ,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_organization_teams PRIMARY KEY (id),
    CONSTRAINT uq_organization_teams_owner UNIQUE (organization_id, id),
    CONSTRAINT fk_organization_teams_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT fk_organization_teams_parent FOREIGN KEY (organization_id, parent_team_id) REFERENCES organization_teams (organization_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_teams_name CHECK (char_length(btrim(name)) BETWEEN 1 AND 120),
    CONSTRAINT ck_organization_teams_status CHECK (status IN ('ACTIVE', 'ARCHIVED')),
    CONSTRAINT ck_organization_teams_archived CHECK ((status = 'ARCHIVED') = (archived_at IS NOT NULL)),
    CONSTRAINT ck_organization_teams_parent CHECK (parent_team_id IS NULL OR parent_team_id <> id),
    CONSTRAINT ck_organization_teams_version CHECK (version >= 1)
);
CREATE UNIQUE INDEX uq_organization_teams_active_name
    ON organization_teams (organization_id, lower(btrim(name))) WHERE status = 'ACTIVE';
CREATE INDEX ix_organization_teams_parent ON organization_teams (organization_id, parent_team_id) WHERE status = 'ACTIVE';

CREATE TABLE organization_team_members (
    id                  UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    team_id             UUID         NOT NULL,
    membership_id       UUID         NOT NULL,
    user_id             UUID         NOT NULL,
    role                VARCHAR(16)  NOT NULL,
    status              VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    added_by            UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    removed_at          TIMESTAMPTZ,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_organization_team_members PRIMARY KEY (id),
    CONSTRAINT fk_organization_team_members_team FOREIGN KEY (organization_id, team_id) REFERENCES organization_teams (organization_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_organization_team_members_membership FOREIGN KEY (organization_id, membership_id) REFERENCES organization_memberships (organization_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_team_members_role CHECK (role IN ('TEAM_MANAGER', 'TEAM_MEMBER')),
    CONSTRAINT ck_organization_team_members_status CHECK (status IN ('ACTIVE', 'REMOVED')),
    CONSTRAINT ck_organization_team_members_removed CHECK ((status = 'REMOVED') = (removed_at IS NOT NULL)),
    CONSTRAINT ck_organization_team_members_version CHECK (version >= 1)
);
CREATE UNIQUE INDEX uq_organization_team_members_current
    ON organization_team_members (team_id, membership_id) WHERE status = 'ACTIVE';
CREATE INDEX ix_organization_team_members_user ON organization_team_members (organization_id, user_id) WHERE status = 'ACTIVE';
CREATE INDEX ix_organization_team_members_membership ON organization_team_members (membership_id) WHERE status = 'ACTIVE';

CREATE TABLE organization_team_member_history (
    seq                 BIGSERIAL    NOT NULL,
    id                  UUID         NOT NULL,
    team_member_id      UUID         NOT NULL,
    team_id             UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    user_id             UUID         NOT NULL,
    change_kind         VARCHAR(32)  NOT NULL,
    from_role           VARCHAR(16),
    to_role             VARCHAR(16)  NOT NULL,
    from_status         VARCHAR(16),
    to_status           VARCHAR(16)  NOT NULL,
    team_member_version BIGINT       NOT NULL,
    actor_kind          VARCHAR(16)  NOT NULL,
    actor_user_id       UUID,
    actor_service       VARCHAR(64)  NOT NULL,
    request_id          VARCHAR(64),
    occurred_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_team_member_history PRIMARY KEY (seq),
    CONSTRAINT uq_organization_team_member_history_id UNIQUE (id),
    CONSTRAINT ck_organization_team_member_history_kind CHECK (change_kind IN ('ADDED', 'ROLE_CHANGED', 'REMOVED', 'MEMBERSHIP_ENDED', 'TEAM_ARCHIVED')),
    CONSTRAINT ck_organization_team_member_history_actor CHECK (actor_kind IN ('USER', 'OPERATOR', 'SYSTEM'))
);
CREATE INDEX ix_organization_team_member_history_member ON organization_team_member_history (team_member_id, seq);
CREATE TRIGGER tr_organization_team_member_history_append_only BEFORE UPDATE OR DELETE ON organization_team_member_history
    FOR EACH ROW EXECUTE FUNCTION organization_append_only();
CREATE TRIGGER tr_organization_team_member_history_no_truncate BEFORE TRUNCATE ON organization_team_member_history
    FOR EACH STATEMENT EXECUTE FUNCTION organization_append_only();

CREATE TABLE organization_policies (
    organization_id     UUID         NOT NULL,
    revision            BIGINT       NOT NULL,
    document            JSONB        NOT NULL,
    updated_by          UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_policies PRIMARY KEY (organization_id),
    CONSTRAINT fk_organization_policies_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_policies_revision CHECK (revision >= 1),
    CONSTRAINT ck_organization_policies_document CHECK (jsonb_typeof(document) = 'object' AND octet_length(document::text) <= 8192)
);

ALTER TABLE organization_platform_scopes
    ADD COLUMN tenant_status_desired VARCHAR(16),
    ADD COLUMN tenant_status_applied VARCHAR(16),
    ADD COLUMN sync_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN next_sync_at TIMESTAMPTZ,
    ADD COLUMN last_sync_error_class VARCHAR(64),
    ADD COLUMN verification_evidence JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD CONSTRAINT ck_organization_platform_scopes_desired CHECK (tenant_status_desired IS NULL OR tenant_status_desired IN ('ACTIVE', 'SUSPENDED')),
    ADD CONSTRAINT ck_organization_platform_scopes_applied CHECK (tenant_status_applied IS NULL OR tenant_status_applied IN ('ACTIVE', 'SUSPENDED')),
    ADD CONSTRAINT ck_organization_platform_scopes_sync_attempts CHECK (sync_attempts >= 0),
    ADD CONSTRAINT ck_organization_platform_scopes_sync_error CHECK (last_sync_error_class IS NULL OR last_sync_error_class ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    ADD CONSTRAINT ck_organization_platform_scopes_evidence CHECK (jsonb_typeof(verification_evidence) = 'object' AND octet_length(verification_evidence::text) <= 2048),
    ADD CONSTRAINT ck_organization_platform_scopes_capability CHECK (capability ~ '^[a-z][a-z0-9-]{0,31}$'),
    -- A recorded triple is all three ids or none.
    ADD CONSTRAINT ck_organization_platform_scopes_triple CHECK (
        (tenant_id IS NULL) = (application_id IS NULL) AND (tenant_id IS NULL) = (environment_id IS NULL));
-- One technical tenant never backs two organizations (no split-brain mapping).
CREATE UNIQUE INDEX uq_organization_platform_scopes_tenant
    ON organization_platform_scopes (tenant_id) WHERE tenant_id IS NOT NULL AND mapping_state <> 'RETIRED';
CREATE INDEX ix_organization_platform_scopes_sync ON organization_platform_scopes (next_sync_at)
    WHERE tenant_status_desired IS NOT NULL AND tenant_status_desired IS DISTINCT FROM tenant_status_applied;

ALTER TABLE organization_provisioning
    ADD COLUMN capability VARCHAR(64) NOT NULL DEFAULT 'primary',
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_organization_provisioning_version CHECK (version >= 1),
    ADD CONSTRAINT ck_organization_provisioning_lease CHECK ((lease_id IS NULL) = (lease_until IS NULL));
CREATE INDEX ix_organization_provisioning_due ON organization_provisioning (next_attempt_at)
    WHERE state IN ('PENDING', 'IN_PROGRESS');

ALTER TABLE organization_outbox DROP CONSTRAINT ck_organization_outbox_aggregate;
ALTER TABLE organization_outbox ADD CONSTRAINT ck_organization_outbox_aggregate
    CHECK (aggregate_type IN ('organization', 'membership', 'invitation', 'team', 'team_member', 'policy', 'scope', 'provisioning'));
"#;

const DOWN: &str = r#"
ALTER TABLE organization_outbox DROP CONSTRAINT ck_organization_outbox_aggregate;
ALTER TABLE organization_outbox ADD CONSTRAINT ck_organization_outbox_aggregate
    CHECK (aggregate_type IN ('organization', 'membership', 'invitation', 'team', 'policy', 'scope', 'provisioning'));
DROP INDEX IF EXISTS ix_organization_provisioning_due;
ALTER TABLE organization_provisioning
    DROP CONSTRAINT IF EXISTS ck_organization_provisioning_lease,
    DROP CONSTRAINT IF EXISTS ck_organization_provisioning_version,
    DROP COLUMN IF EXISTS version,
    DROP COLUMN IF EXISTS capability;
DROP INDEX IF EXISTS ix_organization_platform_scopes_sync;
DROP INDEX IF EXISTS uq_organization_platform_scopes_tenant;
ALTER TABLE organization_platform_scopes
    DROP CONSTRAINT IF EXISTS ck_organization_platform_scopes_triple,
    DROP CONSTRAINT IF EXISTS ck_organization_platform_scopes_capability,
    DROP CONSTRAINT IF EXISTS ck_organization_platform_scopes_evidence,
    DROP CONSTRAINT IF EXISTS ck_organization_platform_scopes_sync_error,
    DROP CONSTRAINT IF EXISTS ck_organization_platform_scopes_sync_attempts,
    DROP CONSTRAINT IF EXISTS ck_organization_platform_scopes_applied,
    DROP CONSTRAINT IF EXISTS ck_organization_platform_scopes_desired,
    DROP COLUMN IF EXISTS verification_evidence,
    DROP COLUMN IF EXISTS last_sync_error_class,
    DROP COLUMN IF EXISTS next_sync_at,
    DROP COLUMN IF EXISTS sync_attempts,
    DROP COLUMN IF EXISTS tenant_status_applied,
    DROP COLUMN IF EXISTS tenant_status_desired;
DROP TABLE IF EXISTS organization_policies;
DROP TABLE IF EXISTS organization_team_member_history;
DROP TABLE IF EXISTS organization_team_members;
DROP TABLE IF EXISTS organization_teams;
ALTER TABLE organization_memberships DROP CONSTRAINT IF EXISTS uq_organization_memberships_owner;
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

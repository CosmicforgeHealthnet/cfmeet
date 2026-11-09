//! Organization integration (final closure: Organizations event delivery and
//! organization-scope offboarding). Additive, own history
//! (`organization_integration_migrations`, binary `organization-integration-migration`).
//!
//! * `organization_event_inbox` — every delivered Organizations event once (dedupe on
//!   `event_id`; a different body under a known id is refused) and its outcome.
//! * `organization_event_fences` — the last applied `aggregateVersion` per aggregate:
//!   a version at or below it is recorded as STALE and not applied (monotonic).
//! * `organization_member_access` — the desired organization access of each member
//!   (ACTIVE / SUSPENDED / REMOVED, fenced by the membership version) and the state the
//!   offboarding worker last applied to the organization-scope identities (Messaging and
//!   Calling principals in the organization's own technical scope). Personal-scope
//!   identities are never touched.
//! * `organization_scope_environments` — the organization technical scope (tenant,
//!   application, environment and the bound service account) of every organization
//!   environment the Meeting Service created member principals in, so offboarding can
//!   reach those principals after the membership itself is gone.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE organization_event_inbox (
    event_id            UUID         NOT NULL,
    event_type          VARCHAR(64)  NOT NULL,
    organization_id     UUID         NOT NULL,
    aggregate_type      VARCHAR(32)  NOT NULL,
    aggregate_id        UUID         NOT NULL,
    aggregate_version   BIGINT       NOT NULL,
    request_digest      CHAR(64)     NOT NULL,
    outcome             VARCHAR(16)  NOT NULL,
    received_at         TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_organization_event_inbox PRIMARY KEY (event_id),
    CONSTRAINT ck_organization_event_inbox_outcome CHECK (outcome IN ('APPLIED', 'STALE', 'RECORDED')),
    CONSTRAINT ck_organization_event_inbox_version CHECK (aggregate_version >= 1),
    CONSTRAINT ck_organization_event_inbox_type CHECK (event_type ~ '^[a-z][a-z_]*\.[a-z][a-z_]*$')
);
CREATE INDEX ix_organization_event_inbox_org ON organization_event_inbox (organization_id, received_at);

CREATE TABLE organization_event_fences (
    aggregate_id        UUID         NOT NULL,
    aggregate_type      VARCHAR(32)  NOT NULL,
    aggregate_version   BIGINT       NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_organization_event_fences PRIMARY KEY (aggregate_id),
    CONSTRAINT ck_organization_event_fences_version CHECK (aggregate_version >= 1)
);

CREATE TABLE organization_member_access (
    organization_id     UUID         NOT NULL,
    user_id             UUID         NOT NULL,
    desired_state       VARCHAR(16)  NOT NULL,
    membership_version  BIGINT       NOT NULL,
    applied_state       VARCHAR(16),
    applied_version     BIGINT,
    attempts            INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    last_error_class    VARCHAR(64),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    applied_at          TIMESTAMPTZ,
    CONSTRAINT pk_organization_member_access PRIMARY KEY (organization_id, user_id),
    CONSTRAINT ck_organization_member_access_desired CHECK (desired_state IN ('ACTIVE', 'SUSPENDED', 'REMOVED')),
    CONSTRAINT ck_organization_member_access_applied CHECK (applied_state IS NULL OR applied_state IN ('ACTIVE', 'SUSPENDED', 'REMOVED')),
    CONSTRAINT ck_organization_member_access_attempts CHECK (attempts >= 0)
);
CREATE INDEX ix_organization_member_access_due ON organization_member_access (next_attempt_at)
    WHERE applied_state IS DISTINCT FROM desired_state OR applied_version IS DISTINCT FROM membership_version;

CREATE TABLE organization_scope_environments (
    environment_id      UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    service_account_id  UUID         NOT NULL,
    recorded_at         TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_organization_scope_environments PRIMARY KEY (environment_id)
);
CREATE INDEX ix_organization_scope_environments_org ON organization_scope_environments (organization_id);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS organization_scope_environments;
DROP TABLE IF EXISTS organization_member_access;
DROP TABLE IF EXISTS organization_event_fences;
DROP TABLE IF EXISTS organization_event_inbox;
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

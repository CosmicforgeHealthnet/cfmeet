//! Phase 2 A–D closure — authoritative CF organization directory.
//!
//! No organization/membership source exists elsewhere in the monorepo; the meeting
//! service is the current CF account authority (it owns `users` and authentication),
//! so organizations and their memberships live here behind the `OrganizationDirectory`
//! abstraction. ORGANIZATION_ONLY access resolves membership from these tables only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE organizations (
    id          UUID PRIMARY KEY,
    name        VARCHAR(200) NOT NULL,
    slug        VARCHAR(100) NOT NULL UNIQUE,
    status      TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','SUSPENDED')),
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (slug ~ '^[a-z0-9][a-z0-9-]*$')
);

CREATE TABLE organization_memberships (
    id               UUID PRIMARY KEY,
    organization_id  UUID NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role             TEXT NOT NULL CHECK (role IN ('OWNER','ADMIN','MEMBER')),
    status           TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','SUSPENDED','REMOVED')),
    created_by       UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    removed_at       TIMESTAMPTZ,
    CHECK (status <> 'REMOVED' OR removed_at IS NOT NULL)
);
-- One non-removed membership per organization + user.
CREATE UNIQUE INDEX uq_org_membership_current
    ON organization_memberships (organization_id, user_id) WHERE status <> 'REMOVED';
CREATE INDEX idx_org_membership_user_active
    ON organization_memberships (user_id) WHERE status = 'ACTIVE';

-- Meeting spaces may only reference real organizations.
ALTER TABLE meeting_spaces
    ADD CONSTRAINT fk_meeting_spaces_organization
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE RESTRICT;
"#;

const DOWN: &str = r#"
ALTER TABLE meeting_spaces DROP CONSTRAINT IF EXISTS fk_meeting_spaces_organization;
DROP TABLE IF EXISTS organization_memberships;
DROP TABLE IF EXISTS organizations;
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

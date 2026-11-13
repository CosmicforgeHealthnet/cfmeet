//! ADR 0058 Amendment A2 — automatic organization service bindings.
//!
//! The Organizations provisioning saga now creates the organization's Calling and
//! Messaging service bindings through Platform Access
//! (`platform-identity.bind-organization-services`) instead of an operator. Additive:
//!
//! * `organization_scope_service_bindings.source` — `OPERATOR` (recorded through the
//!   operator route; every existing row) or `AUTOMATIC` (recorded by the saga). Automation
//!   never replaces an `OPERATOR` binding.
//! * `verified_at`, `credential_expires_at` — the last independent read-back of an
//!   automatic binding and the credential's expiry it reported.
//! * `revoked_at` … `revocation_last_error_class` — the durable desired state "revoke at
//!   Platform Access": set in the archive transaction (the binding stops being resolvable
//!   at once) and applied by the worker with retries until Platform Access confirms.
//! * `organization_platform_scopes.bindings_*` — schedule and outcome of the periodic
//!   binding check (backfill, credential renewal, drift detection).
//!
//! Ids and timestamps only: no credential value is ever stored.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE organization_scope_service_bindings
    ADD COLUMN source VARCHAR(16) NOT NULL DEFAULT 'OPERATOR',
    ADD COLUMN verified_at TIMESTAMPTZ,
    ADD COLUMN credential_expires_at TIMESTAMPTZ,
    ADD COLUMN revoked_at TIMESTAMPTZ,
    ADD COLUMN revocation_applied_at TIMESTAMPTZ,
    ADD COLUMN revocation_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN revocation_next_at TIMESTAMPTZ,
    ADD COLUMN revocation_last_error_class VARCHAR(64),
    ADD CONSTRAINT ck_organization_scope_service_bindings_source CHECK (source IN ('OPERATOR', 'AUTOMATIC')),
    ADD CONSTRAINT ck_organization_scope_service_bindings_revocation CHECK (revocation_applied_at IS NULL OR revoked_at IS NOT NULL);
CREATE INDEX ix_organization_scope_service_bindings_revocation_due
    ON organization_scope_service_bindings (revocation_next_at)
    WHERE revoked_at IS NOT NULL AND revocation_applied_at IS NULL;
ALTER TABLE organization_platform_scopes
    ADD COLUMN bindings_next_check_at TIMESTAMPTZ,
    ADD COLUMN bindings_check_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN bindings_last_error_class VARCHAR(64);
"#;

const DOWN: &str = r#"
ALTER TABLE organization_platform_scopes
    DROP COLUMN IF EXISTS bindings_last_error_class,
    DROP COLUMN IF EXISTS bindings_check_attempts,
    DROP COLUMN IF EXISTS bindings_next_check_at;
DROP INDEX IF EXISTS ix_organization_scope_service_bindings_revocation_due;
DELETE FROM organization_scope_service_bindings WHERE revoked_at IS NOT NULL;
ALTER TABLE organization_scope_service_bindings
    DROP CONSTRAINT IF EXISTS ck_organization_scope_service_bindings_revocation,
    DROP CONSTRAINT IF EXISTS ck_organization_scope_service_bindings_source,
    DROP COLUMN IF EXISTS revocation_last_error_class,
    DROP COLUMN IF EXISTS revocation_next_at,
    DROP COLUMN IF EXISTS revocation_attempts,
    DROP COLUMN IF EXISTS revocation_applied_at,
    DROP COLUMN IF EXISTS revoked_at,
    DROP COLUMN IF EXISTS credential_expires_at,
    DROP COLUMN IF EXISTS verified_at,
    DROP COLUMN IF EXISTS source;
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

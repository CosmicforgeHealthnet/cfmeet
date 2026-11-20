//! ADR 0068, phase A4 — partner and app commands relayed by the HQ admin bridge.
//! Additive only.
//!
//! * `devplat_admin_operations` records the A4 subjects too (a workspace, its support
//!   profile, a push credential, a webhook delivery retry, a usage reprocess): the subject
//!   check is widened, nothing else changes.
//! * `devplat_push_providers` gains a row `version` (the bridge's expected-version lock),
//!   a `state` (`ACTIVE` | `DISABLED`) and the disable time and reason code. A credential
//!   HQ disables is deleted from the Notification Service vault and kept here as
//!   metadata (never key material); the partner re-enables push by uploading again (a new
//!   row). The previous image keeps working: it never reads the new columns and its
//!   inserts take their defaults.
//! * `devplat_operator_audit` (platform-level operator commands) also accepts the admin
//!   bridge's `hq:<staff>` operator (a usage reprocess, the pricing commands).
//!
//! Rollback: DOWN drops the push columns and restores the original subject check
//! (`NOT VALID`, so A4 records already written do not block it).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE devplat_admin_operations DROP CONSTRAINT ck_devplat_admin_operations_subject;
ALTER TABLE devplat_admin_operations ADD CONSTRAINT ck_devplat_admin_operations_subject CHECK (subject_kind IN
    ('PRODUCTION_ACCESS', 'APP', 'ORGANIZATION', 'SUPPORT_PROFILE', 'PUSH_KEY', 'WEBHOOK_DELIVERY', 'USAGE_ROLLUPS'));

ALTER TABLE devplat_push_providers ADD COLUMN version BIGINT NOT NULL DEFAULT 1;
ALTER TABLE devplat_push_providers ADD COLUMN state VARCHAR(16) NOT NULL DEFAULT 'ACTIVE';
ALTER TABLE devplat_push_providers ADD COLUMN disabled_at TIMESTAMPTZ;
ALTER TABLE devplat_push_providers ADD COLUMN disabled_reason VARCHAR(64);
ALTER TABLE devplat_push_providers ADD CONSTRAINT ck_devplat_push_providers_state CHECK (state IN ('ACTIVE', 'DISABLED'));
ALTER TABLE devplat_push_providers ADD CONSTRAINT ck_devplat_push_providers_disabled CHECK (
    (state = 'DISABLED') = (disabled_at IS NOT NULL)
    AND (disabled_reason IS NULL OR disabled_reason ~ '^[A-Z][A-Z0-9_]{1,63}$'));
ALTER TABLE devplat_push_providers ADD CONSTRAINT ck_devplat_push_providers_version CHECK (version >= 1);

ALTER TABLE devplat_operator_audit ALTER COLUMN operator TYPE VARCHAR(160);
ALTER TABLE devplat_operator_audit DROP CONSTRAINT ck_devplat_operator_audit_operator;
ALTER TABLE devplat_operator_audit ADD CONSTRAINT ck_devplat_operator_audit_operator CHECK (
    operator ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$' OR operator ~ '^hq:[A-Za-z0-9._:@-]{1,128}$');
"#;

const DOWN: &str = r#"
ALTER TABLE devplat_operator_audit DROP CONSTRAINT ck_devplat_operator_audit_operator;
ALTER TABLE devplat_operator_audit ADD CONSTRAINT ck_devplat_operator_audit_operator
    CHECK (operator ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$') NOT VALID;
ALTER TABLE devplat_push_providers DROP CONSTRAINT IF EXISTS ck_devplat_push_providers_version;
ALTER TABLE devplat_push_providers DROP CONSTRAINT IF EXISTS ck_devplat_push_providers_disabled;
ALTER TABLE devplat_push_providers DROP CONSTRAINT IF EXISTS ck_devplat_push_providers_state;
ALTER TABLE devplat_push_providers DROP COLUMN IF EXISTS disabled_reason;
ALTER TABLE devplat_push_providers DROP COLUMN IF EXISTS disabled_at;
ALTER TABLE devplat_push_providers DROP COLUMN IF EXISTS state;
ALTER TABLE devplat_push_providers DROP COLUMN IF EXISTS version;
ALTER TABLE devplat_admin_operations DROP CONSTRAINT ck_devplat_admin_operations_subject;
ALTER TABLE devplat_admin_operations ADD CONSTRAINT ck_devplat_admin_operations_subject
    CHECK (subject_kind IN ('PRODUCTION_ACCESS', 'APP')) NOT VALID;
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

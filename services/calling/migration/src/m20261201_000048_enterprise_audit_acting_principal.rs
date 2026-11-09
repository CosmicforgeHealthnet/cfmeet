//! Blueprint Section 12 follow-up (CAL-007 / ORG-012) — the human actor of enterprise
//! administrative changes.
//!
//! A first-party organization request forwarded by the Meeting Service
//! (`calling-request.act-for-organization`) carries the acting member's principal, which
//! Calling resolves inside the organization's own tenant / application / environment
//! before it is recorded. `actor_principal_id` holds it on the append-only
//! `enterprise_admin_audit` row; it stays `NULL` for direct and gateway-forwarded
//! customer credentials, the operator CLI and Calling workers. Additive only: the column
//! is nullable, existing rows are untouched (the append-only trigger fires on UPDATE /
//! DELETE / TRUNCATE, never on `ALTER TABLE`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE enterprise_admin_audit
    ADD COLUMN actor_principal_id UUID;
"#;

const DOWN: &str = r#"
ALTER TABLE enterprise_admin_audit
    DROP COLUMN IF EXISTS actor_principal_id;
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

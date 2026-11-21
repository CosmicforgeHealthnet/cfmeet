//! ADR 0068, phase A4 — `accounts.user.status` may suspend an account (`SUSPENDED`).
//! Additive only.
//!
//! The `user_status` enum gained `suspended` in the Account Communications history
//! (`account_communications_migrations`), which a deployment applies only while the
//! Communications integration is configured. The admin bridge is independent of that
//! integration, so its own history adds the value too (`IF NOT EXISTS`: whichever history
//! runs first adds it, the other one is a no-op). Without it a bridge suspension failed
//! with `invalid input value for enum user_status` (a 503) on a deployment without the
//! Communications integration.
//!
//! In the admin-operations history (`meeting_admin_operations_migrations`), required at
//! startup only while `ADMIN_OPERATOR_API_ENABLED=true`.
//!
//! Rollback: DOWN is a no-op. PostgreSQL cannot drop an enum value, the previous image
//! already knows `suspended` (`UserStatus::Suspended`), and the Account Communications
//! history may own the same value.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = "ALTER TYPE user_status ADD VALUE IF NOT EXISTS 'suspended';";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

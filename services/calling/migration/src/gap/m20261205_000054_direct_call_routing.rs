//! Forwarding and voicemail of direct person-to-person calls (gap closure, additive).
//!
//! A direct one-to-one application call whose callee has a forwarding rule or a mailbox
//! is routed by the enterprise router from the callee as its entry: its
//! `enterprise_call_contexts` row has no entry extension (`entry_extension_id` NULL,
//! entry number / label `direct`). Existing rows (extension calls) are unchanged, and a
//! row without an extension is only ever a direct one.
//!
//! Rollback note: the previous image reads `entry_extension_id` as non-null; contexts of
//! direct calls created by this image are unreadable to it (`ENTERPRISE_CALLING_ENABLED`
//! off stops creating them). DOWN refuses while such rows exist rather than deleting
//! routing evidence.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE enterprise_call_contexts
    ALTER COLUMN entry_extension_id DROP NOT NULL,
    ADD CONSTRAINT ck_enterprise_call_contexts_direct CHECK (
        entry_extension_id IS NOT NULL
        OR (entry_extension_number = 'direct' AND entry_label = 'direct'));
"#;

const DOWN: &str = r#"
ALTER TABLE enterprise_call_contexts
    DROP CONSTRAINT IF EXISTS ck_enterprise_call_contexts_direct,
    ALTER COLUMN entry_extension_id SET NOT NULL;
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

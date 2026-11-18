//! Messaging gap closure G6 / G7 (additive; gap-closure history).
//!
//! * G6 `messaging_moderation_actions.moderator_principal_id`: the organization moderator
//!   (their principal in the organization's own scope) an action relayed by the Meeting
//!   Service was taken for. NULL for actions of an application moderator credential, and
//!   for every action recorded before this migration.
//! * G7 `authority_source = 'ORGANIZATION_TEAM'`: a member of a team channel whose
//!   membership follows the Organizations team snapshot (removed when a later snapshot no
//!   longer lists them). Members added any other way are left alone by the sync.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_moderation_actions
    ADD COLUMN IF NOT EXISTS moderator_principal_id UUID;

ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_authority,
    ADD CONSTRAINT ck_messaging_conversation_members_authority
        CHECK (authority_source IN ('API', 'INVITATION', 'MEETING', 'INVITE_LINK', 'ORGANIZATION_TEAM'));
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_authority,
    ADD CONSTRAINT ck_messaging_conversation_members_authority
        CHECK (authority_source IN ('API', 'INVITATION', 'MEETING', 'INVITE_LINK')) NOT VALID;
ALTER TABLE messaging_moderation_actions DROP COLUMN IF EXISTS moderator_principal_id;
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

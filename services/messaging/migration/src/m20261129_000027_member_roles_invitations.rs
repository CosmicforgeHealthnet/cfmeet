//! Part V §30 / §35 / §36 / §113 (G-01): the full member role set, invitations and the
//! source of a membership's authority.
//!
//! * Roles: OWNER, ADMIN, MODERATOR (moderates members without ownership or role
//!   administration), MEMBER, READ_ONLY (reads its history window, sends no messages),
//!   BOT_SERVICE (only BOT_OR_SERVICE principals).
//! * State INVITED: not a member yet (no history, no events, no sends) until the invitee
//!   accepts; an invitation expires.
//! * `authority_source`: API (an administrator or the application backend), INVITATION
//!   (an accepted invitation), MEETING (the meeting integration).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_role,
    DROP CONSTRAINT ck_messaging_conversation_members_state,
    DROP CONSTRAINT ck_messaging_conversation_members_removed;
ALTER TABLE messaging_conversation_members
    ADD CONSTRAINT ck_messaging_conversation_members_role CHECK (role IN ('OWNER', 'ADMIN', 'MODERATOR', 'MEMBER', 'READ_ONLY', 'BOT_SERVICE')),
    ADD CONSTRAINT ck_messaging_conversation_members_state CHECK (state IN ('ACTIVE', 'INVITED', 'REMOVED', 'LEFT')),
    ADD CONSTRAINT ck_messaging_conversation_members_removed CHECK ((state IN ('ACTIVE', 'INVITED')) = (removed_at IS NULL)),
    ADD COLUMN authority_source VARCHAR(24) NOT NULL DEFAULT 'API',
    ADD COLUMN invited_by_principal_id UUID,
    ADD COLUMN invited_at TIMESTAMPTZ,
    ADD COLUMN invitation_expires_at TIMESTAMPTZ,
    ADD CONSTRAINT ck_messaging_conversation_members_authority CHECK (authority_source IN ('API', 'INVITATION', 'MEETING')),
    ADD CONSTRAINT ck_messaging_conversation_members_invitation CHECK (state <> 'INVITED'
        OR (invited_at IS NOT NULL AND invitation_expires_at IS NOT NULL AND invitation_expires_at > invited_at)),
    ADD CONSTRAINT ck_messaging_conversation_members_owner_active CHECK (role <> 'OWNER' OR state <> 'INVITED');
CREATE INDEX ix_messaging_conversation_members_invited ON messaging_conversation_members (environment_id, principal_id)
    WHERE state = 'INVITED';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS ix_messaging_conversation_members_invited;
UPDATE messaging_conversation_members SET state = 'REMOVED', removed_at = COALESCE(invited_at, updated_at), removed_reason = 'INVITATION_REVOKED'
    WHERE state = 'INVITED';
UPDATE messaging_conversation_members SET role = 'MEMBER' WHERE role IN ('MODERATOR', 'READ_ONLY', 'BOT_SERVICE');
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_owner_active,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_invitation,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversation_members_authority,
    DROP COLUMN IF EXISTS invitation_expires_at,
    DROP COLUMN IF EXISTS invited_at,
    DROP COLUMN IF EXISTS invited_by_principal_id,
    DROP COLUMN IF EXISTS authority_source,
    DROP CONSTRAINT ck_messaging_conversation_members_removed,
    DROP CONSTRAINT ck_messaging_conversation_members_state,
    DROP CONSTRAINT ck_messaging_conversation_members_role;
ALTER TABLE messaging_conversation_members
    ADD CONSTRAINT ck_messaging_conversation_members_role CHECK (role IN ('OWNER', 'ADMIN', 'MEMBER')),
    ADD CONSTRAINT ck_messaging_conversation_members_state CHECK (state IN ('ACTIVE', 'REMOVED', 'LEFT')),
    ADD CONSTRAINT ck_messaging_conversation_members_removed CHECK ((state = 'ACTIVE') = (removed_at IS NULL));
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

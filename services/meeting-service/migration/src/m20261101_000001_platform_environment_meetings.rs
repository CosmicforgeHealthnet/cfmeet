//! Part VI §51 / §53 — meetings owned by a Platform Access application environment.
//!
//! A meeting space is owned by EXACTLY ONE of:
//!
//! * a CF Meet account (`owner_user_id`, unchanged for every existing row), or
//! * a Platform Access application environment (`platform_tenant_id`,
//!   `platform_application_id`, `platform_environment_id`), created by a machine caller
//!   (`created_by_service_account_id`) whose platform credential the API Gateway
//!   forwarded and Platform Access re-verified.
//!
//! The identifiers are Platform Access's canonical ids (references, never a copy of the
//! identity hierarchy). `ck_meeting_spaces_platform_scope` keeps the four platform
//! columns all-or-nothing and `ck_meeting_spaces_single_owner` makes the two ownership
//! kinds mutually exclusive, so a user-owned meeting can never be scoped to an
//! environment and an environment's meeting never has a human owner.
//!
//! * `meeting_invitations.inviter_user_id` becomes nullable: an environment's machine
//!   caller (or a webinar registration on an environment-owned space) has no CF Meet
//!   account. The audit trail records the acting service account.
//! * `outbox_events.channel` gains `PUBLIC_EVENT`: the platform envelopes of
//!   environment-scoped meeting events, written in the same transaction as the fact and
//!   published to the Developer Platform (never delivered to realtime clients).
//!
//! Down refuses while environment-owned meetings exist (no data is silently dropped);
//! pending public-event rows are delivery-only and are removed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE meeting_spaces ALTER COLUMN owner_user_id DROP NOT NULL;
ALTER TABLE meeting_spaces
    ADD COLUMN platform_tenant_id UUID,
    ADD COLUMN platform_application_id UUID,
    ADD COLUMN platform_environment_id UUID,
    ADD COLUMN created_by_service_account_id UUID;
ALTER TABLE meeting_spaces ADD CONSTRAINT ck_meeting_spaces_platform_scope CHECK (
    (platform_tenant_id IS NULL AND platform_application_id IS NULL
        AND platform_environment_id IS NULL AND created_by_service_account_id IS NULL)
    OR (platform_tenant_id IS NOT NULL AND platform_application_id IS NOT NULL
        AND platform_environment_id IS NOT NULL AND created_by_service_account_id IS NOT NULL));
ALTER TABLE meeting_spaces ADD CONSTRAINT ck_meeting_spaces_single_owner CHECK (
    (owner_user_id IS NULL) = (platform_environment_id IS NOT NULL));
CREATE INDEX idx_meeting_spaces_platform_environment
    ON meeting_spaces (platform_environment_id, created_at DESC)
    WHERE platform_environment_id IS NOT NULL;

ALTER TABLE meeting_invitations ALTER COLUMN inviter_user_id DROP NOT NULL;

ALTER TABLE outbox_events DROP CONSTRAINT IF EXISTS outbox_events_channel_check;
ALTER TABLE outbox_events ADD CONSTRAINT ck_outbox_events_channel
    CHECK (channel IN ('REALTIME','PROVIDER_COMMAND','PUBLIC_EVENT'));
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM meeting_spaces WHERE platform_environment_id IS NOT NULL) THEN
        RAISE EXCEPTION 'environment-owned meetings exist; refusing to drop their ownership';
    END IF;
    IF EXISTS (SELECT 1 FROM meeting_invitations WHERE inviter_user_id IS NULL) THEN
        RAISE EXCEPTION 'invitations without a CF Meet inviter exist; refusing to downgrade';
    END IF;
END $$;
DELETE FROM outbox_events WHERE channel = 'PUBLIC_EVENT';
ALTER TABLE outbox_events DROP CONSTRAINT IF EXISTS ck_outbox_events_channel;
ALTER TABLE outbox_events ADD CONSTRAINT outbox_events_channel_check
    CHECK (channel IN ('REALTIME','PROVIDER_COMMAND'));

ALTER TABLE meeting_invitations ALTER COLUMN inviter_user_id SET NOT NULL;

DROP INDEX IF EXISTS idx_meeting_spaces_platform_environment;
ALTER TABLE meeting_spaces DROP CONSTRAINT IF EXISTS ck_meeting_spaces_single_owner;
ALTER TABLE meeting_spaces DROP CONSTRAINT IF EXISTS ck_meeting_spaces_platform_scope;
ALTER TABLE meeting_spaces
    DROP COLUMN IF EXISTS created_by_service_account_id,
    DROP COLUMN IF EXISTS platform_environment_id,
    DROP COLUMN IF EXISTS platform_application_id,
    DROP COLUMN IF EXISTS platform_tenant_id;
ALTER TABLE meeting_spaces ALTER COLUMN owner_user_id SET NOT NULL;
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

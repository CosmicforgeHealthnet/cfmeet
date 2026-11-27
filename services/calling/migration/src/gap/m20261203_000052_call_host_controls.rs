//! C4 — the host of a group call (additive, backward compatible).
//!
//! * `call_participants.is_host` (default false): the participant the host role was
//!   transferred to; at most one per Call (partial unique index). Without a flagged host
//!   the Call's INITIATOR is its host, so every existing Call keeps the creator as host and
//!   no backfill is needed.
//! * `call_participant_media_permissions.host_muted_at`: set while the host muted this
//!   participant; the effective permissions then grant no audio publishing (enforced at
//!   the media provider) until the host unmutes.
//!
//! Removal by the host reuses the existing participant retirement (`ended_at`, the
//! CALL_PARTICIPANT_REMOVED event with removal reason HOST_REMOVED): no column is needed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE call_participants ADD COLUMN is_host BOOLEAN NOT NULL DEFAULT false;
CREATE UNIQUE INDEX uq_call_participants_one_host ON call_participants (call_id) WHERE is_host;
ALTER TABLE call_participant_media_permissions ADD COLUMN host_muted_at TIMESTAMPTZ;
"#;

const DOWN: &str = r#"
ALTER TABLE call_participant_media_permissions DROP COLUMN IF EXISTS host_muted_at;
DROP INDEX IF EXISTS uq_call_participants_one_host;
ALTER TABLE call_participants DROP COLUMN IF EXISTS is_host;
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

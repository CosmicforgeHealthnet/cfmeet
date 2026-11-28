//! ADR 0073 (event contract 1.10.0, common contract 1.9.0): Communities in Messaging. The
//! community notices (`messaging.community.invited`, `messaging.community.join_requested`
//! and `messaging.community.request_approved`, catalogue events #48–#50) are delivered
//! under the routing category `MESSAGING_COMMUNITIES`, in-app and push, so two
//! `notif_routing` rows, seeded `OFF` like every other (category, channel) at the start of
//! its cutover; `catalogue::routing_rows` and its seed test include this file. Their inbox
//! items open the community, so the closed inbox action set gains `OPEN_COMMUNITY`.
//!
//! It runs after ADR 0071's `OPEN_EVENT` (m20261218) and ADR 0069's `OPEN_STATUS`
//! (m20261220), so the action constraint it writes keeps both.
//!
//! `down` refuses while a row is not `OFF` or an inbox item still opens a community, then
//! removes the rows and drops `OPEN_COMMUNITY` from the action set.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
INSERT INTO notif_routing (category, channel, mode, changed_by_run) VALUES
    ('MESSAGING_COMMUNITIES', 'IN_APP', 'OFF', 'migration'),
    ('MESSAGING_COMMUNITIES', 'PUSH', 'OFF', 'migration')
ON CONFLICT (category, channel) DO NOTHING;
ALTER TABLE notif_inbox_items DROP CONSTRAINT ck_notif_inbox_items_action;
ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'OPEN_EVENT', 'OPEN_STATUS', 'OPEN_COMMUNITY', 'NONE'));
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notif_routing
                WHERE category = 'MESSAGING_COMMUNITIES' AND mode <> 'OFF') THEN
        RAISE EXCEPTION 'refusing to remove MESSAGING_COMMUNITIES: its routing is not OFF';
    END IF;
    IF EXISTS (SELECT 1 FROM notif_inbox_items WHERE action_kind = 'OPEN_COMMUNITY') THEN
        RAISE EXCEPTION 'refusing to remove OPEN_COMMUNITY: inbox items still open a community';
    END IF;
END
$$;
ALTER TABLE notif_inbox_items DROP CONSTRAINT ck_notif_inbox_items_action;
ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'OPEN_EVENT', 'OPEN_STATUS', 'NONE'));
DELETE FROM notif_routing WHERE category = 'MESSAGING_COMMUNITIES';
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

//! ADR 0071 (event contract 1.8.0, common contract 1.7.0): Events in Messaging. The event
//! notices (`messaging.event.invited`, `messaging.event.cancelled` and
//! `messaging.event.reminder`, catalogue events #43–#45) are delivered under the routing
//! category `MESSAGING_EVENTS`, in-app and push, so two `notif_routing` rows, seeded `OFF`
//! like every other (category, channel) at the start of its cutover;
//! `catalogue::routing_rows` and its seed test include this file. Their inbox items open
//! the event, so the closed inbox action set gains `OPEN_EVENT`.
//!
//! `down` refuses while a row is not `OFF` or an inbox item still opens an event, then
//! removes the rows and restores the previous action set.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
INSERT INTO notif_routing (category, channel, mode, changed_by_run) VALUES
    ('MESSAGING_EVENTS', 'IN_APP', 'OFF', 'migration'),
    ('MESSAGING_EVENTS', 'PUSH', 'OFF', 'migration')
ON CONFLICT (category, channel) DO NOTHING;
ALTER TABLE notif_inbox_items DROP CONSTRAINT ck_notif_inbox_items_action;
ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'OPEN_EVENT', 'NONE'));
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notif_routing
                WHERE category = 'MESSAGING_EVENTS' AND mode <> 'OFF') THEN
        RAISE EXCEPTION 'refusing to remove MESSAGING_EVENTS: its routing is not OFF';
    END IF;
    IF EXISTS (SELECT 1 FROM notif_inbox_items WHERE action_kind = 'OPEN_EVENT') THEN
        RAISE EXCEPTION 'refusing to remove OPEN_EVENT: inbox items still open an event';
    END IF;
END
$$;
ALTER TABLE notif_inbox_items DROP CONSTRAINT ck_notif_inbox_items_action;
ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'NONE'));
DELETE FROM notif_routing WHERE category = 'MESSAGING_EVENTS';
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

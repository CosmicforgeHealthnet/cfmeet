//! ADR 0069 (event contract 1.9.0, common contract 1.8.0): Status in Messaging. The status
//! notices (`messaging.status.mentioned` and `messaging.status.shared`, catalogue events
//! #46–#47) are delivered under the routing category `MESSAGING_STATUS`, in-app and push,
//! so two `notif_routing` rows, seeded `OFF` like every other (category, channel) at the
//! start of its cutover; `catalogue::routing_rows` and its seed test include this file.
//! Their inbox items open the status, so the closed inbox action set gains `OPEN_STATUS`.
//!
//! The action constraint keeps ADR 0071's `OPEN_EVENT` (m20261218) when it is already
//! there, so the two compose whichever is applied first.
//!
//! `down` refuses while a row is not `OFF` or an inbox item still opens a status, then
//! removes the rows and drops `OPEN_STATUS` from the action set.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
INSERT INTO notif_routing (category, channel, mode, changed_by_run) VALUES
    ('MESSAGING_STATUS', 'IN_APP', 'OFF', 'migration'),
    ('MESSAGING_STATUS', 'PUSH', 'OFF', 'migration')
ON CONFLICT (category, channel) DO NOTHING;
DO $$
DECLARE
    has_event BOOLEAN := EXISTS (SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_notif_inbox_items_action' AND pg_get_constraintdef(oid) LIKE '%OPEN_EVENT%');
BEGIN
    ALTER TABLE notif_inbox_items DROP CONSTRAINT ck_notif_inbox_items_action;
    IF has_event THEN
        ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'OPEN_EVENT', 'OPEN_STATUS', 'NONE'));
    ELSE
        ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'OPEN_STATUS', 'NONE'));
    END IF;
END
$$;
"#;

const DOWN: &str = r#"
DO $$
DECLARE
    has_event BOOLEAN := EXISTS (SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_notif_inbox_items_action' AND pg_get_constraintdef(oid) LIKE '%OPEN_EVENT%');
BEGIN
    IF EXISTS (SELECT 1 FROM notif_routing
                WHERE category = 'MESSAGING_STATUS' AND mode <> 'OFF') THEN
        RAISE EXCEPTION 'refusing to remove MESSAGING_STATUS: its routing is not OFF';
    END IF;
    IF EXISTS (SELECT 1 FROM notif_inbox_items WHERE action_kind = 'OPEN_STATUS') THEN
        RAISE EXCEPTION 'refusing to remove OPEN_STATUS: inbox items still open a status';
    END IF;
    ALTER TABLE notif_inbox_items DROP CONSTRAINT ck_notif_inbox_items_action;
    IF has_event THEN
        ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'OPEN_EVENT', 'NONE'));
    ELSE
        ALTER TABLE notif_inbox_items ADD CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'NONE'));
    END IF;
END
$$;
DELETE FROM notif_routing WHERE category = 'MESSAGING_STATUS';
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

//! Owner decision A (roadmap N12 follow-up): the account notices — welcome, new sign-in
//! alert, email-change attempt and email-changed notices (catalogue events #35–#38,
//! contract `cfmeet.notifications.event` 1.3.0) — are delivered by the service in the
//! CRITICAL lane under the routing category `ACCOUNT_NOTICES` (the Meeting fence category
//! of the same name). Email only, so one `notif_routing` row, seeded `OFF` like every
//! other (category, channel) at the start of its cutover; `catalogue::routing_rows` and
//! its seed test include this file.
//!
//! `down` refuses while the row is not `OFF` and then removes it.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
INSERT INTO notif_routing (category, channel, mode, changed_by_run) VALUES
    ('ACCOUNT_NOTICES', 'EMAIL', 'OFF', 'migration')
ON CONFLICT (category, channel) DO NOTHING;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notif_routing
                WHERE category = 'ACCOUNT_NOTICES' AND mode <> 'OFF') THEN
        RAISE EXCEPTION 'refusing to remove ACCOUNT_NOTICES: its routing is not OFF';
    END IF;
END
$$;
DELETE FROM notif_routing WHERE category = 'ACCOUNT_NOTICES';
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

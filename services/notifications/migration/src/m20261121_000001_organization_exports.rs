//! ADR 0068 A4 (event contract 1.7.0): the organization export notice
//! (`organizations.organization.export_ready`, catalogue event #42) — the single-use
//! download link of an export HQ requested through the admin bridge, sent to the
//! organization owner's account — is delivered under the routing category `ORGANIZATIONS`.
//! Email only, so one `notif_routing` row, seeded `OFF` like every other (category,
//! channel) at the start of its cutover; `catalogue::routing_rows` and its seed test
//! include this file.
//!
//! `down` refuses while the row is not `OFF` and then removes it.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
INSERT INTO notif_routing (category, channel, mode, changed_by_run) VALUES
    ('ORGANIZATIONS', 'EMAIL', 'OFF', 'migration')
ON CONFLICT (category, channel) DO NOTHING;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notif_routing
                WHERE category = 'ORGANIZATIONS' AND mode <> 'OFF') THEN
        RAISE EXCEPTION 'refusing to remove ORGANIZATIONS: its routing is not OFF';
    END IF;
END
$$;
DELETE FROM notif_routing WHERE category = 'ORGANIZATIONS';
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

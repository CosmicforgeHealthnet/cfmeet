//! ADR 0064 (CF Meet Apps) — the presentation of an OAuth client the Meeting Service shows
//! (consent screen, Connected apps, in-meeting apps): an optional developer name override,
//! an https logo URL and the in-meeting surface (`{url, origins}`). Read and written only
//! behind `PLATFORM_ACCESS_OAUTH_ENABLED`.
//!
//! Additive (nullable / defaulted columns). The down migration refuses to run once any of
//! the new values is set.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE platform_oauth_clients
    ADD COLUMN developer_name          VARCHAR(200),
    ADD COLUMN logo_url                VARCHAR(512),
    ADD COLUMN meeting_surface_url     VARCHAR(2048),
    ADD COLUMN meeting_surface_origins TEXT[] NOT NULL DEFAULT '{}',
    ADD CONSTRAINT ck_platform_oauth_clients_developer_name
        CHECK (developer_name IS NULL OR length(btrim(developer_name)) BETWEEN 1 AND 200),
    ADD CONSTRAINT ck_platform_oauth_clients_logo_url
        CHECK (logo_url IS NULL OR (logo_url LIKE 'https://%' AND logo_url !~ '[[:space:][:cntrl:]]')),
    ADD CONSTRAINT ck_platform_oauth_clients_meeting_surface_url
        CHECK (meeting_surface_url IS NULL OR (meeting_surface_url LIKE 'https://%' AND meeting_surface_url !~ '[[:space:][:cntrl:]]')),
    ADD CONSTRAINT ck_platform_oauth_clients_meeting_surface_origins
        CHECK (cardinality(meeting_surface_origins) <= 10
               AND (meeting_surface_url IS NOT NULL OR cardinality(meeting_surface_origins) = 0));
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_oauth_clients WHERE developer_name IS NOT NULL \
                  OR logo_url IS NOT NULL OR meeting_surface_url IS NOT NULL) THEN \
                 RAISE EXCEPTION 'OAuth client presentation data exists; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE platform_oauth_clients \
               DROP CONSTRAINT ck_platform_oauth_clients_meeting_surface_origins, \
               DROP CONSTRAINT ck_platform_oauth_clients_meeting_surface_url, \
               DROP CONSTRAINT ck_platform_oauth_clients_logo_url, \
               DROP CONSTRAINT ck_platform_oauth_clients_developer_name, \
               DROP COLUMN meeting_surface_origins, DROP COLUMN meeting_surface_url, \
               DROP COLUMN logo_url, DROP COLUMN developer_name;",
        )
        .await?;
        Ok(())
    }
}

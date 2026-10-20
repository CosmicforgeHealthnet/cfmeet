//! Part V remediation — server-side inspection of MANAGED media (§80).
//!
//! An `INSPECTION` derivative records what the bytes of a READY MANAGED attachment are
//! (container, measured duration, dimensions) in `metadata`; it has no object. The
//! MANAGED-only CHECK and trigger of migration 16 apply unchanged.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_attachment_derivatives ADD COLUMN metadata JSONB;
ALTER TABLE messaging_attachment_derivatives DROP CONSTRAINT ck_messaging_attachment_derivatives_kind;
ALTER TABLE messaging_attachment_derivatives ADD CONSTRAINT ck_messaging_attachment_derivatives_kind
    CHECK (kind IN ('THUMBNAIL', 'INSPECTION'));
ALTER TABLE messaging_attachment_derivatives DROP CONSTRAINT ck_messaging_attachment_derivatives_ready;
ALTER TABLE messaging_attachment_derivatives ADD CONSTRAINT ck_messaging_attachment_derivatives_ready CHECK (
    (kind = 'THUMBNAIL' AND (status = 'READY') = (object_key IS NOT NULL) AND metadata IS NULL)
    OR (kind = 'INSPECTION' AND object_key IS NULL AND (status = 'READY') = (metadata IS NOT NULL))
);
ALTER TABLE messaging_attachment_derivatives ADD CONSTRAINT ck_messaging_attachment_derivatives_metadata
    CHECK (metadata IS NULL OR jsonb_typeof(metadata) = 'object');
"#;

const DOWN: &str = r#"
DELETE FROM messaging_attachment_derivatives WHERE kind = 'INSPECTION';
ALTER TABLE messaging_attachment_derivatives DROP CONSTRAINT ck_messaging_attachment_derivatives_metadata;
ALTER TABLE messaging_attachment_derivatives DROP CONSTRAINT ck_messaging_attachment_derivatives_ready;
ALTER TABLE messaging_attachment_derivatives ADD CONSTRAINT ck_messaging_attachment_derivatives_ready
    CHECK ((status = 'READY') = (object_key IS NOT NULL));
ALTER TABLE messaging_attachment_derivatives DROP CONSTRAINT ck_messaging_attachment_derivatives_kind;
ALTER TABLE messaging_attachment_derivatives ADD CONSTRAINT ck_messaging_attachment_derivatives_kind
    CHECK (kind IN ('THUMBNAIL'));
ALTER TABLE messaging_attachment_derivatives DROP COLUMN metadata;
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

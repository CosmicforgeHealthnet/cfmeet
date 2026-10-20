//! Part V remediation — server-side derivatives of MANAGED attachments (§80).
//!
//! `messaging_attachment_derivatives`: one derivative (today: a THUMBNAIL of a MANAGED
//! image) per attachment and kind. The database itself refuses a derivative of anything
//! but a MANAGED attachment (CHECK on the copied profile and a trigger on the source
//! attachment): PRIVATE_E2EE content is ciphertext and is never processed by the server.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_attachment_derivatives (
    id                UUID         NOT NULL,
    attachment_id     UUID         NOT NULL REFERENCES messaging_attachments (id) ON DELETE CASCADE,
    environment_id    UUID         NOT NULL,
    security_profile  VARCHAR(16)  NOT NULL,
    kind              VARCHAR(16)  NOT NULL,
    status            VARCHAR(16)  NOT NULL,
    object_key        VARCHAR(512),
    media_type        VARCHAR(64),
    width             INTEGER,
    height            INTEGER,
    source_width      INTEGER,
    source_height     INTEGER,
    bytes             BIGINT,
    sha256            VARCHAR(64),
    error_code        VARCHAR(32),
    attempts          INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at   TIMESTAMPTZ  NOT NULL,
    lease_token       UUID,
    lease_expires_at  TIMESTAMPTZ,
    created_at        TIMESTAMPTZ  NOT NULL,
    completed_at      TIMESTAMPTZ,
    CONSTRAINT pk_messaging_attachment_derivatives PRIMARY KEY (id),
    CONSTRAINT uq_messaging_attachment_derivatives UNIQUE (attachment_id, kind),
    CONSTRAINT ck_messaging_attachment_derivatives_managed CHECK (security_profile = 'MANAGED'),
    CONSTRAINT ck_messaging_attachment_derivatives_kind CHECK (kind IN ('THUMBNAIL')),
    CONSTRAINT ck_messaging_attachment_derivatives_status CHECK (status IN ('PENDING', 'READY', 'FAILED', 'DELETED')),
    CONSTRAINT ck_messaging_attachment_derivatives_ready CHECK ((status = 'READY') = (object_key IS NOT NULL)),
    CONSTRAINT ck_messaging_attachment_derivatives_error CHECK (error_code IS NULL OR error_code ~ '^[A-Z][A-Z0-9_]{1,31}$')
);
CREATE INDEX ix_messaging_attachment_derivatives_due ON messaging_attachment_derivatives (next_attempt_at)
    WHERE status = 'PENDING';

CREATE FUNCTION messaging_derivative_source_is_managed() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT security_profile FROM messaging_attachments WHERE id = NEW.attachment_id) IS DISTINCT FROM 'MANAGED' THEN
        RAISE EXCEPTION 'messaging: derivatives exist only for MANAGED attachments' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_attachment_derivatives_managed BEFORE INSERT OR UPDATE ON messaging_attachment_derivatives
    FOR EACH ROW EXECUTE FUNCTION messaging_derivative_source_is_managed();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_attachment_derivatives;
DROP FUNCTION IF EXISTS messaging_derivative_source_is_managed();
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

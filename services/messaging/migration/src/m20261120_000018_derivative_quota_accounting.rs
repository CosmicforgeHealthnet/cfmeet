//! Part V §80 — count READY MANAGED thumbnail object bytes in the same per-environment
//! committed storage quota as original attachments. This records physical derivatives
//! exactly once across worker retries and releases them on logical deletion.
//!
//! Accounting is transactional with the derivative state change, never inferred from
//! untrusted client metadata. Inspection-only derivatives store no object and cost 0.
//! A one-time backfill accounts READY thumbnails already present before this upgrade.
//! Storage budgets are checked on the next upload reservation under the same row lock.
//! If a derivative takes an environment over its cap, subsequent uploads fail closed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_attachment_derivatives
    ADD CONSTRAINT ck_messaging_thumbnail_accountable_bytes CHECK (
        kind <> 'THUMBNAIL'
        OR status <> 'READY'
        OR (bytes IS NOT NULL AND bytes > 0)
    );

-- This function and the accounting backfill are installed in the migration
-- transaction, so no concurrent writer can observe a partial accounting regime.
CREATE FUNCTION messaging_derivative_storage_account() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
DECLARE
    old_bytes BIGINT := 0;
    new_bytes BIGINT := 0;
    adjustment BIGINT := 0;
    target_environment UUID;
BEGIN
    -- OLD is undefined for INSERT and NEW is undefined for DELETE. Select the
    -- owning environment in separate branches, never in one SQL CASE expression.
    IF TG_OP = 'INSERT' THEN
        target_environment := NEW.environment_id;
    ELSIF TG_OP = 'DELETE' THEN
        target_environment := OLD.environment_id;
    ELSE
        target_environment := NEW.environment_id;
        IF OLD.environment_id IS DISTINCT FROM NEW.environment_id THEN
            RAISE EXCEPTION 'messaging: a derivative cannot change environment' USING ERRCODE = 'P0001';
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        IF OLD.kind = 'THUMBNAIL' AND OLD.status = 'READY' THEN
            old_bytes := OLD.bytes;
        END IF;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        IF NEW.kind = 'THUMBNAIL' AND NEW.status = 'READY' THEN
            new_bytes := NEW.bytes;
        END IF;
    END IF;

    adjustment := new_bytes - old_bytes;
    IF adjustment <> 0 THEN
        UPDATE messaging_storage_usage
        SET committed_bytes = committed_bytes + adjustment, updated_at = now()
        WHERE environment_id = target_environment;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'messaging: derivative storage usage row missing' USING ERRCODE = 'P0001';
        END IF;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER trg_messaging_derivative_storage_account
AFTER INSERT OR UPDATE OR DELETE ON messaging_attachment_derivatives
FOR EACH ROW EXECUTE FUNCTION messaging_derivative_storage_account();

-- A READY thumbnail already written under the previous release counts once.
-- Migration is atomic with the trigger installation, so no duplicate backfill.
UPDATE messaging_storage_usage u SET
    committed_bytes = u.committed_bytes + d.bytes,
    updated_at = now()
FROM (
    SELECT environment_id, SUM(bytes) AS bytes
    FROM messaging_attachment_derivatives
    WHERE kind = 'THUMBNAIL' AND status = 'READY'
    GROUP BY environment_id
) d
WHERE u.environment_id = d.environment_id;
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_messaging_derivative_storage_account
    ON messaging_attachment_derivatives;
DROP FUNCTION IF EXISTS messaging_derivative_storage_account();
-- Undo current READY thumbnail accounting before rolling back the policy.
UPDATE messaging_storage_usage u SET
    committed_bytes = GREATEST(u.committed_bytes - d.bytes, 0),
    updated_at = now()
FROM (
    SELECT environment_id, SUM(bytes) AS bytes
    FROM messaging_attachment_derivatives
    WHERE kind = 'THUMBNAIL' AND status = 'READY'
    GROUP BY environment_id
) d
WHERE u.environment_id = d.environment_id;
ALTER TABLE messaging_attachment_derivatives
    DROP CONSTRAINT IF EXISTS ck_messaging_thumbnail_accountable_bytes;
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

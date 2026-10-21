//! Part V §78 / §80 (ADR 0054) — derivative storage is reserved before it is written,
//! and every stored object stays charged until it is physically deleted.
//!
//! Migration 18 charged a READY thumbnail's bytes after the object was written. That left
//! three gaps this migration closes in the database itself:
//!
//! 1. **Reservation before write.** `messaging_attachment_derivatives.reserved_bytes` holds
//!    the bytes of a rendered thumbnail between the quota decision and the READY update,
//!    with the object key of that attempt in `pending_object_key`. The accounting trigger
//!    now moves both committed and reserved bytes, so a worker crash between the object
//!    upload and the database acknowledgement leaves the attempt reserved (never free and
//!    never counted twice). A thumbnail that does not fit the environment's budget is
//!    `QUOTA_DEFERRED`: no object is written and it is retried later.
//! 2. **Charges follow the object.** `messaging_object_deletions.charged_bytes` carries
//!    the bytes of an object that was logically deleted but still exists (pending, held by
//!    a legal hold, or dead-lettered). Usage is released only when the object deletion is
//!    DONE, so held objects keep their accounting.
//! 3. **Stale attempt objects** are queued for deletion with their reservation as the charge.
//!
//! Objects that were already queued for deletion before this migration were released under
//! the previous rule; they are not charged again (no retroactive charge, no double count).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_attachment_derivatives
    ADD COLUMN reserved_bytes BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN pending_object_key VARCHAR(512),
    ADD CONSTRAINT ck_messaging_derivative_reserved_bytes CHECK (reserved_bytes >= 0),
    ADD CONSTRAINT ck_messaging_derivative_reservation CHECK (
        (reserved_bytes = 0 AND pending_object_key IS NULL)
        OR (status = 'PENDING' AND kind = 'THUMBNAIL' AND reserved_bytes > 0 AND pending_object_key IS NOT NULL)
    );
ALTER TABLE messaging_attachment_derivatives DROP CONSTRAINT ck_messaging_attachment_derivatives_status;
ALTER TABLE messaging_attachment_derivatives ADD CONSTRAINT ck_messaging_attachment_derivatives_status
    CHECK (status IN ('PENDING', 'READY', 'FAILED', 'DELETED', 'QUOTA_DEFERRED'));
DROP INDEX IF EXISTS ix_messaging_attachment_derivatives_due;
CREATE INDEX ix_messaging_attachment_derivatives_due ON messaging_attachment_derivatives (next_attempt_at)
    WHERE status IN ('PENDING', 'QUOTA_DEFERRED');

-- One function accounts every derivative transition: committed bytes for a READY
-- thumbnail, reserved bytes for an attempt between the quota decision and READY.
CREATE OR REPLACE FUNCTION messaging_derivative_storage_account() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
DECLARE
    old_committed BIGINT := 0;
    new_committed BIGINT := 0;
    old_reserved BIGINT := 0;
    new_reserved BIGINT := 0;
    target_environment UUID;
BEGIN
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
            old_committed := OLD.bytes;
        END IF;
        old_reserved := OLD.reserved_bytes;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        IF NEW.kind = 'THUMBNAIL' AND NEW.status = 'READY' THEN
            new_committed := NEW.bytes;
        END IF;
        new_reserved := NEW.reserved_bytes;
    END IF;
    IF new_committed <> old_committed OR new_reserved <> old_reserved THEN
        UPDATE messaging_storage_usage
        SET committed_bytes = GREATEST(committed_bytes + (new_committed - old_committed), 0),
            reserved_bytes = GREATEST(reserved_bytes + (new_reserved - old_reserved), 0),
            updated_at = now()
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

ALTER TABLE messaging_object_deletions
    ADD COLUMN charged_bytes BIGINT NOT NULL DEFAULT 0,
    ADD CONSTRAINT ck_messaging_object_deletions_charged CHECK (charged_bytes >= 0);

-- An object waiting for deletion (PENDING), protected by a legal hold (HELD) or
-- dead-lettered (DEAD) still exists and stays charged; DONE releases it.
CREATE FUNCTION messaging_deletion_storage_account() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
DECLARE
    old_charge BIGINT := 0;
    new_charge BIGINT := 0;
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.environment_id IS DISTINCT FROM NEW.environment_id THEN
        RAISE EXCEPTION 'messaging: an object deletion cannot change environment' USING ERRCODE = 'P0001';
    END IF;
    IF TG_OP <> 'INSERT' AND OLD.status IN ('PENDING', 'HELD', 'DEAD') THEN
        old_charge := OLD.charged_bytes;
    END IF;
    IF TG_OP <> 'DELETE' AND NEW.status IN ('PENDING', 'HELD', 'DEAD') THEN
        new_charge := NEW.charged_bytes;
    END IF;
    IF new_charge <> old_charge THEN
        UPDATE messaging_storage_usage
        SET committed_bytes = GREATEST(committed_bytes + (new_charge - old_charge), 0), updated_at = now()
        WHERE environment_id = COALESCE(NEW.environment_id, OLD.environment_id);
        IF NOT FOUND THEN
            RAISE EXCEPTION 'messaging: deletion storage usage row missing' USING ERRCODE = 'P0001';
        END IF;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER trg_messaging_deletion_storage_account
AFTER INSERT OR UPDATE OR DELETE ON messaging_object_deletions
FOR EACH ROW EXECUTE FUNCTION messaging_deletion_storage_account();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_messaging_deletion_storage_account ON messaging_object_deletions;
DROP FUNCTION IF EXISTS messaging_deletion_storage_account();
-- Release what open deletions still charge (the previous rule released at logical deletion).
UPDATE messaging_storage_usage u SET
    committed_bytes = GREATEST(u.committed_bytes - d.bytes, 0),
    updated_at = now()
FROM (
    SELECT environment_id, SUM(charged_bytes) AS bytes
    FROM messaging_object_deletions
    WHERE status IN ('PENDING', 'HELD', 'DEAD') AND charged_bytes > 0
    GROUP BY environment_id
) d
WHERE u.environment_id = d.environment_id;
ALTER TABLE messaging_object_deletions
    DROP CONSTRAINT IF EXISTS ck_messaging_object_deletions_charged,
    DROP COLUMN IF EXISTS charged_bytes;

-- Release open derivative reservations, then restore migration 18's function.
UPDATE messaging_storage_usage u SET
    reserved_bytes = GREATEST(u.reserved_bytes - d.bytes, 0),
    updated_at = now()
FROM (
    SELECT environment_id, SUM(reserved_bytes) AS bytes
    FROM messaging_attachment_derivatives
    WHERE reserved_bytes > 0
    GROUP BY environment_id
) d
WHERE u.environment_id = d.environment_id;
UPDATE messaging_attachment_derivatives SET status = 'PENDING', error_code = NULL WHERE status = 'QUOTA_DEFERRED';
CREATE OR REPLACE FUNCTION messaging_derivative_storage_account() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
DECLARE
    old_bytes BIGINT := 0;
    new_bytes BIGINT := 0;
    adjustment BIGINT := 0;
    target_environment UUID;
BEGIN
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
DROP INDEX IF EXISTS ix_messaging_attachment_derivatives_due;
CREATE INDEX ix_messaging_attachment_derivatives_due ON messaging_attachment_derivatives (next_attempt_at)
    WHERE status = 'PENDING';
ALTER TABLE messaging_attachment_derivatives DROP CONSTRAINT ck_messaging_attachment_derivatives_status;
ALTER TABLE messaging_attachment_derivatives ADD CONSTRAINT ck_messaging_attachment_derivatives_status
    CHECK (status IN ('PENDING', 'READY', 'FAILED', 'DELETED'));
ALTER TABLE messaging_attachment_derivatives
    DROP CONSTRAINT IF EXISTS ck_messaging_derivative_reservation,
    DROP CONSTRAINT IF EXISTS ck_messaging_derivative_reserved_bytes,
    DROP COLUMN IF EXISTS pending_object_key,
    DROP COLUMN IF EXISTS reserved_bytes;
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

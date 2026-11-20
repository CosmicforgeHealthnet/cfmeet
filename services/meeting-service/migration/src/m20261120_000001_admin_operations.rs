//! ADR 0068, phases A4–A7 — commands relayed by the HQ admin bridge. Additive only.
//!
//! * `meeting_admin_operations` records every bridge command the Meeting Service APPLIED,
//!   in the same transaction as the change, keyed by HQ's `Idempotency-Key`: the command,
//!   its subject, who acted (`hq:<staff>`), who approved it (R3 / R4) and the resulting
//!   state and version. The bridge settles an unknown outcome from this record
//!   (`GET /internal/v1/operator/operations/{key}`); a key that is not here was never
//!   applied. Append-only. Ids, codes and references only: never an address or a reason
//!   text.
//! * `users.account_version`: the account's administration version (status, role and
//!   sign-in address). The trigger moves it forward whenever one of those changes, by any
//!   path (console, self-service, bridge), so a bridge command applies only at the version
//!   HQ looked at. Named so that no existing query joining `users` with a table that has a
//!   `version` column becomes ambiguous.
//!
//! In a SEPARATE history (`meeting_admin_operations_migrations`), so the previous image
//! still starts after it is applied (it ignores the table, the column and the trigger).
//! Required at startup only while `ADMIN_OPERATOR_API_ENABLED=true`.
//!
//! Rollback: DOWN drops the trigger, the column and the table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS meeting_admin_operations (
    idempotency_key   UUID          NOT NULL,
    command           VARCHAR(48)   NOT NULL,
    subject_kind      VARCHAR(32)   NOT NULL,
    subject_id        UUID          NOT NULL,
    operator_ref      VARCHAR(160)  NOT NULL,
    approver_ref      VARCHAR(160),
    approval_ref      VARCHAR(128),
    resulting_state   VARCHAR(32)   NOT NULL,
    resulting_version BIGINT,
    recorded_at       TIMESTAMPTZ   NOT NULL DEFAULT now(),
    CONSTRAINT pk_meeting_admin_operations PRIMARY KEY (idempotency_key),
    CONSTRAINT ck_meeting_admin_operations_command CHECK (command ~ '^[a-z][a-z0-9.-]{2,47}$'),
    CONSTRAINT ck_meeting_admin_operations_subject CHECK (subject_kind IN (
        'USER', 'ACCOUNT_DEVICE', 'IDENTITY_UNIQUENESS', 'CONFERENCE', 'MEETING_SPACE',
        'ARTIFACT', 'CHAT_MIGRATION', 'CRITICAL_DECISION')),
    CONSTRAINT ck_meeting_admin_operations_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_meeting_admin_operations_approver CHECK
        ((approver_ref IS NULL) = (approval_ref IS NULL)
         AND (approver_ref IS NULL OR (approver_ref ~ '^hq:' AND approver_ref <> operator_ref))),
    CONSTRAINT ck_meeting_admin_operations_state CHECK (resulting_state ~ '^[A-Z][A-Z_]{1,31}$'),
    CONSTRAINT ck_meeting_admin_operations_version CHECK (resulting_version IS NULL OR resulting_version >= 1)
);
CREATE INDEX IF NOT EXISTS ix_meeting_admin_operations_subject
    ON meeting_admin_operations (subject_kind, subject_id, recorded_at);

CREATE OR REPLACE FUNCTION meeting_admin_operations_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'meeting_admin_operations is append-only';
END;
$$;
DROP TRIGGER IF EXISTS trg_meeting_admin_operations_append_only ON meeting_admin_operations;
CREATE TRIGGER trg_meeting_admin_operations_append_only
    BEFORE UPDATE OR DELETE ON meeting_admin_operations
    FOR EACH ROW EXECUTE FUNCTION meeting_admin_operations_append_only();
DROP TRIGGER IF EXISTS trg_meeting_admin_operations_no_truncate ON meeting_admin_operations;
CREATE TRIGGER trg_meeting_admin_operations_no_truncate
    BEFORE TRUNCATE ON meeting_admin_operations
    FOR EACH STATEMENT EXECUTE FUNCTION meeting_admin_operations_append_only();

ALTER TABLE users ADD COLUMN IF NOT EXISTS account_version INTEGER NOT NULL DEFAULT 1;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_users_account_version') THEN
        ALTER TABLE users ADD CONSTRAINT ck_users_account_version CHECK (account_version >= 1);
    END IF;
END $$;
CREATE OR REPLACE FUNCTION users_account_version_bump() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.account_version := OLD.account_version + 1;
    RETURN NEW;
END;
$$;
DROP TRIGGER IF EXISTS trg_users_account_version ON users;
CREATE TRIGGER trg_users_account_version
    BEFORE UPDATE ON users
    FOR EACH ROW
    WHEN (OLD.status IS DISTINCT FROM NEW.status
          OR OLD.role IS DISTINCT FROM NEW.role
          OR OLD.email IS DISTINCT FROM NEW.email)
    EXECUTE FUNCTION users_account_version_bump();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_users_account_version ON users;
DROP FUNCTION IF EXISTS users_account_version_bump();
ALTER TABLE users DROP CONSTRAINT IF EXISTS ck_users_account_version;
ALTER TABLE users DROP COLUMN IF EXISTS account_version;
DROP TABLE IF EXISTS meeting_admin_operations;
DROP FUNCTION IF EXISTS meeting_admin_operations_append_only();
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

//! ADR 0068 (admin bridge, phase A4) — commands relayed by the HQ admin bridge. Additive only.
//!
//! * `notif_admin_operations` records every bridge command this service APPLIED, in the same
//!   transaction as the change, keyed by HQ's `Idempotency-Key` (UUID): the command, its
//!   subject (`DELIVERY` id, `EMAIL_SUPPRESSION` keyed digest, `ROUTING`
//!   `<category>/<channel>`, `ROUTE_DECISION` id), who acted (`hq:<staff>`), who approved
//!   it (R3 commands), and the resulting state and version. The bridge settles an unknown
//!   outcome from it (`GET /internal/v1/operator/operations/{key}`); a key that is not here
//!   was never applied. Append-only. Both replicas share it: the primary key makes a key
//!   apply at most once whichever replica serves it.
//! * `notif_email_suppressions` gains `version` (starts at 1), bumped by a row trigger on
//!   every update — whichever code path (provider events, older binaries) changes the row —
//!   so a bridge removal happens only at the version HQ saw.
//!
//! Rollback: DOWN drops the table, the trigger and the column (the changes themselves stay).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE notif_admin_operations (
    idempotency_key   UUID         NOT NULL,
    command           VARCHAR(48)  NOT NULL,
    subject_kind      VARCHAR(24)  NOT NULL,
    subject_id        VARCHAR(80)  NOT NULL,
    operator_ref      VARCHAR(160) NOT NULL,
    approver_ref      VARCHAR(160),
    approval_ref      VARCHAR(128),
    resulting_state   VARCHAR(24)  NOT NULL,
    resulting_version BIGINT,
    recorded_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_admin_operations PRIMARY KEY (idempotency_key),
    CONSTRAINT ck_notif_admin_operations_command CHECK (command ~ '^[a-z][a-z0-9_.-]{1,47}$'),
    CONSTRAINT ck_notif_admin_operations_subject CHECK (subject_kind IN ('DELIVERY', 'EMAIL_SUPPRESSION', 'ROUTING', 'ROUTE_DECISION')),
    CONSTRAINT ck_notif_admin_operations_subject_id CHECK (subject_id ~ '^[A-Za-z0-9_/-]{1,80}$'),
    CONSTRAINT ck_notif_admin_operations_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_notif_admin_operations_approver CHECK
        ((approver_ref IS NULL) = (approval_ref IS NULL)
         AND (approver_ref IS NULL OR (approver_ref ~ '^hq:' AND approver_ref <> operator_ref))),
    CONSTRAINT ck_notif_admin_operations_state CHECK (resulting_state ~ '^[A-Z][A-Z_]{1,23}$'),
    CONSTRAINT ck_notif_admin_operations_version CHECK (resulting_version IS NULL OR resulting_version >= 1)
);
CREATE INDEX ix_notif_admin_operations_subject ON notif_admin_operations (subject_kind, subject_id, recorded_at);
CREATE TRIGGER notif_admin_operations_append_only BEFORE UPDATE OR DELETE ON notif_admin_operations
    FOR EACH ROW EXECUTE FUNCTION notif_append_only();
CREATE TRIGGER notif_admin_operations_no_truncate BEFORE TRUNCATE ON notif_admin_operations
    FOR EACH STATEMENT EXECUTE FUNCTION notif_append_only();

ALTER TABLE notif_email_suppressions ADD COLUMN version BIGINT NOT NULL DEFAULT 1;
ALTER TABLE notif_email_suppressions ADD CONSTRAINT ck_notif_email_suppressions_version CHECK (version >= 1);
CREATE FUNCTION notif_bump_version() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    NEW.version := OLD.version + 1;
    RETURN NEW;
END $$;
CREATE TRIGGER notif_email_suppressions_version BEFORE UPDATE ON notif_email_suppressions
    FOR EACH ROW EXECUTE FUNCTION notif_bump_version();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS notif_email_suppressions_version ON notif_email_suppressions;
DROP FUNCTION IF EXISTS notif_bump_version();
ALTER TABLE notif_email_suppressions DROP CONSTRAINT IF EXISTS ck_notif_email_suppressions_version;
ALTER TABLE notif_email_suppressions DROP COLUMN IF EXISTS version;
DROP TABLE IF EXISTS notif_admin_operations;
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

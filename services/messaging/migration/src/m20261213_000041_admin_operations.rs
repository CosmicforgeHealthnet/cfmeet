//! HQ admin bridge commands (ADR 0068, phases A4 / A5; additive; its own
//! `messaging_admin_operations_seaql_migrations` history).
//!
//! * `messaging_admin_operations` — the append-only operations ledger of the commands the
//!   admin bridge (`cfmeet.admin-bridge`, verified `hq:<staff>` attribution) relays. Each
//!   applied command writes exactly one row, keyed by HQ's `Idempotency-Key`, in the SAME
//!   transaction as its change: a repeated key never applies twice, and the bridge settles
//!   an unknown outcome from it (`GET /internal/v1/operator/operations/{key}`). Only ids,
//!   staff references, codes and versions; `resource` is the small closed answer object
//!   (ids, states, times), never content, a key or a name.
//! * `messaging_reports.version`, `messaging_legal_holds.version` — the row versions a
//!   bridge command must name (`expectedVersion`); resolving a report and releasing a hold
//!   advance them by one.
//!
//! Rollback: the previous image never reads this table, the new columns have defaults
//! (its inserts still work; its updates simply leave the version unchanged) and it never
//! reads this history table, so it keeps booting on this schema.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_admin_operations (
    idempotency_key    UUID          NOT NULL,
    command            VARCHAR(64)   NOT NULL,
    subject_kind       VARCHAR(32)   NOT NULL,
    subject_id         VARCHAR(128)  NOT NULL,
    operator_ref       VARCHAR(160)  NOT NULL,
    approver_ref       VARCHAR(160),
    approval_ref       VARCHAR(128),
    resulting_state    VARCHAR(48)   NOT NULL,
    resulting_version  BIGINT,
    resource           JSONB         NOT NULL,
    recorded_at        TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_admin_operations PRIMARY KEY (idempotency_key),
    CONSTRAINT ck_messaging_admin_operations_command CHECK (command ~ '^messaging\.[a-z][a-z.-]{2,54}$'),
    CONSTRAINT ck_messaging_admin_operations_subject CHECK (subject_kind ~ '^[A-Z][A-Z_]{1,31}$'
        AND length(subject_id) >= 1),
    CONSTRAINT ck_messaging_admin_operations_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_messaging_admin_operations_approver CHECK (approver_ref IS NULL
        OR (approver_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$' AND approver_ref <> operator_ref)),
    CONSTRAINT ck_messaging_admin_operations_approval CHECK ((approver_ref IS NULL) = (approval_ref IS NULL)),
    CONSTRAINT ck_messaging_admin_operations_state CHECK (resulting_state ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_admin_operations_version CHECK (resulting_version IS NULL OR resulting_version >= 0),
    CONSTRAINT ck_messaging_admin_operations_resource CHECK (jsonb_typeof(resource) = 'object'
        AND octet_length(resource::text) <= 4096)
);
CREATE INDEX ix_messaging_admin_operations_subject ON messaging_admin_operations (subject_kind, subject_id, recorded_at);
CREATE FUNCTION messaging_admin_operations_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'messaging: admin operations are append-only' USING ERRCODE = 'P0001';
END $$;
CREATE TRIGGER trg_messaging_admin_operations_guard BEFORE UPDATE OR DELETE ON messaging_admin_operations
    FOR EACH ROW EXECUTE FUNCTION messaging_admin_operations_guard();

ALTER TABLE messaging_reports
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_messaging_reports_version CHECK (version >= 1);
ALTER TABLE messaging_legal_holds
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_messaging_legal_holds_version CHECK (version >= 1);
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_legal_holds DROP CONSTRAINT IF EXISTS ck_messaging_legal_holds_version;
ALTER TABLE messaging_legal_holds DROP COLUMN IF EXISTS version;
ALTER TABLE messaging_reports DROP CONSTRAINT IF EXISTS ck_messaging_reports_version;
ALTER TABLE messaging_reports DROP COLUMN IF EXISTS version;
DROP TABLE IF EXISTS messaging_admin_operations;
DROP FUNCTION IF EXISTS messaging_admin_operations_guard();
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

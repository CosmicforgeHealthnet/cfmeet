//! Admin bridge operations ledger (ADR 0068, design §9, phase A3.0).
//!
//! `admin_operations` records every HQ command the bridge accepts, keyed by the calling
//! staff member and HQ's `Idempotency-Key`:
//!
//! * a replay with the same intent returns the stored outcome; a different intent under the
//!   same key is refused (`IDEMPOTENCY_CONFLICT`);
//! * at most one operation holds a target's expected version unless it FAILED (a second
//!   one is refused with `VERSION_CONFLICT`, nothing done);
//! * send once: `PENDING` (never sent) → `SENT` → `SUCCEEDED` | `FAILED` |
//!   `UNKNOWN_OUTCOME`; `PENDING` → `FAILED` (refused before the owner call);
//!   `UNKNOWN_OUTCOME` → `SUCCEEDED` | `FAILED` only by reading the owner's own record.
//!   Settled rows never change; rows are never deleted.
//!
//! No business data: the owner's result is reduced to its status and version.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE admin_operations (
    principal_sub     VARCHAR(128) NOT NULL,
    idempotency_key   UUID         NOT NULL,
    operation_id      UUID         NOT NULL,
    capability        VARCHAR(96)  NOT NULL,
    owner             VARCHAR(32)  NOT NULL,
    purpose           VARCHAR(64)  NOT NULL,
    scope             JSON         NOT NULL,
    intent_sha256     VARCHAR(64)  NOT NULL,
    target            VARCHAR(256) NOT NULL,
    expected_version  VARCHAR(64)  NOT NULL,
    approval_id       VARCHAR(128),
    correlation_id    UUID         NOT NULL,
    status            VARCHAR(16)  NOT NULL,
    source_status     VARCHAR(64),
    source_version    VARCHAR(64),
    error_code        VARCHAR(64),
    created_at        TIMESTAMPTZ  NOT NULL,
    sent_at           TIMESTAMPTZ,
    settled_at        TIMESTAMPTZ,
    CONSTRAINT pk_admin_operations PRIMARY KEY (principal_sub, idempotency_key),
    CONSTRAINT ck_admin_operations_status CHECK
        (status IN ('PENDING', 'SENT', 'SUCCEEDED', 'FAILED', 'UNKNOWN_OUTCOME')),
    CONSTRAINT ck_admin_operations_intent CHECK (intent_sha256 ~ '^[0-9a-f]{64}$'),
    -- FAILED may or may not have been sent (refused before, or by the owner after).
    CONSTRAINT ck_admin_operations_sent CHECK
        (status = 'FAILED' OR (status = 'PENDING') = (sent_at IS NULL)),
    CONSTRAINT ck_admin_operations_settled CHECK
        ((status IN ('SUCCEEDED', 'FAILED')) = (settled_at IS NOT NULL)),
    CONSTRAINT ck_admin_operations_error CHECK
        (error_code IS NULL OR error_code ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
-- One live (or succeeded) operation per target and expected version.
CREATE UNIQUE INDEX uq_admin_operations_target ON admin_operations (target, expected_version)
    WHERE status <> 'FAILED';
-- Settlement sweep.
CREATE INDEX ix_admin_operations_open ON admin_operations (created_at)
    WHERE status IN ('PENDING', 'SENT', 'UNKNOWN_OUTCOME');

CREATE FUNCTION admin_operations_transition() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'admin_operations: rows are never deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF (NEW.principal_sub, NEW.idempotency_key, NEW.operation_id, NEW.capability, NEW.owner,
        NEW.purpose, NEW.scope::text, NEW.intent_sha256, NEW.target, NEW.expected_version,
        NEW.approval_id, NEW.correlation_id, NEW.created_at)
       IS DISTINCT FROM
       (OLD.principal_sub, OLD.idempotency_key, OLD.operation_id, OLD.capability, OLD.owner,
        OLD.purpose, OLD.scope::text, OLD.intent_sha256, OLD.target, OLD.expected_version,
        OLD.approval_id, OLD.correlation_id, OLD.created_at)
       OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN
        RAISE EXCEPTION 'admin_operations: only the outcome may change' USING ERRCODE = 'check_violation';
    END IF;
    IF NOT ((OLD.status = 'PENDING' AND NEW.status IN ('SENT', 'FAILED'))
         OR (OLD.status = 'SENT' AND NEW.status IN ('SUCCEEDED', 'FAILED', 'UNKNOWN_OUTCOME'))
         OR (OLD.status = 'UNKNOWN_OUTCOME' AND NEW.status IN ('SUCCEEDED', 'FAILED'))) THEN
        RAISE EXCEPTION 'admin_operations: % cannot become %', OLD.status, NEW.status
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_admin_operations_transition BEFORE UPDATE OR DELETE ON admin_operations
    FOR EACH ROW EXECUTE FUNCTION admin_operations_transition();
CREATE TRIGGER tr_admin_operations_truncate BEFORE TRUNCATE ON admin_operations
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS admin_operations;
DROP FUNCTION IF EXISTS admin_operations_transition();
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

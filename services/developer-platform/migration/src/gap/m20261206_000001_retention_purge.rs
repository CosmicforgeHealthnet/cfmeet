//! P9 — bounded retention purge of webhook, API usage and activity history.
//!
//! These tables were append-only / never-delete by trigger, with no purge at all. This
//! migration keeps every guard against ordinary deletes (and every UPDATE guard as it
//! is) and lets ONLY the retention worker delete: a DELETE is allowed when the
//! transaction set `SET LOCAL devplat.retention_worker = 'on'` (the worker does, inside
//! the transaction of each bounded batch). Truncation stays refused.
//!
//! * `devplat_retention_audit`: one append-only row per purge batch (class, rows,
//!   outcome, policy) — the durable per-class counters operators read.
//!
//! Rollback: DOWN restores the original triggers / guard bodies and drops the audit
//! table (nothing else is lost; purged rows were due).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Tables whose `BEFORE UPDATE OR DELETE ... devplat_append_only()` trigger is split into
/// an UPDATE guard (unchanged) and a retention-aware DELETE guard:
/// `(table, original trigger name)`.
const APPEND_ONLY: &[(&str, &str)] = &[
    (
        "devplat_webhook_events",
        "tr_devplat_webhook_events_append_only",
    ),
    (
        "devplat_webhook_attempts",
        "tr_devplat_webhook_attempts_append_only",
    ),
    (
        "devplat_activity_events",
        "tr_devplat_activity_events_append_only",
    ),
    (
        "devplat_api_usage_batches",
        "trg_devplat_api_usage_batches_append_only",
    ),
    (
        "devplat_usage_rejections",
        "tr_devplat_usage_rejections_append_only",
    ),
    (
        "devplat_event_rejections",
        "tr_devplat_event_rejections_append_only",
    ),
];

const DAILY_GUARD_ORIGINAL: &str = r#"
CREATE OR REPLACE FUNCTION devplat_api_usage_daily_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'devplat_api_usage_daily: usage history is never deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.environment_id <> OLD.environment_id OR NEW.usage_day <> OLD.usage_day OR NEW.request_count < OLD.request_count THEN
        RAISE EXCEPTION 'devplat_api_usage_daily: counters only grow' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
"#;

const OPERATION_GUARD_ORIGINAL: &str = r#"
CREATE OR REPLACE FUNCTION devplat_api_operation_usage_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'devplat_api_operation_usage: usage history is never deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.environment_id <> OLD.environment_id OR NEW.usage_day <> OLD.usage_day OR NEW.domain <> OLD.domain
       OR NEW.operation_id <> OLD.operation_id OR NEW.status_class <> OLD.status_class
       OR NEW.request_count < OLD.request_count THEN
        RAISE EXCEPTION 'devplat_api_operation_usage: counters only grow' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
"#;

fn up_sql() -> String {
    let mut sql = String::from(
        r#"
CREATE FUNCTION devplat_delete_unless_retention() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF current_setting('devplat.retention_worker', true) = 'on' THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION '%: rows are deleted only by the retention worker', TG_TABLE_NAME
        USING ERRCODE = 'check_violation';
END $$;
"#,
    );
    for (table, trigger) in APPEND_ONLY {
        sql.push_str(&format!(
            "DROP TRIGGER {trigger} ON {table};\n\
             CREATE TRIGGER {trigger} BEFORE UPDATE ON {table}\n    \
                 FOR EACH ROW EXECUTE FUNCTION devplat_append_only();\n\
             CREATE TRIGGER {trigger}_retention BEFORE DELETE ON {table}\n    \
                 FOR EACH ROW EXECUTE FUNCTION devplat_delete_unless_retention();\n"
        ));
    }
    sql.push_str(
        r#"
DROP TRIGGER tr_devplat_webhook_deliveries_no_delete ON devplat_webhook_deliveries;
CREATE TRIGGER tr_devplat_webhook_deliveries_no_delete BEFORE DELETE ON devplat_webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION devplat_delete_unless_retention();
CREATE OR REPLACE FUNCTION devplat_api_operation_usage_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF current_setting('devplat.retention_worker', true) = 'on' THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'devplat_api_operation_usage: usage history is deleted only by the retention worker'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.environment_id <> OLD.environment_id OR NEW.usage_day <> OLD.usage_day OR NEW.domain <> OLD.domain
       OR NEW.operation_id <> OLD.operation_id OR NEW.status_class <> OLD.status_class
       OR NEW.request_count < OLD.request_count THEN
        RAISE EXCEPTION 'devplat_api_operation_usage: counters only grow' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE OR REPLACE FUNCTION devplat_api_usage_daily_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF current_setting('devplat.retention_worker', true) = 'on' THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'devplat_api_usage_daily: usage history is deleted only by the retention worker'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.environment_id <> OLD.environment_id OR NEW.usage_day <> OLD.usage_day OR NEW.request_count < OLD.request_count THEN
        RAISE EXCEPTION 'devplat_api_usage_daily: counters only grow' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TABLE devplat_retention_audit (
    id            UUID         NOT NULL,
    data_class    VARCHAR(48)  NOT NULL,
    deleted_rows  BIGINT       NOT NULL,
    outcome       VARCHAR(16)  NOT NULL,
    policy        VARCHAR(80)  NOT NULL,
    occurred_at   TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_retention_audit PRIMARY KEY (id),
    CONSTRAINT ck_devplat_retention_audit_class CHECK (data_class ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_devplat_retention_audit_rows CHECK (deleted_rows >= 0),
    CONSTRAINT ck_devplat_retention_audit_outcome CHECK (outcome IN ('DELETED', 'FAILED')),
    CONSTRAINT ck_devplat_retention_audit_policy CHECK (policy ~ '^[A-Z][A-Z0-9_]{0,63}=[0-9]{1,5}$')
);
CREATE INDEX ix_devplat_retention_audit_class ON devplat_retention_audit (data_class, occurred_at DESC);
CREATE TRIGGER tr_devplat_retention_audit_append_only BEFORE UPDATE OR DELETE ON devplat_retention_audit
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
"#,
    );
    sql
}

fn down_sql() -> String {
    let mut sql = String::from(
        "DROP TABLE IF EXISTS devplat_retention_audit;\n\
         DROP TRIGGER IF EXISTS tr_devplat_webhook_deliveries_no_delete ON devplat_webhook_deliveries;\n\
         CREATE TRIGGER tr_devplat_webhook_deliveries_no_delete BEFORE DELETE ON devplat_webhook_deliveries\n    \
             FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();\n",
    );
    sql.push_str(OPERATION_GUARD_ORIGINAL);
    sql.push_str(DAILY_GUARD_ORIGINAL);
    for (table, trigger) in APPEND_ONLY {
        sql.push_str(&format!(
            "DROP TRIGGER IF EXISTS {trigger}_retention ON {table};\n\
             DROP TRIGGER IF EXISTS {trigger} ON {table};\n\
             CREATE TRIGGER {trigger} BEFORE UPDATE OR DELETE ON {table}\n    \
                 FOR EACH ROW EXECUTE FUNCTION devplat_append_only();\n"
        ));
    }
    sql.push_str("DROP FUNCTION IF EXISTS devplat_delete_unless_retention();\n");
    sql
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&up_sql())
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&down_sql())
            .await?;
        Ok(())
    }
}

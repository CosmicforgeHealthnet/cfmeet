//! Retention (Part H §80, §102, §106): Telephony's part.
//!
//! * `telephony_retention_audit`: append-only record of every retention batch (data
//!   class, action, rows, policy, actor, outcome).
//! * `telephony_retention_holds`: a retention hold keeps a leg's, a provider's or a whole
//!   data class's evidence out of the purge until it is released (neutral wording: a
//!   "hold", whatever its operational reason).
//! * Deletes of `provider_events` and `telephony_incidents` are refused unless the
//!   session is a retention worker (`SET LOCAL telephony.retention_worker = 'on'`): a
//!   plain `DELETE` from any other session fails.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE FUNCTION telephony_forbid_delete_outside_retention_worker() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_setting('telephony.retention_worker', true) IS DISTINCT FROM 'on' THEN
        RAISE EXCEPTION 'telephony invariant violated: % rows are deleted by the retention worker only', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN OLD;
END;
$$;

CREATE TABLE telephony_retention_audit (
    id          UUID        PRIMARY KEY,
    data_class  TEXT        NOT NULL,
    action      TEXT        NOT NULL,
    batch_count INTEGER     NOT NULL,
    policy      VARCHAR(64) NOT NULL,
    actor       VARCHAR(64) NOT NULL,
    outcome     TEXT        NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT ck_telephony_retention_audit_class CHECK (data_class IN (
        'PROVIDER_EVENTS', 'TELEPHONY_INCIDENTS', 'INGRESS_WINDOWS', 'NORMALIZED_LEG_EVIDENCE', 'CAPTURE_SPOOL')),
    CONSTRAINT ck_telephony_retention_audit_action CHECK (action IN ('DELETE', 'STATUS_ONLY')),
    CONSTRAINT ck_telephony_retention_audit_count CHECK (batch_count >= 0),
    CONSTRAINT ck_telephony_retention_audit_outcome CHECK (outcome IN ('DELETED', 'NOTHING_DUE', 'FAILED')),
    CONSTRAINT ck_telephony_retention_audit_actor CHECK (actor ~ '^[A-Za-z0-9][A-Za-z0-9._@:-]{1,63}$')
);
CREATE INDEX idx_telephony_retention_audit_occurred ON telephony_retention_audit (occurred_at);
CREATE TRIGGER trg_telephony_retention_audit_append_only
    BEFORE UPDATE OR DELETE ON telephony_retention_audit
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();
CREATE TRIGGER trg_telephony_retention_audit_no_truncate
    BEFORE TRUNCATE ON telephony_retention_audit
    FOR EACH STATEMENT EXECUTE FUNCTION telephony_forbid_mutation();

CREATE TABLE telephony_retention_holds (
    id               UUID        PRIMARY KEY,
    data_class       TEXT        NOT NULL,
    telephony_leg_id UUID        REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    provider_id      VARCHAR(32) REFERENCES telecom_providers (id) ON DELETE RESTRICT,
    reason_code      TEXT        NOT NULL,
    created_by       VARCHAR(64) NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL,
    released_at      TIMESTAMPTZ,
    CONSTRAINT ck_telephony_retention_holds_class CHECK (data_class IN (
        'PROVIDER_EVENTS', 'TELEPHONY_INCIDENTS', 'NORMALIZED_LEG_EVIDENCE')),
    CONSTRAINT ck_telephony_retention_holds_reason CHECK (reason_code IN (
        'INVESTIGATION', 'DISPUTE', 'REGULATORY_REQUEST', 'INCIDENT_REVIEW', 'OTHER')),
    CONSTRAINT ck_telephony_retention_holds_scope CHECK (telephony_leg_id IS NULL OR provider_id IS NULL),
    CONSTRAINT ck_telephony_retention_holds_actor CHECK (created_by ~ '^[A-Za-z0-9][A-Za-z0-9._@:-]{1,63}$'),
    CONSTRAINT ck_telephony_retention_holds_released CHECK (released_at IS NULL OR released_at >= created_at)
);
CREATE INDEX idx_telephony_retention_holds_active ON telephony_retention_holds (data_class) WHERE released_at IS NULL;
CREATE TRIGGER trg_telephony_retention_holds_immutable
    BEFORE UPDATE ON telephony_retention_holds
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'data_class', 'telephony_leg_id', 'provider_id', 'reason_code', 'created_by', 'created_at');
CREATE TRIGGER trg_telephony_retention_holds_release_once
    BEFORE UPDATE ON telephony_retention_holds
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment('released_at');
CREATE TRIGGER trg_telephony_retention_holds_no_delete
    BEFORE DELETE ON telephony_retention_holds
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();

CREATE TRIGGER trg_provider_events_retention_worker_only
    BEFORE DELETE ON provider_events
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_delete_outside_retention_worker();
CREATE TRIGGER trg_telephony_incidents_retention_worker_only
    BEFORE DELETE ON telephony_incidents
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_delete_outside_retention_worker();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_telephony_incidents_retention_worker_only ON telephony_incidents;
DROP TRIGGER IF EXISTS trg_provider_events_retention_worker_only ON provider_events;
DROP TABLE IF EXISTS telephony_retention_holds;
DROP TABLE IF EXISTS telephony_retention_audit;
DROP FUNCTION IF EXISTS telephony_forbid_delete_outside_retention_worker();
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

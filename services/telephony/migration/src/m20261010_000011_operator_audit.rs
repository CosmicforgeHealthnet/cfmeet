//! Operator audit (Part H §45, §49): every mutating `telephony-admin` action — provider /
//! route enablement and circuit overrides, relay-number binding changes, destination
//! class rules, retention holds — is recorded in the SAME transaction as its effect.
//! Append-only (no UPDATE / DELETE / TRUNCATE); the detail is a bounded JSON object of
//! safe operational values (never a secret, never a full telephone number).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_operator_audit (
    id             UUID         PRIMARY KEY,
    actor          VARCHAR(64)  NOT NULL,
    correlation_id VARCHAR(64)  NOT NULL,
    action         TEXT         NOT NULL,
    target_kind    TEXT         NOT NULL,
    target_id      VARCHAR(64)  NOT NULL,
    detail         JSONB        NOT NULL,
    occurred_at    TIMESTAMPTZ  NOT NULL,
    CONSTRAINT ck_telephony_operator_audit_actor CHECK (actor ~ '^[A-Za-z0-9][A-Za-z0-9._@:-]{1,63}$'),
    CONSTRAINT ck_telephony_operator_audit_correlation CHECK (correlation_id ~ '^[A-Za-z0-9][A-Za-z0-9._-]{3,63}$'),
    CONSTRAINT ck_telephony_operator_audit_action CHECK (action IN (
        'PROVIDER_ADD', 'PROVIDER_ENABLE', 'PROVIDER_DISABLE', 'PROVIDER_CIRCUIT',
        'ROUTE_ADD', 'ROUTE_ENABLE', 'ROUTE_DISABLE', 'ROUTE_CIRCUIT',
        'SIP_PEER_ADD', 'NUMBER_BIND', 'NUMBER_DISABLE',
        'ENTERPRISE_DEVICE_ADD', 'ENTERPRISE_DEVICE_ENABLE', 'ENTERPRISE_DEVICE_DISABLE',
        'DESTINATION_CLASS_UPSERT', 'DESTINATION_CLASS_DISABLE',
        'RETENTION_HOLD', 'RETENTION_RELEASE')),
    CONSTRAINT ck_telephony_operator_audit_target_kind CHECK (target_kind IN (
        'PROVIDER', 'ROUTE', 'SIP_PEER', 'NUMBER_BINDING', 'ENTERPRISE_DEVICE', 'DESTINATION_CLASS', 'RETENTION_HOLD')),
    CONSTRAINT ck_telephony_operator_audit_target_id CHECK (target_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$'),
    CONSTRAINT ck_telephony_operator_audit_detail CHECK (
        jsonb_typeof(detail) = 'object' AND octet_length(detail::text) <= 2048)
);
CREATE INDEX idx_telephony_operator_audit_occurred ON telephony_operator_audit (occurred_at);
CREATE INDEX idx_telephony_operator_audit_target ON telephony_operator_audit (target_kind, target_id);
CREATE TRIGGER trg_telephony_operator_audit_append_only
    BEFORE UPDATE OR DELETE ON telephony_operator_audit
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();
CREATE TRIGGER trg_telephony_operator_audit_no_truncate
    BEFORE TRUNCATE ON telephony_operator_audit
    FOR EACH STATEMENT EXECUTE FUNCTION telephony_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS telephony_operator_audit;
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

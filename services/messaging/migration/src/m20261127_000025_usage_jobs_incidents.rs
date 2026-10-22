//! Part V §129 / §136 / §141.
//!
//! * Usage facts also cover realtime connection time, attachment egress and search
//!   queries (still facts only — no price, rate or cost).
//! * Incident categories for an unavailable scanner, a retention backlog, storage
//!   accounting drift and an abuse wave.
//! * `messaging_key_package_alerts` — one open low-stock alert per device (the
//!   maintenance pass raises it once; replenishing the device's KeyPackages clears it).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_usage_records DROP CONSTRAINT ck_messaging_usage_records_type;
ALTER TABLE messaging_usage_records ADD CONSTRAINT ck_messaging_usage_records_type CHECK (
    (usage_type = 'MESSAGE_ACCEPTED' AND unit = 'MESSAGES') OR
    (usage_type = 'ATTACHMENT_STORED' AND unit = 'BYTES') OR
    (usage_type = 'SMS_BRIDGED' AND unit = 'SEGMENTS') OR
    (usage_type = 'REALTIME_CONNECTION' AND unit = 'SECONDS') OR
    (usage_type = 'ATTACHMENT_EGRESS' AND unit = 'BYTES') OR
    (usage_type = 'SEARCH_QUERY' AND unit = 'QUERIES'));

ALTER TABLE messaging_incidents DROP CONSTRAINT ck_messaging_incidents_category;
ALTER TABLE messaging_incidents ADD CONSTRAINT ck_messaging_incidents_category CHECK (category IN (
    'OUTBOX_DEAD_LETTER', 'WEBHOOK_RELAY_DEAD_LETTER', 'SMS_TRANSPORT_FAILED',
    'ATTACHMENT_DELETE_DEAD_LETTER', 'KT_ROLLBACK_DETECTED',
    'SCANNER_UNAVAILABLE', 'RETENTION_BACKLOG', 'STORAGE_ACCOUNTING_DRIFT', 'ABUSE_WAVE'));

CREATE TABLE messaging_key_package_alerts (
    device_id       UUID        NOT NULL,
    environment_id  UUID        NOT NULL,
    available       INTEGER     NOT NULL,
    alerted_at      TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_messaging_key_package_alerts PRIMARY KEY (device_id),
    CONSTRAINT fk_messaging_key_package_alerts_device FOREIGN KEY (device_id) REFERENCES messaging_devices (id),
    CONSTRAINT ck_messaging_key_package_alerts_available CHECK (available >= 0)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_key_package_alerts;
DELETE FROM messaging_incidents WHERE category IN ('SCANNER_UNAVAILABLE', 'RETENTION_BACKLOG', 'STORAGE_ACCOUNTING_DRIFT', 'ABUSE_WAVE');
ALTER TABLE messaging_incidents DROP CONSTRAINT ck_messaging_incidents_category;
ALTER TABLE messaging_incidents ADD CONSTRAINT ck_messaging_incidents_category CHECK (category IN (
    'OUTBOX_DEAD_LETTER', 'WEBHOOK_RELAY_DEAD_LETTER', 'SMS_TRANSPORT_FAILED',
    'ATTACHMENT_DELETE_DEAD_LETTER', 'KT_ROLLBACK_DETECTED'));
ALTER TABLE messaging_usage_records DROP CONSTRAINT ck_messaging_usage_records_type;
ALTER TABLE messaging_usage_records ADD CONSTRAINT ck_messaging_usage_records_type CHECK (
    (usage_type = 'MESSAGE_ACCEPTED' AND unit = 'MESSAGES') OR
    (usage_type = 'ATTACHMENT_STORED' AND unit = 'BYTES') OR
    (usage_type = 'SMS_BRIDGED' AND unit = 'SEGMENTS')) NOT VALID;
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

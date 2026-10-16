//! Destination classification rules (Part H §66).
//!
//! `telecom_destination_classes` holds PREFIX rules per destination country: the
//! national-significant leading digits (after the country calling code) of a number
//! range and its class (`STANDARD`, `PREMIUM`, `SHARED_COST`, `SATELLITE`, `HIGH_RISK`).
//! A prefix of at most 15 digits is a class rule for a number RANGE, never a stored
//! telephone number; the executor matches the longest enabled prefix within the
//! destination country before any provider action. With no rule loaded a destination
//! is "unclassified" — which is not "standard" (`TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telecom_destination_classes (
    id         UUID        PRIMARY KEY,
    country    CHAR(2)     NOT NULL,
    prefix     VARCHAR(15) NOT NULL,
    class      TEXT        NOT NULL,
    source     VARCHAR(64) NOT NULL,
    enabled    BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT uq_telecom_destination_classes_rule UNIQUE (country, prefix),
    CONSTRAINT ck_telecom_destination_classes_country CHECK (country ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_telecom_destination_classes_prefix CHECK (prefix ~ '^[0-9]{1,15}$'),
    CONSTRAINT ck_telecom_destination_classes_class CHECK (class IN (
        'STANDARD', 'PREMIUM', 'SHARED_COST', 'SATELLITE', 'HIGH_RISK')),
    CONSTRAINT ck_telecom_destination_classes_source CHECK (source ~ '^[A-Za-z0-9][A-Za-z0-9._:/-]{0,63}$'),
    CONSTRAINT ck_telecom_destination_classes_updated CHECK (updated_at >= created_at)
);
CREATE INDEX idx_telecom_destination_classes_country ON telecom_destination_classes (country) WHERE enabled;
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS telecom_destination_classes;
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

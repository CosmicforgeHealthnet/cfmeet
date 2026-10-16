//! Compliance eligibility of an environment for paid public-network routes (Part H
//! §78, §81).
//!
//! One row per environment, set by an operator only (`platform_admin compliance set`,
//! audited). `countries` lists the ISO 3166-1 alpha-2 destination countries the
//! assessment covers (empty = every country); `evidence_ref` is an opaque reference to
//! the operator's record system, never a document, number or address. The Routing
//! Engine requires an ELIGIBLE, unexpired row covering the destination country for every
//! country the platform overlay or the customer policy marks as compliance-required.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_compliance_eligibility (
    id             UUID         NOT NULL,
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    environment_id UUID         NOT NULL,
    status         TEXT         NOT NULL DEFAULT 'NOT_ASSESSED',
    countries      TEXT[]       NOT NULL DEFAULT '{}',
    evidence_ref   VARCHAR(200),
    decided_by     VARCHAR(100) NOT NULL,
    decided_at     TIMESTAMPTZ  NOT NULL,
    expires_at     TIMESTAMPTZ,
    version        BIGINT       NOT NULL DEFAULT 1,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_compliance_eligibility PRIMARY KEY (id),
    CONSTRAINT uq_platform_compliance_eligibility_environment UNIQUE (environment_id),
    CONSTRAINT fk_platform_compliance_eligibility_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_compliance_eligibility_status
        CHECK (status IN ('NOT_ASSESSED', 'ELIGIBLE', 'INELIGIBLE')),
    CONSTRAINT ck_platform_compliance_eligibility_countries
        CHECK (array_position(countries, NULL) IS NULL AND cardinality(countries) <= 250),
    CONSTRAINT ck_platform_compliance_eligibility_evidence
        CHECK (evidence_ref IS NULL OR evidence_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:/-]{0,199}$'),
    CONSTRAINT ck_platform_compliance_eligibility_decided_by
        CHECK (decided_by ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$'),
    CONSTRAINT ck_platform_compliance_eligibility_expiry
        CHECK (expires_at IS NULL OR expires_at > decided_at),
    CONSTRAINT ck_platform_compliance_eligibility_version CHECK (version >= 1)
);
CREATE TRIGGER trg_platform_compliance_eligibility_immutable
    BEFORE UPDATE ON platform_compliance_eligibility
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_compliance_eligibility;
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

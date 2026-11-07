//! Blueprint v1.0 Section 12 follow-up (CAL-007 / ORG-012 / CAL-009): per-organization
//! service bindings of the organization's technical scope.
//!
//! `organization_scope_service_bindings` records, for one organization scope capability
//! (`primary`) and one consuming service (`calling` only), the Platform Access service
//! account and credential an operator created inside the organization's own FIRST_PARTY
//! triple. These are opaque identifiers only (ADR 0056 §4: Organizations stores ids and
//! evidence, never a secret): no credential value, verifier or scope grant is stored, and
//! Platform Access re-verifies the credential against the triple on every use, so a wrong
//! binding can only refuse. Additive: one new table, nothing existing changes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE organization_scope_service_bindings (
    organization_id     UUID         NOT NULL,
    capability          VARCHAR(64)  NOT NULL DEFAULT 'primary',
    service             VARCHAR(32)  NOT NULL,
    service_account_id  UUID         NOT NULL,
    credential_id       UUID         NOT NULL,
    bound_by            UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_organization_scope_service_bindings PRIMARY KEY (organization_id, capability, service),
    CONSTRAINT fk_organization_scope_service_bindings_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_scope_service_bindings_capability CHECK (capability ~ '^[a-z][a-z0-9-]{0,31}$'),
    CONSTRAINT ck_organization_scope_service_bindings_service CHECK (service IN ('calling')),
    CONSTRAINT ck_organization_scope_service_bindings_version CHECK (version >= 1)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS organization_scope_service_bindings;
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

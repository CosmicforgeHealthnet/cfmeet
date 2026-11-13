//! ADR 0058 Amendment A2 — automatic organization service bindings.
//!
//! * `platform_organization_service_bindings`: the registry of the Calling / Messaging
//!   service account and credential Platform Access created for one first-party
//!   organization INSIDE that organization's own provisioned FIRST_PARTY triple
//!   (`platform_organization_scopes`). One row per `(organization_ref, capability,
//!   service)` is the idempotency key of the binding operation. The service account is
//!   pinned to the triple by a composite foreign key; a service account backs at most one
//!   binding. Rows are mutable only to follow credential renewal / rotation (`revision`)
//!   and revocation (`revoked_at`, terminal). The raw credential is never stored here (the
//!   credential table keeps only its verifier).
//! * No new audit action: the binding operations write the existing SCOPE_GRANTED,
//!   SERVICE_ACCOUNT_CREATED, SERVICE_ACCOUNT_STATUS_SET and CREDENTIAL_* actions with
//!   `detail.organizationRef` / `detail.service`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_organization_service_bindings (
    organization_ref   UUID         NOT NULL,
    capability         VARCHAR(32)  NOT NULL,
    service            VARCHAR(32)  NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    service_account_id UUID         NOT NULL,
    credential_id      UUID         NOT NULL,
    revision           BIGINT       NOT NULL DEFAULT 1,
    revoked_at         TIMESTAMPTZ,
    requested_by       VARCHAR(64)  NOT NULL,
    created_at         TIMESTAMPTZ  NOT NULL,
    updated_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_organization_service_bindings PRIMARY KEY (organization_ref, capability, service),
    CONSTRAINT uq_platform_organization_service_bindings_account UNIQUE (service_account_id),
    CONSTRAINT fk_platform_organization_service_bindings_scope
        FOREIGN KEY (organization_ref, capability)
        REFERENCES platform_organization_scopes (organization_ref, capability) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_organization_service_bindings_account
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_organization_service_bindings_credential
        FOREIGN KEY (credential_id) REFERENCES platform_api_credentials (id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_organization_service_bindings_service CHECK (service IN ('calling', 'messaging')),
    CONSTRAINT ck_platform_organization_service_bindings_revision CHECK (revision > 0),
    CONSTRAINT ck_platform_organization_service_bindings_requester CHECK (requested_by ~ '^cfmeet\.[a-z][a-z0-9-]*$')
);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_organization_service_bindings) THEN \
                 RAISE EXCEPTION 'organization service bindings exist; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared("DROP TABLE platform_organization_service_bindings;")
            .await?;
        Ok(())
    }
}

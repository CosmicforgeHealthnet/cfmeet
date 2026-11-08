//! Final closure: an organization scope may also bind a Messaging service account
//! (organization-scope Messaging through the Meeting relay). Additive: widens the service
//! check of `organization_scope_service_bindings`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE organization_scope_service_bindings DROP CONSTRAINT ck_organization_scope_service_bindings_service;
ALTER TABLE organization_scope_service_bindings ADD CONSTRAINT ck_organization_scope_service_bindings_service
    CHECK (service IN ('calling', 'messaging'));
"#;

const DOWN: &str = r#"
DELETE FROM organization_scope_service_bindings WHERE service = 'messaging';
ALTER TABLE organization_scope_service_bindings DROP CONSTRAINT ck_organization_scope_service_bindings_service;
ALTER TABLE organization_scope_service_bindings ADD CONSTRAINT ck_organization_scope_service_bindings_service
    CHECK (service IN ('calling'));
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

//! Gap audit G19 — organization lifecycle notifications.
//!
//! Members are notified when their organization is archived or reactivated
//! (`organizations.organization.archived` / `organizations.organization.reactivated`,
//! published through the existing `organization_notification_publications` relay, only
//! while `ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED=true`). Additive: the event-type
//! CHECK of the publication rows admits the two new catalogue types; every existing row
//! still satisfies it. The ids-only envelope CHECK is unchanged (`organizationId` only).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE organization_notification_publications
    DROP CONSTRAINT ck_organization_notification_publications_event_type,
    ADD CONSTRAINT ck_organization_notification_publications_event_type CHECK (event_type IN (
        'organizations.invitation.created', 'organizations.invitation.accepted',
        'organizations.invitation.declined', 'organizations.invitation.revoked',
        'organizations.invitation.expired', 'organizations.membership.role_changed',
        'organizations.membership.suspended', 'organizations.membership.removed',
        'organizations.ownership.transferred', 'organizations.organization.suspended',
        'organizations.organization.archived', 'organizations.organization.reactivated'));
"#;

const DOWN: &str = r#"
DELETE FROM organization_notification_publications
 WHERE event_type IN ('organizations.organization.archived', 'organizations.organization.reactivated');
ALTER TABLE organization_notification_publications
    DROP CONSTRAINT ck_organization_notification_publications_event_type,
    ADD CONSTRAINT ck_organization_notification_publications_event_type CHECK (event_type IN (
        'organizations.invitation.created', 'organizations.invitation.accepted',
        'organizations.invitation.declined', 'organizations.invitation.revoked',
        'organizations.invitation.expired', 'organizations.membership.role_changed',
        'organizations.membership.suspended', 'organizations.membership.removed',
        'organizations.ownership.transferred', 'organizations.organization.suspended'));
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

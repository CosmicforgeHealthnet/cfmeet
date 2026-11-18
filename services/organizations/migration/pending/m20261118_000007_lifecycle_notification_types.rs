//! PENDING — not registered in the migrator on purpose (rollback safety).
//!
//! The Organizations migration history refuses an older binary on a newer schema, so a
//! registered migration would make the previous image unable to start after a rollback.
//! G19 ships switched off (`ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED=false`), which
//! never writes these two types, so the schema change is not needed yet. Move this file
//! back to `migration/src/`, register it in `Migrator::migrations()`, and release it
//! together with the Notification Service catalogue types, before turning the switch on.
//! Until then Organizations refuses to start with the switch on (startup schema check).
//!
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

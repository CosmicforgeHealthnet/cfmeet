//! Notification Service roadmap N12 follow-up (owner decision A) — the account notices in
//! the Account CRITICAL lane: the welcome email, the new sign-in alert, the email-change
//! attempt notice and the two email-changed notices, which Meeting sent only through its
//! legacy ledger, become the CRITICAL fence category `ACCOUNT_NOTICES` (catalogue events
//! `account.welcome.requested`, `account.security.new-sign-in`,
//! `account.security.email-in-use`, `account.security.email-changed`).
//!
//! Fourth migration of the SEPARATE additive history `notification_fence_migrations`
//! (binary `notification-fence-migration`): previously deployed images never read the new
//! row, and their `routingProtocol` readiness fact fails closed on the version they do not
//! know (as for m20261118_000001). Additive; inert while `ACCOUNT_NOTICES` is
//! `LEGACY_ONLY` (the seeded mode): the legacy ledger rows stay byte for byte identical.
//!
//! * `notification_routing_state` gains the row `ACCOUNT_NOTICES` (`LEGACY_ONLY`).
//! * `notification_fence_email_category` (the `email_deliveries` guard trigger's mapping)
//!   maps `AUTH_WELCOME` and the `NOTIFICATION` keys `new-sign-in:`, `email-in-use:` and
//!   `email-changed:` to `ACCOUNT_NOTICES`; `device-linked:` keeps mapping to
//!   `SECURITY_DEVICE_LINKED`, every other mapping is unchanged. (No CHECK constraint of
//!   the fence lists the CRITICAL categories: `notification_routing_state.category` and
//!   `notification_route_decisions.category` are free-form, and the CHECK of
//!   `meeting_notification_publications.category` lists only the STANDARD ones.)
//!
//! `down` refuses while `ACCOUNT_NOTICES` is not `LEGACY_ONLY` or any route decision uses
//! it; it restores the previous mapping and removes the row.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
INSERT INTO notification_routing_state (category, mode, routing_version, changed_by_run) VALUES
    ('ACCOUNT_NOTICES', 'LEGACY_ONLY', 1, 'migration')
ON CONFLICT (category) DO NOTHING;

CREATE OR REPLACE FUNCTION notification_fence_email_category(p_category TEXT, p_key TEXT)
RETURNS TEXT AS $$
    SELECT CASE p_category
        WHEN 'AUTH_VERIFICATION'         THEN 'AUTH_VERIFICATION'
        WHEN 'AUTH_PASSWORD_RESET'       THEN 'AUTH_PASSWORD_RESET'
        WHEN 'SECURITY_DEVICE_STEP_UP'   THEN 'SECURITY_DEVICE_STEP_UP'
        WHEN 'SECURITY_PASSWORD_CHANGED' THEN 'SECURITY_PASSWORD_CHANGED'
        WHEN 'MEETING_INVITATION'        THEN 'MEETING_INVITATION'
        WHEN 'MEETING_NOTIFICATION'      THEN 'MEETING_NOTIFICATION'
        WHEN 'ORGANIZATION_INVITATION'   THEN 'ORGANIZATION_INVITATION'
        WHEN 'DEVELOPER_PLATFORM'        THEN 'DEVELOPER_PLATFORM'
        WHEN 'AUTH_WELCOME'              THEN 'ACCOUNT_NOTICES'
        WHEN 'NOTIFICATION' THEN
            CASE
                WHEN p_key LIKE 'device-linked:%' THEN 'SECURITY_DEVICE_LINKED'
                WHEN p_key LIKE 'new-sign-in:%'
                  OR p_key LIKE 'email-in-use:%'
                  OR p_key LIKE 'email-changed:%' THEN 'ACCOUNT_NOTICES'
            END
    END
$$ LANGUAGE sql IMMUTABLE;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notification_routing_state
                WHERE category = 'ACCOUNT_NOTICES' AND mode <> 'LEGACY_ONLY') THEN
        RAISE EXCEPTION 'refusing to remove ACCOUNT_NOTICES: the category is not LEGACY_ONLY';
    END IF;
    IF EXISTS (SELECT 1 FROM notification_route_decisions WHERE category = 'ACCOUNT_NOTICES') THEN
        RAISE EXCEPTION 'refusing to remove ACCOUNT_NOTICES: route decisions use it';
    END IF;
END
$$;
CREATE OR REPLACE FUNCTION notification_fence_email_category(p_category TEXT, p_key TEXT)
RETURNS TEXT AS $$
    SELECT CASE p_category
        WHEN 'AUTH_VERIFICATION'         THEN 'AUTH_VERIFICATION'
        WHEN 'AUTH_PASSWORD_RESET'       THEN 'AUTH_PASSWORD_RESET'
        WHEN 'SECURITY_DEVICE_STEP_UP'   THEN 'SECURITY_DEVICE_STEP_UP'
        WHEN 'SECURITY_PASSWORD_CHANGED' THEN 'SECURITY_PASSWORD_CHANGED'
        WHEN 'MEETING_INVITATION'        THEN 'MEETING_INVITATION'
        WHEN 'MEETING_NOTIFICATION'      THEN 'MEETING_NOTIFICATION'
        WHEN 'ORGANIZATION_INVITATION'   THEN 'ORGANIZATION_INVITATION'
        WHEN 'DEVELOPER_PLATFORM'        THEN 'DEVELOPER_PLATFORM'
        WHEN 'NOTIFICATION' THEN
            CASE WHEN p_key LIKE 'device-linked:%' THEN 'SECURITY_DEVICE_LINKED' END
    END
$$ LANGUAGE sql IMMUTABLE;
DELETE FROM notification_routing_state WHERE category = 'ACCOUNT_NOTICES';
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

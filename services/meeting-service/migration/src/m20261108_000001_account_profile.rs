//! Blueprint v1.0 Section 9 — personal account profile (ACC-021, DATA-016).
//!
//! Part of the SEPARATE additive history `profile_notifications_migrations`: the previously
//! deployed image checks the exact base, account-security and meetings-calendar histories,
//! so it keeps starting after this migration is applied (rollback safety).
//!
//! Only NULLABLE columns without defaults are added to `users` (a catalog-only change; the
//! previous image selects and inserts its own explicit column list and never sees them):
//!
//! * `display_name`, `avatar_url`, `timezone`, `locale` — the account profile. The Account
//!   (this service) is the authority; the Communications Hub `communication_profiles`
//!   display name / avatar reference is a projection refreshed from the account on every
//!   account-authenticated Hub request.
//! * `email_verified_at` — when the address was proven (verification code or Google).
//!   Backfilled only where the proof is recorded: the Section 2 verified Google subject
//!   binding (`account_security_state.google_linked_at`). Local accounts are not backfilled
//!   because `status = active` can also mean an administrator activated the account.
//! * `profile_updated_at` — last profile change (separate from `updated_at`, which the
//!   legacy code also moves on status and password changes).
//!
//! The Section 2 and Communications triggers fire only on `password_hash` / `status`
//! updates, so neither the backfill nor profile edits emit revocation events.
//! Constraints mirror the service-side validation so every writer stays within the
//! contract; they are added NOT VALID and then validated (the new columns are all NULL).
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE users
    ADD COLUMN IF NOT EXISTS display_name VARCHAR(64) NULL,
    ADD COLUMN IF NOT EXISTS avatar_url VARCHAR(512) NULL,
    ADD COLUMN IF NOT EXISTS timezone VARCHAR(64) NULL,
    ADD COLUMN IF NOT EXISTS locale VARCHAR(35) NULL,
    ADD COLUMN IF NOT EXISTS email_verified_at TIMESTAMPTZ NULL,
    ADD COLUMN IF NOT EXISTS profile_updated_at TIMESTAMPTZ NULL;

ALTER TABLE users ADD CONSTRAINT ck_users_display_name CHECK (
    display_name IS NULL OR (
        char_length(display_name) BETWEEN 1 AND 64
        AND display_name = btrim(display_name)
        AND display_name !~ '[[:cntrl:]]'
    )
) NOT VALID;
ALTER TABLE users ADD CONSTRAINT ck_users_avatar_url CHECK (
    avatar_url IS NULL OR (
        avatar_url ~ '^https://[^[:space:]@]+$' AND char_length(avatar_url) <= 512
    )
) NOT VALID;
ALTER TABLE users ADD CONSTRAINT ck_users_timezone CHECK (
    timezone IS NULL OR timezone ~ '^[A-Za-z0-9_+/-]{1,64}$'
) NOT VALID;
ALTER TABLE users ADD CONSTRAINT ck_users_locale CHECK (
    locale IS NULL OR locale ~ '^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8}){0,3}$'
) NOT VALID;
ALTER TABLE users VALIDATE CONSTRAINT ck_users_display_name;
ALTER TABLE users VALIDATE CONSTRAINT ck_users_avatar_url;
ALTER TABLE users VALIDATE CONSTRAINT ck_users_timezone;
ALTER TABLE users VALIDATE CONSTRAINT ck_users_locale;

DO $$
BEGIN
    IF to_regclass('public.account_security_state') IS NOT NULL THEN
        UPDATE users u
           SET email_verified_at = s.google_linked_at
          FROM account_security_state s
         WHERE s.user_id = u.id
           AND s.google_subject IS NOT NULL
           AND s.google_linked_at IS NOT NULL
           AND u.email_verified_at IS NULL;
    END IF;
END
$$;
"#;

const DOWN: &str = r#"
ALTER TABLE users
    DROP CONSTRAINT IF EXISTS ck_users_locale,
    DROP CONSTRAINT IF EXISTS ck_users_timezone,
    DROP CONSTRAINT IF EXISTS ck_users_avatar_url,
    DROP CONSTRAINT IF EXISTS ck_users_display_name,
    DROP COLUMN IF EXISTS profile_updated_at,
    DROP COLUMN IF EXISTS email_verified_at,
    DROP COLUMN IF EXISTS locale,
    DROP COLUMN IF EXISTS timezone,
    DROP COLUMN IF EXISTS avatar_url,
    DROP COLUMN IF EXISTS display_name;
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

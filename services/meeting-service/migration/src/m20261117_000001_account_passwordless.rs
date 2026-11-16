//! Passwordless sign-in (email one-time code / magic link) and secure email change.
//!
//! Appended to the `account_security_migrations` history. Additive only: the previously
//! deployed image keeps starting (its readiness check is "applied ⊇ known") and never
//! writes the new values or reads the new table. Nobody is signed out: existing sessions
//! and `users.password_hash` are left untouched (a bulk change of `password_hash` would
//! fire the credential triggers and revoke every session).
//!
//! * `account_sessions.auth_method` also accepts `EMAIL_OTP`.
//! * `email_change_reverts` — one row per completed self-service email change, carrying
//!   the single-use "undo" token (digest only) mailed to the previous address, valid for
//!   seven days. Also the source of the one-change-per-seven-days limit.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE account_sessions DROP CONSTRAINT IF EXISTS account_sessions_auth_method_check;
ALTER TABLE account_sessions DROP CONSTRAINT IF EXISTS ck_account_sessions_auth_method;
ALTER TABLE account_sessions ADD CONSTRAINT ck_account_sessions_auth_method CHECK (
    auth_method IN ('PASSWORD', 'EMAIL_OTP', 'GOOGLE', 'LEGACY_REFRESH')
);

CREATE TABLE email_change_reverts (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    old_email VARCHAR(255) NOT NULL,
    new_email VARCHAR(255) NOT NULL,
    token_digest CHAR(64) NOT NULL UNIQUE CHECK (token_digest ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    reverted_at TIMESTAMPTZ NULL,
    CONSTRAINT ck_email_change_reverts_window CHECK (expires_at > created_at)
);
CREATE INDEX idx_email_change_reverts_user ON email_change_reverts (user_id, created_at DESC);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS email_change_reverts;
ALTER TABLE account_sessions DROP CONSTRAINT IF EXISTS ck_account_sessions_auth_method;
UPDATE account_sessions SET auth_method = 'PASSWORD' WHERE auth_method = 'EMAIL_OTP';
ALTER TABLE account_sessions ADD CONSTRAINT account_sessions_auth_method_check CHECK (
    auth_method IN ('PASSWORD', 'GOOGLE', 'LEGACY_REFRESH')
);
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

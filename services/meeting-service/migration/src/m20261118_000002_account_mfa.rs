//! Gap audit A1: authenticator-app (TOTP) second factor and its single-use recovery codes.
//! The shared secret is stored sealed (`services::mfa`), recovery codes as SHA-256 only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS account_mfa_totp (
    user_id        UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    secret_sealed  TEXT NOT NULL CHECK (length(secret_sealed) BETWEEN 10 AND 200),
    confirmed_at   TIMESTAMPTZ,
    last_used_step BIGINT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS account_mfa_recovery_codes (
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash  CHAR(64) NOT NULL,
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, code_hash)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS account_mfa_recovery_codes;
DROP TABLE IF EXISTS account_mfa_totp;
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

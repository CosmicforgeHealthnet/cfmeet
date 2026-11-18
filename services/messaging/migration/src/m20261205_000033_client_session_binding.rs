//! Gap audit A11: a client session issued by a first-party relay for a VERIFIED account
//! session records that session's opaque reference (the same domain-separated SHA-256 as
//! `messaging_session_devices.account_session_sha256`), so ending ONE account session ends
//! only the client sessions it created, never those of the account's other devices.
//! Nullable: tokens issued without a binding (other applications, older relays) keep
//! ending with their device, their own revoke, or "revoke all".

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_client_sessions
    ADD COLUMN IF NOT EXISTS account_session_sha256 CHAR(64);
ALTER TABLE messaging_client_sessions
    ADD CONSTRAINT ck_messaging_client_sessions_binding
    CHECK (account_session_sha256 IS NULL OR account_session_sha256 ~ '^[0-9a-f]{64}$');
CREATE INDEX IF NOT EXISTS ix_messaging_client_sessions_binding
    ON messaging_client_sessions (environment_id, principal_id, account_session_sha256)
    WHERE status = 'ACTIVE' AND account_session_sha256 IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS ix_messaging_client_sessions_binding;
ALTER TABLE messaging_client_sessions DROP CONSTRAINT IF EXISTS ck_messaging_client_sessions_binding;
ALTER TABLE messaging_client_sessions DROP COLUMN IF EXISTS account_session_sha256;
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

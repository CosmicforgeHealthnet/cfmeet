//! Webhook signing-secret key rotation (Part H §74.12).
//!
//! * `webhook_subscriptions.secret_key_id` — the id of the deployment key the signing
//!   secret is sealed under (`v2.<kid>.<…>`). NULL marks a legacy `v1.` seal (made before
//!   key ids existed), attributed to the configured legacy key id. The seal format and
//!   the column must agree (CHECK), so `platform_admin webhooks key-status` / `reseal` and
//!   the startup key-coverage check can rely on the column alone. Never key material.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE webhook_subscriptions
    ADD COLUMN secret_key_id VARCHAR(32),
    DROP CONSTRAINT ck_webhook_subscriptions_secret,
    ADD CONSTRAINT ck_webhook_subscriptions_secret CHECK (
        (secret_key_id IS NULL AND secret_sealed ~ '^v1\.[A-Za-z0-9_-]+$')
        OR (secret_key_id IS NOT NULL
            AND secret_key_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$'
            AND secret_sealed ~ '^v2\.[A-Za-z0-9][A-Za-z0-9_-]{0,31}\.[A-Za-z0-9_-]+$'
            AND left(secret_sealed, length(secret_key_id) + 4) = 'v2.' || secret_key_id || '.'));
CREATE INDEX idx_webhook_subscriptions_secret_key ON webhook_subscriptions (secret_key_id, id);
"#;

// Rolling back after `v2.` seals were written leaves rows the old code cannot open; the
// original constraint is therefore restored NOT VALID (the schema rolls back cleanly and
// new rows are checked again) — re-seal or rotate the affected secrets before relying on it.
const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_webhook_subscriptions_secret_key;
ALTER TABLE webhook_subscriptions
    DROP CONSTRAINT ck_webhook_subscriptions_secret,
    DROP COLUMN IF EXISTS secret_key_id,
    ADD CONSTRAINT ck_webhook_subscriptions_secret CHECK (secret_sealed ~ '^v1\.[A-Za-z0-9_-]+$') NOT VALID;
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

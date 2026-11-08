//! Blueprint v1.0 Sections 2–3 hardening — legacy secrets at rest (PLT-002, SEC-006,
//! DATA-003).
//!
//! Part of the SEPARATE additive history `account_hardening_migrations`: the previously
//! deployed image checks the exact base, account-security, meetings-calendar and
//! profile/notifications histories, so it keeps starting after this migration is applied
//! (rollback safety).
//!
//! Only NULLABLE columns, indexes and a one-way backfill are added; no existing column
//! changes and the plaintext columns stay readable (expand step):
//!
//! * `api_keys.key_hash` — SHA-256 (hex) of `cfmeet-legacy-api-key:v1|<key>`. Keys are 64
//!   random alphanumerics (~380 bits), so an unkeyed hash is a sufficient verifier and the
//!   backfill needs no server secret. Unique when present.
//! * `api_keys.key_prefix` — the first 8 characters, a non-secret lookup and display hint.
//! * `webhooks.secret_sha256` — SHA-256 (hex) of `cfmeet-legacy-webhook-secret:v1|<secret>`:
//!   legacy webhooks are never delivered (DATA-007), so the secret is never needed again
//!   and only a fingerprint is kept for support.
//!
//! New rows written by this image store a non-secret placeholder in the plaintext column
//! (it starts with U+0001, which no HTTP header value can carry, so the previous image's
//! plaintext lookup can never match it). Existing rows keep their plaintext until the
//! documented contract step (`docs/operations/hardening-sections-2-3.md`, "Cleanup"),
//! which overwrites it once every running image reads the hash.
//!
//! `down` refuses once a placeholder row exists: such a key or webhook would become
//! unusable for the previous image.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE api_keys
    ADD COLUMN IF NOT EXISTS key_hash VARCHAR(64) NULL,
    ADD COLUMN IF NOT EXISTS key_prefix VARCHAR(16) NULL;
ALTER TABLE webhooks
    ADD COLUMN IF NOT EXISTS secret_sha256 VARCHAR(64) NULL;

UPDATE api_keys
   SET key_hash = encode(sha256(convert_to('cfmeet-legacy-api-key:v1|' || api_key, 'UTF8')), 'hex'),
       key_prefix = left(api_key, 8)
 WHERE key_hash IS NULL AND left(api_key, 1) <> chr(1);
UPDATE webhooks
   SET secret_sha256 = encode(sha256(convert_to('cfmeet-legacy-webhook-secret:v1|' || secret, 'UTF8')), 'hex')
 WHERE secret_sha256 IS NULL AND left(secret, 1) <> chr(1);

CREATE UNIQUE INDEX IF NOT EXISTS uq_api_keys_key_hash ON api_keys (key_hash) WHERE key_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_api_keys_key_prefix ON api_keys (key_prefix) WHERE key_prefix IS NOT NULL;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM api_keys WHERE left(api_key, 1) = chr(1))
       OR EXISTS (SELECT 1 FROM webhooks WHERE left(secret, 1) = chr(1)) THEN
        RAISE EXCEPTION 'refusing to downgrade: hashed-only legacy API keys or webhooks exist';
    END IF;
END
$$;
DROP INDEX IF EXISTS idx_api_keys_key_prefix;
DROP INDEX IF EXISTS uq_api_keys_key_hash;
ALTER TABLE webhooks DROP COLUMN IF EXISTS secret_sha256;
ALTER TABLE api_keys DROP COLUMN IF EXISTS key_prefix, DROP COLUMN IF EXISTS key_hash;
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

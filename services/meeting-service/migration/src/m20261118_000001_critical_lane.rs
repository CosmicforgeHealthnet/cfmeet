//! Notification Service roadmap N12 — the Account CRITICAL lane (blueprint Document 4 §9.2,
//! §9.8, §10.4; Document 5 §4 / §4.5 / §4.6): verification, reset and device step-up codes
//! and the password-changed / device-linked security notices.
//!
//! Third migration of the SEPARATE additive history `notification_fence_migrations`
//! (binary `notification-fence-migration`): previously deployed images never read these
//! columns and keep starting after it is applied (their `routingProtocol` readiness fact
//! fails closed on the version they do not know, which is what the activation operation
//! certifies). Additive; inert while every CRITICAL category is `LEGACY_ONLY`.
//!
//! `account_notification_outbox` (created by m20261116_000001 in the CRITICAL-lane shape:
//! one row per decision, the envelope with the code HPKE-sealed to the service key, and
//! `fallback_sealed`, the message's legacy email sealed to the Account fallback key) gains:
//!
//! * `shadow` — the SHADOW copy of an `OLD` decision (the legacy email is written as well;
//!   posted to I1 with `shadow: true`, recorded by the service and never delivered;
//!   Document 5 §4). Never failed over, never leased.
//! * `outcome`, `last_error` — the last I1 answer (machine codes only).
//! * `settled_at` — when the row stopped needing its secrets (acknowledged shadow copy,
//!   X7 completion, AUTO / FORCED / operator failover, expiry, dead letter). Settled rows
//!   carry neither the sealed part of the envelope nor `fallback_sealed` (both erased) and
//!   are purged after the retention period.
//! * state `DEAD` — I1 refused the event for good (shadow copy, or a decision that could
//!   not fail over because `critical_fallback = OFF`; alerted, never silent).
//!
//! `down` refuses while any category is not `LEGACY_ONLY` or any row uses what it adds.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE account_notification_outbox
    ADD COLUMN IF NOT EXISTS shadow     BOOLEAN     NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS outcome    VARCHAR(9),
    ADD COLUMN IF NOT EXISTS last_error VARCHAR(64),
    ADD COLUMN IF NOT EXISTS settled_at TIMESTAMPTZ;

ALTER TABLE account_notification_outbox
    DROP CONSTRAINT IF EXISTS account_notification_outbox_state_check,
    ADD CONSTRAINT account_notification_outbox_state_check
        CHECK (state IN ('PENDING', 'ACKED', 'SUPERSEDED', 'EXPIRED', 'DEAD')),
    DROP CONSTRAINT IF EXISTS ck_account_notification_outbox_outcome,
    ADD CONSTRAINT ck_account_notification_outbox_outcome
        CHECK (outcome IS NULL OR outcome IN ('ACCEPTED', 'DUPLICATE', 'HELD', 'REJECTED')),
    DROP CONSTRAINT IF EXISTS ck_account_notification_outbox_last_error,
    ADD CONSTRAINT ck_account_notification_outbox_last_error
        CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$');

-- The relay's AUTO / FORCED failover sweep and the expiry step: open, non-shadow rows.
CREATE INDEX IF NOT EXISTS ix_account_notification_outbox_open
    ON account_notification_outbox (created_at)
    WHERE settled_at IS NULL AND state IN ('PENDING', 'ACKED');
CREATE INDEX IF NOT EXISTS ix_account_notification_outbox_settled
    ON account_notification_outbox (settled_at) WHERE settled_at IS NOT NULL;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notification_routing_state WHERE mode <> 'LEGACY_ONLY') THEN
        RAISE EXCEPTION 'refusing to remove the CRITICAL lane: a category is not LEGACY_ONLY';
    END IF;
    IF EXISTS (SELECT 1 FROM account_notification_outbox WHERE shadow OR state = 'DEAD') THEN
        RAISE EXCEPTION 'refusing to remove the CRITICAL lane: outbox rows use its columns';
    END IF;
END
$$;
DROP INDEX IF EXISTS ix_account_notification_outbox_settled;
DROP INDEX IF EXISTS ix_account_notification_outbox_open;
ALTER TABLE account_notification_outbox
    DROP CONSTRAINT IF EXISTS ck_account_notification_outbox_last_error,
    DROP CONSTRAINT IF EXISTS ck_account_notification_outbox_outcome,
    DROP CONSTRAINT IF EXISTS account_notification_outbox_state_check,
    ADD CONSTRAINT account_notification_outbox_state_check
        CHECK (state IN ('PENDING', 'ACKED', 'SUPERSEDED', 'EXPIRED')),
    DROP COLUMN IF EXISTS settled_at,
    DROP COLUMN IF EXISTS last_error,
    DROP COLUMN IF EXISTS outcome,
    DROP COLUMN IF EXISTS shadow;
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

//! Blueprint v1.0 Section 2 — account security foundation (ACC-001/002/003/004/007/017,
//! SEC-013/DATA-015, DATA-002).
//!
//! A SEPARATE additive history (`account_security_migrations`): the previously deployed
//! image checks the exact base and Communications histories, so it keeps starting after
//! this migration is applied (rollback safety). No existing table or column changes:
//!
//! * `account_security_state` — the account revocation epoch, independent of the
//!   Communications integration, plus the verified Google subject binding.
//! * A trigger on `users` advances the epoch and revokes every open session whenever the
//!   password hash changes or the account becomes inactive or suspended.
//! * `account_sessions` — one row per sign-in; refresh rotation with reuse detection and
//!   an absolute lifetime.
//! * `account_security_events` — append-only security audit trail.
//! * A non-unique `LOWER(email)` index for case-insensitive lookups. A unique
//!   case-insensitive constraint needs a duplicate review first (see the runbook).
//!
//! No foreign keys point at `users` from the new tables, so the base history's own
//! `down` (CI migration smoke) is never blocked by them.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE account_security_state (
    user_id UUID PRIMARY KEY,
    credentials_not_before TIMESTAMPTZ NULL,
    google_subject VARCHAR(255) NULL,
    google_linked_at TIMESTAMPTZ NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT ck_account_security_google_subject CHECK (
        google_subject IS NULL OR (length(google_subject) BETWEEN 1 AND 255)
    )
);
CREATE UNIQUE INDEX uq_account_security_google_subject
    ON account_security_state (google_subject) WHERE google_subject IS NOT NULL;

CREATE TABLE account_sessions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    auth_method VARCHAR(16) NOT NULL
        CHECK (auth_method IN ('PASSWORD', 'GOOGLE', 'LEGACY_REFRESH')),
    authenticated_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_refreshed_at TIMESTAMPTZ NULL,
    absolute_expires_at TIMESTAMPTZ NOT NULL,
    refresh_generation INTEGER NOT NULL DEFAULT 0 CHECK (refresh_generation >= 0),
    current_refresh_id UUID NOT NULL,
    client_label VARCHAR(120) NULL,
    revoked_at TIMESTAMPTZ NULL,
    revoke_reason VARCHAR(32) NULL CHECK (revoke_reason IS NULL OR revoke_reason IN (
        'LOGOUT', 'LOGOUT_ALL', 'USER_REVOKED', 'REFRESH_REUSE', 'CREDENTIALS_CHANGED',
        'ACCOUNT_DISABLED', 'ADMIN_REVOKED', 'EXPIRED'
    )),
    CONSTRAINT ck_account_sessions_revocation CHECK ((revoked_at IS NULL) = (revoke_reason IS NULL)),
    CONSTRAINT ck_account_sessions_lifetime CHECK (absolute_expires_at > authenticated_at)
);
CREATE INDEX idx_account_sessions_user ON account_sessions (user_id, created_at DESC);
CREATE INDEX idx_account_sessions_open ON account_sessions (user_id) WHERE revoked_at IS NULL;

CREATE TABLE account_security_events (
    seq BIGSERIAL PRIMARY KEY,
    event_id UUID NOT NULL UNIQUE,
    user_id UUID NULL,
    session_id UUID NULL,
    event_type VARCHAR(48) NOT NULL CHECK (event_type ~ '^[A-Z][A-Z_]{2,47}$'),
    outcome VARCHAR(16) NOT NULL CHECK (outcome IN ('SUCCESS', 'FAILURE', 'DENIED')),
    actor_kind VARCHAR(16) NOT NULL CHECK (actor_kind IN ('USER', 'ADMIN', 'SYSTEM', 'ANONYMOUS')),
    actor_user_id UUID NULL,
    request_id VARCHAR(100) NULL,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(detail) = 'object' AND octet_length(detail::text) <= 2048),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_account_security_events_user ON account_security_events (user_id, seq DESC);
CREATE INDEX idx_account_security_events_type ON account_security_events (event_type, seq DESC);

CREATE FUNCTION account_security_events_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'account_security_events is append-only';
END;
$$;
CREATE TRIGGER trg_account_security_events_no_update
    BEFORE UPDATE OR DELETE ON account_security_events
    FOR EACH ROW EXECUTE FUNCTION account_security_events_append_only();
CREATE TRIGGER trg_account_security_events_no_truncate
    BEFORE TRUNCATE ON account_security_events
    FOR EACH STATEMENT EXECUTE FUNCTION account_security_events_append_only();

-- The revocation epoch advances in the same transaction as the credential change, for
-- every writer (handlers, admin tools, direct SQL), and every open session is closed.
CREATE FUNCTION account_security_credentials_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    reason VARCHAR(32);
BEGIN
    IF NEW.password_hash IS DISTINCT FROM OLD.password_hash THEN
        reason := 'CREDENTIALS_CHANGED';
    ELSIF NEW.status IS DISTINCT FROM OLD.status
          AND NEW.status::text IN ('inactive', 'suspended') THEN
        reason := 'ACCOUNT_DISABLED';
    ELSE
        RETURN NEW;
    END IF;
    INSERT INTO account_security_state (user_id, credentials_not_before, updated_at)
    VALUES (NEW.id, now(), now())
    ON CONFLICT (user_id) DO UPDATE
        SET credentials_not_before = now(), updated_at = now();
    UPDATE account_sessions
       SET revoked_at = now(), revoke_reason = reason
     WHERE user_id = NEW.id AND revoked_at IS NULL;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_account_security_credentials_changed
    AFTER UPDATE OF password_hash, status ON users
    FOR EACH ROW EXECUTE FUNCTION account_security_credentials_changed();

CREATE INDEX IF NOT EXISTS idx_users_email_lower ON users (LOWER(email));
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_users_email_lower;
DROP TRIGGER IF EXISTS trg_account_security_credentials_changed ON users;
DROP FUNCTION IF EXISTS account_security_credentials_changed();
DROP TABLE IF EXISTS account_security_events;
DROP FUNCTION IF EXISTS account_security_events_append_only();
DROP TABLE IF EXISTS account_sessions;
DROP TABLE IF EXISTS account_security_state;
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

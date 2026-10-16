//! Privacy Relay Sessions (Part D §30).
//!
//! A Relay Session is NOT a Call: it is the protected communication window derived from
//! one Communication Context, under a relay strategy, and it may authorize many Call
//! attempts over its lifetime (there is deliberately no session ↔ call uniqueness).
//!
//! * `privacy_relay_sessions`: own version and lifecycle (same graph and guards as
//!   contexts), the context version it was last derived from, its strategy (policy
//!   vocabulary, not proof a route exists), validity CONTAINED by the parent context
//!   (enforced by a trigger on every validity write), the inherited bounded policy, and
//!   an optional opaque server-owned `relay_identity_binding_ref` (no number, no carrier,
//!   no provider resource; nothing assigns it yet).
//! * `privacy_relay_session_participants` / `_channels` / `_directions`: the session's
//!   derived authorization snapshot and its history. Snapshot rows reference context
//!   participants of the SAME context and ownership (composite foreign keys), so a
//!   session can never name a subject outside its context; revocation is one-way.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE privacy_relay_sessions (
    id                            UUID PRIMARY KEY,
    context_id                    UUID        NOT NULL,
    tenant_id                     UUID        NOT NULL,
    application_id                UUID        NOT NULL,
    environment_id                UUID        NOT NULL,
    source_context_version        BIGINT      NOT NULL,
    version                       BIGINT      NOT NULL,
    status                        TEXT        NOT NULL,
    strategy                      TEXT        NOT NULL,
    direction_policy              TEXT        NOT NULL,
    participant_scope             TEXT        NOT NULL,
    channel_scope                 TEXT        NOT NULL,
    direction_scope               TEXT        NOT NULL,
    starts_at                     TIMESTAMPTZ NOT NULL,
    expires_at                    TIMESTAMPTZ NOT NULL,
    grace_expires_at              TIMESTAMPTZ,
    final_expires_at              TIMESTAMPTZ GENERATED ALWAYS AS (COALESCE(grace_expires_at, expires_at)) STORED,
    policy_version                VARCHAR(32) NOT NULL,
    policy_snapshot               JSONB       NOT NULL,
    relay_identity_binding_ref    UUID,
    created_by_service_account_id UUID        NOT NULL,
    created_at                    TIMESTAMPTZ NOT NULL,
    updated_at                    TIMESTAMPTZ NOT NULL,
    suspended_at                  TIMESTAMPTZ,
    revoked_at                    TIMESTAMPTZ,
    completed_at                  TIMESTAMPTZ,
    expired_at                    TIMESTAMPTZ,
    CONSTRAINT fk_relay_sessions_context
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id)
        REFERENCES communication_contexts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_relay_sessions_owner_id UNIQUE (tenant_id, application_id, environment_id, context_id, id),
    CONSTRAINT ck_relay_sessions_source_version CHECK (source_context_version >= 1),
    CONSTRAINT ck_relay_sessions_version CHECK (version >= 1),
    CONSTRAINT ck_relay_sessions_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'REVOKED', 'COMPLETED', 'EXPIRED')),
    CONSTRAINT ck_relay_sessions_strategy
        CHECK (strategy IN ('APP_ONLY', 'APP_PREFERRED', 'PUBLIC_RELAY_ALLOWED', 'CALLBACK_RELAY_ALLOWED')),
    CONSTRAINT ck_relay_sessions_direction_policy CHECK (direction_policy IN ('BIDIRECTIONAL', 'EXPLICIT')),
    CONSTRAINT ck_relay_sessions_scopes CHECK (
        participant_scope IN ('INHERITED', 'NARROWED') AND channel_scope IN ('INHERITED', 'NARROWED')
        AND direction_scope IN ('INHERITED', 'NARROWED')),
    CONSTRAINT ck_relay_sessions_validity CHECK (starts_at < expires_at),
    CONSTRAINT ck_relay_sessions_grace CHECK (grace_expires_at IS NULL OR grace_expires_at >= expires_at),
    CONSTRAINT ck_relay_sessions_policy_version CHECK (policy_version ~ '^[A-Z][A-Z0-9_]{1,31}$'),
    CONSTRAINT ck_relay_sessions_policy CHECK (privacy_relay_valid_context_policy(policy_snapshot)),
    CONSTRAINT ck_relay_sessions_suspended CHECK (status <> 'SUSPENDED' OR suspended_at IS NOT NULL),
    CONSTRAINT ck_relay_sessions_completed CHECK (status <> 'COMPLETED' OR completed_at IS NOT NULL),
    CONSTRAINT ck_relay_sessions_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_relay_sessions_expired CHECK ((status = 'EXPIRED') = (expired_at IS NOT NULL)),
    CONSTRAINT ck_relay_sessions_updated CHECK (updated_at >= created_at)
);
CREATE INDEX idx_relay_sessions_context ON privacy_relay_sessions (context_id);
CREATE INDEX idx_relay_sessions_scope_status
    ON privacy_relay_sessions (tenant_id, application_id, environment_id, status);
CREATE INDEX idx_relay_sessions_scope_created
    ON privacy_relay_sessions (tenant_id, application_id, environment_id, created_at DESC, id DESC);
CREATE INDEX idx_relay_sessions_final_expiry ON privacy_relay_sessions (final_expires_at)
    WHERE status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED');

-- A session never outlives or precedes its context's authority.
CREATE FUNCTION privacy_relay_session_contained() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    ctx communication_contexts%ROWTYPE;
BEGIN
    SELECT * INTO ctx FROM communication_contexts WHERE id = NEW.context_id;
    IF NEW.starts_at < ctx.starts_at
       OR NEW.expires_at > ctx.expires_at
       OR COALESCE(NEW.grace_expires_at, NEW.expires_at) > COALESCE(ctx.grace_expires_at, ctx.expires_at) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a relay session must be contained by its context validity'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_relay_sessions_contained_insert
    BEFORE INSERT ON privacy_relay_sessions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_session_contained();
CREATE TRIGGER trg_relay_sessions_contained_update
    BEFORE UPDATE OF starts_at, expires_at, grace_expires_at ON privacy_relay_sessions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_session_contained();
CREATE TRIGGER trg_relay_sessions_lifecycle
    BEFORE INSERT OR UPDATE ON privacy_relay_sessions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_versioned_lifecycle();
CREATE TRIGGER trg_relay_sessions_immutable
    BEFORE UPDATE ON privacy_relay_sessions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'context_id', 'tenant_id', 'application_id', 'environment_id', 'strategy',
        'created_by_service_account_id', 'created_at');
CREATE TRIGGER trg_relay_sessions_set_once
    BEFORE UPDATE ON privacy_relay_sessions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment(
        'relay_identity_binding_ref', 'revoked_at', 'completed_at', 'expired_at');
CREATE TRIGGER trg_relay_sessions_no_delete
    BEFORE DELETE ON privacy_relay_sessions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Session authorization snapshot: participants.
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_session_participants (
    id                         UUID PRIMARY KEY,
    session_id                 UUID        NOT NULL,
    context_id                 UUID        NOT NULL,
    tenant_id                  UUID        NOT NULL,
    application_id             UUID        NOT NULL,
    environment_id             UUID        NOT NULL,
    context_participant_id     UUID        NOT NULL,
    subject_kind               TEXT        NOT NULL,
    principal_ref              UUID,
    contact_ref                UUID,
    source_context_version     BIGINT      NOT NULL,
    status                     TEXT        NOT NULL,
    added_at                   TIMESTAMPTZ NOT NULL,
    revoked_at                 TIMESTAMPTZ,
    revoked_in_session_version BIGINT,
    CONSTRAINT fk_session_participants_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_session_participants_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, context_participant_id)
        REFERENCES communication_context_participants (tenant_id, application_id, environment_id, context_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT uq_session_participants_membership UNIQUE (session_id, context_participant_id),
    CONSTRAINT ck_session_participants_subject CHECK (
        (subject_kind = 'PRINCIPAL' AND principal_ref IS NOT NULL AND contact_ref IS NULL)
        OR (subject_kind = 'CONTACT_REFERENCE' AND contact_ref IS NOT NULL AND principal_ref IS NULL)),
    CONSTRAINT ck_session_participants_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_session_participants_revoked CHECK (
        (status = 'REVOKED') = (revoked_at IS NOT NULL)
        AND (revoked_at IS NULL) = (revoked_in_session_version IS NULL)),
    CONSTRAINT ck_session_participants_source CHECK (source_context_version >= 1)
);
CREATE INDEX idx_session_participants_active ON privacy_relay_session_participants (session_id)
    WHERE status = 'ACTIVE';
CREATE INDEX idx_session_participants_context_participant
    ON privacy_relay_session_participants (context_participant_id);
CREATE TRIGGER trg_session_participants_immutable
    BEFORE UPDATE ON privacy_relay_session_participants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'session_id', 'context_id', 'tenant_id', 'application_id', 'environment_id',
        'context_participant_id', 'subject_kind', 'principal_ref', 'contact_ref',
        'source_context_version', 'added_at');
CREATE TRIGGER trg_session_participants_set_once
    BEFORE UPDATE ON privacy_relay_session_participants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('revoked_at', 'revoked_in_session_version');
CREATE TRIGGER trg_session_participants_no_delete
    BEFORE DELETE ON privacy_relay_session_participants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Session authorization snapshot: channels (never broader than the context's).
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_session_channels (
    session_id                 UUID        NOT NULL,
    context_id                 UUID        NOT NULL,
    tenant_id                  UUID        NOT NULL,
    application_id             UUID        NOT NULL,
    environment_id             UUID        NOT NULL,
    channel                    TEXT        NOT NULL,
    enabled                    BOOLEAN     NOT NULL,
    changed_in_session_version BIGINT      NOT NULL,
    created_at                 TIMESTAMPTZ NOT NULL,
    updated_at                 TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_session_channels PRIMARY KEY (session_id, channel),
    CONSTRAINT fk_session_channels_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_session_channels_channel CHECK (channel IN ('VOICE', 'MESSAGING')),
    CONSTRAINT ck_session_channels_version CHECK (changed_in_session_version >= 1),
    CONSTRAINT ck_session_channels_updated CHECK (updated_at >= created_at)
);
CREATE TRIGGER trg_session_channels_immutable
    BEFORE UPDATE ON privacy_relay_session_channels
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'session_id', 'context_id', 'tenant_id', 'application_id', 'environment_id', 'channel', 'created_at');
CREATE TRIGGER trg_session_channels_no_delete
    BEFORE DELETE ON privacy_relay_session_channels
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Session authorization snapshot: explicit directions (never broader than the context's).
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_session_directions (
    id                  UUID PRIMARY KEY,
    session_id          UUID        NOT NULL,
    context_id          UUID        NOT NULL,
    tenant_id           UUID        NOT NULL,
    application_id      UUID        NOT NULL,
    environment_id      UUID        NOT NULL,
    from_participant_id UUID        NOT NULL,
    to_participant_id   UUID        NOT NULL,
    channel             TEXT,
    status              TEXT        NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL,
    revoked_at          TIMESTAMPTZ,
    CONSTRAINT fk_session_directions_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_session_directions_from
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, from_participant_id)
        REFERENCES communication_context_participants (tenant_id, application_id, environment_id, context_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_session_directions_to
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, to_participant_id)
        REFERENCES communication_context_participants (tenant_id, application_id, environment_id, context_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_session_directions_distinct CHECK (from_participant_id <> to_participant_id),
    CONSTRAINT ck_session_directions_channel CHECK (channel IS NULL OR channel IN ('VOICE', 'MESSAGING')),
    CONSTRAINT ck_session_directions_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_session_directions_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL))
);
CREATE UNIQUE INDEX uq_session_directions_active
    ON privacy_relay_session_directions (session_id, from_participant_id, to_participant_id, COALESCE(channel, '*'))
    WHERE status = 'ACTIVE';
CREATE INDEX idx_session_directions_edge
    ON privacy_relay_session_directions (session_id, from_participant_id, to_participant_id);
CREATE TRIGGER trg_session_directions_immutable
    BEFORE UPDATE ON privacy_relay_session_directions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'session_id', 'context_id', 'tenant_id', 'application_id', 'environment_id',
        'from_participant_id', 'to_participant_id', 'channel', 'created_at');
CREATE TRIGGER trg_session_directions_set_once
    BEFORE UPDATE ON privacy_relay_session_directions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('revoked_at');
CREATE TRIGGER trg_session_directions_no_delete
    BEFORE DELETE ON privacy_relay_session_directions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS privacy_relay_session_directions;
DROP TABLE IF EXISTS privacy_relay_session_channels;
DROP TABLE IF EXISTS privacy_relay_session_participants;
DROP TABLE IF EXISTS privacy_relay_sessions;
DROP FUNCTION IF EXISTS privacy_relay_session_contained();
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

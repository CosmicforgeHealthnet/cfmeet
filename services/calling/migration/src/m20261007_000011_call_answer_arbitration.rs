//! Single-winner answer arbitration and session media authority (Part C §20, §24).
//!
//! * `call_answer_claims` holds at most ONE row per (Call, logical participant)
//!   (`uq_call_answer_claims_participant`). Accepting an invitation inserts the claim in
//!   the accepting transaction; a concurrent second acceptance for the same participant
//!   cannot commit a second claim, whatever the application code does. Winners are per
//!   participant, never per Call, so every participant of a group Call has its own winner.
//! * An invitation can only become ACCEPTED if it is the claim's winning invitation
//!   (trigger), so "two accepted devices for one participant" is unrepresentable.
//! * `call_sessions` gains its media authority:
//!   - `endpoint_id` — the endpoint the session runs on (same principal as the
//!     participant, trigger-enforced; immutable);
//!   - `media_access` — NONE → AUTHORIZED → REVOKED (never back). A RECIPIENT session can
//!     only become AUTHORIZED if it (or the session it replaced) won its participant's
//!     claim; an ended session never holds media access;
//!   - `media_generation` — incremented on every media credential issuance, only while
//!     AUTHORIZED; provider identities carry it so superseded credentials are refused;
//!   - `media_admitted_at` — the provider confirmed the session's current connection and
//!     its server-authorized permissions were applied;
//!   - `media_enforcement_pending` — a provider-side enforcement (removal / permission
//!     change) is not yet confirmed and is retried by reconciliation;
//!   - `removed_at` — the session was removed by command; removal is final.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Session media authority.
-- ---------------------------------------------------------------------------
ALTER TABLE call_sessions
    ADD COLUMN endpoint_id               UUID,
    ADD COLUMN media_access              TEXT        NOT NULL DEFAULT 'NONE',
    ADD COLUMN media_generation          INTEGER     NOT NULL DEFAULT 0,
    ADD COLUMN media_admitted_at         TIMESTAMPTZ,
    ADD COLUMN media_enforcement_pending BOOLEAN     NOT NULL DEFAULT FALSE,
    ADD COLUMN removed_at                TIMESTAMPTZ,
    ADD CONSTRAINT fk_call_sessions_endpoint
        FOREIGN KEY (tenant_id, application_id, environment_id, endpoint_id)
        REFERENCES call_endpoints (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    ADD CONSTRAINT ck_call_sessions_media_access CHECK (media_access IN ('NONE', 'AUTHORIZED', 'REVOKED')),
    ADD CONSTRAINT ck_call_sessions_media_generation CHECK (media_generation >= 0),
    ADD CONSTRAINT ck_call_sessions_ended_media CHECK (state <> 'ENDED' OR media_access <> 'AUTHORIZED'),
    ADD CONSTRAINT ck_call_sessions_removed CHECK (
        removed_at IS NULL OR (state = 'ENDED' AND media_access = 'REVOKED' AND removed_at >= created_at)),
    ADD CONSTRAINT ck_call_sessions_admitted CHECK (
        media_admitted_at IS NULL OR (media_generation >= 1 AND media_admitted_at >= created_at));
CREATE INDEX idx_call_sessions_endpoint ON call_sessions (endpoint_id);
CREATE INDEX idx_call_sessions_enforcement_pending
    ON call_sessions (call_id) WHERE media_enforcement_pending;

DROP TRIGGER trg_call_sessions_immutable ON call_sessions;
CREATE TRIGGER trg_call_sessions_immutable
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'participant_id', 'tenant_id', 'application_id', 'environment_id',
        'media_identity', 'endpoint_category', 'replaces_session_id', 'created_at', 'endpoint_id');
DROP TRIGGER trg_call_sessions_set_once ON call_sessions;
CREATE TRIGGER trg_call_sessions_set_once
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'first_connected_at', 'ended_at', 'removed_at');
DROP TRIGGER trg_call_sessions_monotonic ON call_sessions;
CREATE TRIGGER trg_call_sessions_monotonic
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_decrease('generation', 'media_generation');
CREATE TRIGGER trg_call_sessions_media_access_transition
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('media_access',
        'NONE>AUTHORIZED', 'NONE>REVOKED', 'AUTHORIZED>REVOKED');

-- A session's endpoint belongs to the session's own participant's principal.
CREATE FUNCTION calling_call_sessions_guard_endpoint() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.endpoint_id IS NOT NULL AND NOT EXISTS (
        SELECT 1
          FROM call_endpoints e
          JOIN call_participants p ON p.id = NEW.participant_id
         WHERE e.id = NEW.endpoint_id AND e.principal_id = p.principal_id
    ) THEN
        RAISE EXCEPTION 'calling invariant violated: a session endpoint belongs to the participant''s principal'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.media_access <> 'NONE' OR NEW.media_generation <> 0 OR NEW.removed_at IS NOT NULL
       OR NEW.media_admitted_at IS NOT NULL THEN
        RAISE EXCEPTION 'calling invariant violated: sessions are created without media authority'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_sessions_guard_endpoint
    BEFORE INSERT ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION calling_call_sessions_guard_endpoint();

-- ---------------------------------------------------------------------------
-- Answer claim: the single winner of one logical participant's answer.
-- ---------------------------------------------------------------------------
CREATE TABLE call_answer_claims (
    id                   UUID PRIMARY KEY,
    call_id              UUID        NOT NULL,
    participant_id       UUID        NOT NULL,
    tenant_id            UUID        NOT NULL,
    application_id       UUID        NOT NULL,
    environment_id       UUID        NOT NULL,
    winner_invitation_id UUID        NOT NULL,
    winner_session_id    UUID        NOT NULL,
    claimed_at           TIMESTAMPTZ NOT NULL,
    created_at           TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_answer_claims_invitation
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id, winner_invitation_id)
        REFERENCES call_invitations (tenant_id, application_id, environment_id, call_id, participant_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_answer_claims_session
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id, winner_session_id)
        REFERENCES call_sessions (tenant_id, application_id, environment_id, call_id, participant_id, id)
        ON DELETE RESTRICT,
    -- THE single-winner guarantee: one winning answer per logical participant per Call.
    CONSTRAINT uq_call_answer_claims_participant UNIQUE (call_id, participant_id),
    CONSTRAINT uq_call_answer_claims_invitation UNIQUE (winner_invitation_id),
    CONSTRAINT uq_call_answer_claims_session UNIQUE (winner_session_id),
    CONSTRAINT ck_call_answer_claims_timestamps CHECK (created_at >= claimed_at)
);
CREATE TRIGGER trg_call_answer_claims_immutable
    BEFORE UPDATE OR DELETE ON call_answer_claims
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
CREATE TRIGGER trg_call_answer_claims_no_truncate
    BEFORE TRUNCATE ON call_answer_claims
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

-- Only the claim's winning invitation can be ACCEPTED.
CREATE FUNCTION calling_call_invitations_require_claim() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.status = 'ACCEPTED' AND OLD.status IS DISTINCT FROM 'ACCEPTED' AND NOT EXISTS (
        SELECT 1 FROM call_answer_claims c
         WHERE c.winner_invitation_id = NEW.id AND c.winner_session_id = NEW.session_id
    ) THEN
        RAISE EXCEPTION 'calling invariant violated: only the winning invitation of a participant is accepted'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_invitations_require_claim
    BEFORE UPDATE ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION calling_call_invitations_require_claim();

-- Media authority: a RECIPIENT session (or a replacement of one) is AUTHORIZED only if it
-- won its participant's answer; media credentials (generation increments) are issued only
-- to an AUTHORIZED session.
CREATE FUNCTION calling_call_sessions_guard_media() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    participant_role text;
BEGIN
    IF NEW.media_access = 'AUTHORIZED' AND OLD.media_access <> 'AUTHORIZED' THEN
        SELECT role INTO participant_role FROM call_participants WHERE id = NEW.participant_id;
        IF participant_role <> 'INITIATOR' AND NOT EXISTS (
            WITH RECURSIVE lineage(id, replaces) AS (
                SELECT s.id, s.replaces_session_id FROM call_sessions s WHERE s.id = NEW.id
                UNION ALL
                SELECT s.id, s.replaces_session_id
                  FROM call_sessions s JOIN lineage l ON s.id = l.replaces
            )
            SELECT 1 FROM call_answer_claims c JOIN lineage l ON c.winner_session_id = l.id
             WHERE c.participant_id = NEW.participant_id
        ) THEN
            RAISE EXCEPTION 'calling invariant violated: only a winning session gains media access'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END IF;
    IF NEW.media_generation > OLD.media_generation AND NEW.media_access <> 'AUTHORIZED' THEN
        RAISE EXCEPTION 'calling invariant violated: media credentials are issued only to authorized sessions'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_sessions_guard_media
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION calling_call_sessions_guard_media();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_call_sessions_guard_media ON call_sessions;
DROP FUNCTION IF EXISTS calling_call_sessions_guard_media();
DROP TRIGGER IF EXISTS trg_call_invitations_require_claim ON call_invitations;
DROP FUNCTION IF EXISTS calling_call_invitations_require_claim();
DROP TABLE IF EXISTS call_answer_claims;
DROP TRIGGER IF EXISTS trg_call_sessions_guard_endpoint ON call_sessions;
DROP FUNCTION IF EXISTS calling_call_sessions_guard_endpoint();
DROP TRIGGER IF EXISTS trg_call_sessions_media_access_transition ON call_sessions;
DROP TRIGGER IF EXISTS trg_call_sessions_monotonic ON call_sessions;
CREATE TRIGGER trg_call_sessions_monotonic
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_decrease('generation');
DROP TRIGGER IF EXISTS trg_call_sessions_set_once ON call_sessions;
CREATE TRIGGER trg_call_sessions_set_once
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('first_connected_at', 'ended_at');
DROP TRIGGER IF EXISTS trg_call_sessions_immutable ON call_sessions;
CREATE TRIGGER trg_call_sessions_immutable
    BEFORE UPDATE ON call_sessions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'participant_id', 'tenant_id', 'application_id', 'environment_id',
        'media_identity', 'endpoint_category', 'replaces_session_id', 'created_at');
DROP INDEX IF EXISTS idx_call_sessions_enforcement_pending;
DROP INDEX IF EXISTS idx_call_sessions_endpoint;
ALTER TABLE call_sessions
    DROP CONSTRAINT IF EXISTS ck_call_sessions_admitted,
    DROP CONSTRAINT IF EXISTS ck_call_sessions_removed,
    DROP CONSTRAINT IF EXISTS ck_call_sessions_ended_media,
    DROP CONSTRAINT IF EXISTS ck_call_sessions_media_generation,
    DROP CONSTRAINT IF EXISTS ck_call_sessions_media_access,
    DROP CONSTRAINT IF EXISTS fk_call_sessions_endpoint,
    DROP COLUMN IF EXISTS removed_at,
    DROP COLUMN IF EXISTS media_enforcement_pending,
    DROP COLUMN IF EXISTS media_admitted_at,
    DROP COLUMN IF EXISTS media_generation,
    DROP COLUMN IF EXISTS media_access,
    DROP COLUMN IF EXISTS endpoint_id;
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

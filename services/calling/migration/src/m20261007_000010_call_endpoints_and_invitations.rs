//! Application endpoints and call invitations (Part C §20).
//!
//! ```text
//! Principal ─┬─ CallEndpoint   (one registered application device / signaling destination)
//!            │
//! Call ─ CallParticipant ─┬─ CallInvitation (the ring offer of one Call to one endpoint)
//!                         └─ CallSession    (created only when an invitation wins)
//! ```
//!
//! * A **call endpoint** is a Calling-owned registry entry: one application / device
//!   destination of a principal, in the principal's own tenant / application /
//!   environment (composite foreign key). Its key is an opaque, bounded, non-secret
//!   application identifier, unique among the ACTIVE endpoints of one application
//!   environment. Presence (`presence_state`, `last_seen_at`) is a routing hint only;
//!   nothing in this schema derives authority from it. `push_target_ref` is an opaque
//!   reference for later push infrastructure (never a push token).
//! * A **call invitation** is the Call-specific ring offer to one endpoint of one logical
//!   participant. Its endpoint must belong to the participant's own principal (composite
//!   foreign keys through `principal_id`), it is created PENDING for a Call that is not
//!   ending or terminal, by an ACTIVE endpoint only, and it moves along a strict state
//!   machine whose terminal states (ACCEPTED, DECLINED, ANSWERED_ELSEWHERE, CANCELLED,
//!   EXPIRED) are final. `expires_at` is server time, after `issued_at` and bounded; an
//!   acceptance must happen strictly before it. At most one live (PENDING / RINGING)
//!   invitation exists per Call and endpoint.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- Composite target: (scope, call, participant, principal) proves which principal a
-- participant represents.
ALTER TABLE call_participants
    ADD CONSTRAINT uq_call_participants_owner_principal
        UNIQUE (tenant_id, application_id, environment_id, call_id, id, principal_id);

-- ---------------------------------------------------------------------------
-- Call endpoint: a registered application device / signaling destination.
-- ---------------------------------------------------------------------------
CREATE TABLE call_endpoints (
    id                              UUID PRIMARY KEY,
    tenant_id                       UUID         NOT NULL,
    application_id                  UUID         NOT NULL,
    environment_id                  UUID         NOT NULL,
    principal_id                    UUID         NOT NULL,
    endpoint_key                    VARCHAR(128) NOT NULL,
    endpoint_kind                   TEXT         NOT NULL,
    supports_audio                  BOOLEAN      NOT NULL,
    supports_video                  BOOLEAN      NOT NULL,
    status                          TEXT         NOT NULL,
    presence_state                  TEXT         NOT NULL,
    last_seen_at                    TIMESTAMPTZ,
    -- Opaque reference a later push service can bind to (never a push token).
    push_target_ref                 UUID,
    registered_by_service_account_id UUID        NOT NULL,
    registered_by_credential_id     UUID         NOT NULL,
    created_at                      TIMESTAMPTZ  NOT NULL,
    updated_at                      TIMESTAMPTZ  NOT NULL,
    revoked_at                      TIMESTAMPTZ,
    CONSTRAINT fk_call_endpoints_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_endpoints_registered_by
        FOREIGN KEY (tenant_id, application_id, environment_id, registered_by_service_account_id, registered_by_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT uq_call_endpoints_owner_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_call_endpoints_owner_principal_id
        UNIQUE (tenant_id, application_id, environment_id, principal_id, id),
    -- Opaque, bounded, non-secret application identifier; never telephone-number shaped.
    CONSTRAINT ck_call_endpoints_key CHECK (
        endpoint_key ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'
        AND endpoint_key !~ '^[0-9]{7,15}$'
        AND endpoint_key !~* '^(tel|sip|sips):'),
    CONSTRAINT ck_call_endpoints_kind CHECK (endpoint_kind IN ('WEB', 'MOBILE', 'DESKTOP', 'ENTERPRISE_APP')),
    CONSTRAINT ck_call_endpoints_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_call_endpoints_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_call_endpoints_presence CHECK (presence_state IN ('ONLINE', 'OFFLINE')),
    CONSTRAINT ck_call_endpoints_media CHECK (supports_audio OR supports_video),
    CONSTRAINT ck_call_endpoints_timestamps CHECK (
        updated_at >= created_at
        AND (revoked_at IS NULL OR revoked_at >= created_at)
        AND (last_seen_at IS NULL OR last_seen_at >= created_at))
);
-- An endpoint key identifies one active endpoint per application environment.
CREATE UNIQUE INDEX uq_call_endpoints_active_key
    ON call_endpoints (tenant_id, application_id, environment_id, endpoint_key) WHERE status = 'ACTIVE';
CREATE INDEX idx_call_endpoints_principal_active ON call_endpoints (principal_id) WHERE status = 'ACTIVE';
CREATE TRIGGER trg_call_endpoints_immutable
    BEFORE UPDATE ON call_endpoints
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'principal_id', 'endpoint_key',
        'endpoint_kind', 'registered_by_service_account_id', 'registered_by_credential_id', 'created_at');
CREATE TRIGGER trg_call_endpoints_set_once
    BEFORE UPDATE ON call_endpoints
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('revoked_at');
CREATE TRIGGER trg_call_endpoints_status_transition
    BEFORE UPDATE ON call_endpoints
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status', 'ACTIVE>REVOKED');
CREATE TRIGGER trg_call_endpoints_no_delete
    BEFORE DELETE ON call_endpoints
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Call invitation: the ring offer of one Call to one endpoint of one participant.
-- ---------------------------------------------------------------------------

-- Invitations are created PENDING, for a Call that is neither ending nor terminal, by an
-- active endpoint, for a participant that is still part of the Call.
CREATE FUNCTION calling_call_invitations_guard_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    call_state      text;
    endpoint_status text;
    participant_end timestamptz;
BEGIN
    IF NEW.status <> 'PENDING'
       OR num_nonnulls(NEW.ringing_at, NEW.accepted_at, NEW.declined_at, NEW.cancelled_at,
                       NEW.finished_at, NEW.terminal_reason, NEW.session_id) > 0 THEN
        RAISE EXCEPTION 'calling invariant violated: invitations are created PENDING'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    SELECT current_state INTO call_state FROM calls WHERE id = NEW.call_id;
    IF call_state IS NULL
       OR call_state IN ('ENDING', 'ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED') THEN
        RAISE EXCEPTION 'calling invariant violated: an ending or terminal call cannot receive a new invitation'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    SELECT status INTO endpoint_status FROM call_endpoints WHERE id = NEW.endpoint_id;
    IF endpoint_status IS DISTINCT FROM 'ACTIVE' THEN
        RAISE EXCEPTION 'calling invariant violated: only an active endpoint can be invited'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    SELECT ended_at INTO participant_end FROM call_participants WHERE id = NEW.participant_id;
    IF participant_end IS NOT NULL THEN
        RAISE EXCEPTION 'calling invariant violated: a participant that left the call cannot be invited'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE call_invitations (
    id                    UUID PRIMARY KEY,
    call_id               UUID        NOT NULL,
    participant_id        UUID        NOT NULL,
    principal_id          UUID        NOT NULL,
    endpoint_id           UUID        NOT NULL,
    tenant_id             UUID        NOT NULL,
    application_id        UUID        NOT NULL,
    environment_id        UUID        NOT NULL,
    -- The session created for this invitation when (and only when) it wins.
    session_id            UUID,
    -- The transfer this invitation rings for (foreign key added with call_transfers).
    transfer_id           UUID,
    purpose               TEXT        NOT NULL,
    status                TEXT        NOT NULL,
    invitation_generation INTEGER     NOT NULL,
    issued_at             TIMESTAMPTZ NOT NULL,
    expires_at            TIMESTAMPTZ NOT NULL,
    ringing_at            TIMESTAMPTZ,
    accepted_at           TIMESTAMPTZ,
    declined_at           TIMESTAMPTZ,
    cancelled_at          TIMESTAMPTZ,
    finished_at           TIMESTAMPTZ,
    terminal_reason       TEXT,
    created_at            TIMESTAMPTZ NOT NULL,
    updated_at            TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_invitations_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    -- The participant of this Call that represents `principal_id` …
    CONSTRAINT fk_call_invitations_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id, principal_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id, principal_id)
        ON DELETE RESTRICT,
    -- … and an endpoint of that same principal, in the same scope.
    CONSTRAINT fk_call_invitations_endpoint
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id, endpoint_id)
        REFERENCES call_endpoints (tenant_id, application_id, environment_id, principal_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_invitations_session
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id, session_id)
        REFERENCES call_sessions (tenant_id, application_id, environment_id, call_id, participant_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT uq_call_invitations_owner_participant_id
        UNIQUE (tenant_id, application_id, environment_id, call_id, participant_id, id),
    CONSTRAINT uq_call_invitations_generation UNIQUE (call_id, endpoint_id, invitation_generation),
    CONSTRAINT uq_call_invitations_session UNIQUE (session_id),
    CONSTRAINT ck_call_invitations_purpose CHECK (purpose IN ('CALL', 'TRANSFER')),
    CONSTRAINT ck_call_invitations_transfer CHECK ((purpose = 'TRANSFER') = (transfer_id IS NOT NULL)),
    CONSTRAINT ck_call_invitations_status CHECK (status IN (
        'PENDING', 'RINGING', 'ACCEPTED', 'DECLINED', 'ANSWERED_ELSEWHERE', 'CANCELLED', 'EXPIRED')),
    CONSTRAINT ck_call_invitations_generation CHECK (invitation_generation >= 1),
    -- Server-authoritative, bounded ring window.
    CONSTRAINT ck_call_invitations_expiry
        CHECK (expires_at > issued_at AND expires_at <= issued_at + interval '10 minutes'),
    CONSTRAINT ck_call_invitations_ringing CHECK (
        (status <> 'PENDING' OR ringing_at IS NULL)
        AND (status NOT IN ('RINGING', 'ACCEPTED', 'DECLINED') OR ringing_at IS NOT NULL)),
    CONSTRAINT ck_call_invitations_accepted CHECK (
        (status = 'ACCEPTED') = (accepted_at IS NOT NULL)
        AND (status = 'ACCEPTED') = (session_id IS NOT NULL)
        AND (accepted_at IS NULL OR accepted_at < expires_at)),
    CONSTRAINT ck_call_invitations_declined CHECK ((status = 'DECLINED') = (declined_at IS NOT NULL)),
    CONSTRAINT ck_call_invitations_cancelled CHECK ((status = 'CANCELLED') = (cancelled_at IS NOT NULL)),
    CONSTRAINT ck_call_invitations_finished CHECK (
        (status IN ('ACCEPTED', 'DECLINED', 'ANSWERED_ELSEWHERE', 'CANCELLED', 'EXPIRED')) = (finished_at IS NOT NULL)
        AND (finished_at IS NULL) = (terminal_reason IS NULL)),
    CONSTRAINT ck_call_invitations_reason CHECK (
        terminal_reason IS NULL
        OR (status = 'ACCEPTED' AND terminal_reason = 'ACCEPTED')
        OR (status = 'DECLINED' AND terminal_reason = 'DECLINED')
        OR (status = 'ANSWERED_ELSEWHERE' AND terminal_reason = 'ANSWERED_ELSEWHERE')
        OR (status = 'EXPIRED' AND terminal_reason = 'EXPIRED')
        OR (status = 'CANCELLED' AND terminal_reason IN (
            'CALL_CANCELLED', 'CALL_TERMINATED', 'ENDPOINT_REVOKED', 'PARTICIPANT_REMOVED', 'TRANSFER_ENDED'))),
    CONSTRAINT ck_call_invitations_timestamps CHECK (
        (ringing_at IS NULL OR ringing_at >= issued_at)
        AND (finished_at IS NULL OR finished_at >= issued_at)
        AND (accepted_at IS NULL OR accepted_at >= issued_at)
        AND (declined_at IS NULL OR declined_at >= issued_at)
        AND (cancelled_at IS NULL OR cancelled_at >= issued_at)
        AND updated_at >= created_at)
);
-- At most one live ring offer per Call and endpoint.
CREATE UNIQUE INDEX uq_call_invitations_one_live_per_endpoint
    ON call_invitations (call_id, endpoint_id) WHERE status IN ('PENDING', 'RINGING');
CREATE INDEX idx_call_invitations_call ON call_invitations (call_id);
CREATE INDEX idx_call_invitations_participant ON call_invitations (participant_id);
CREATE INDEX idx_call_invitations_endpoint_live
    ON call_invitations (endpoint_id) WHERE status IN ('PENDING', 'RINGING');
-- Expiry worker: only live invitations, ordered by expiry.
CREATE INDEX idx_call_invitations_live_expiry
    ON call_invitations (expires_at) WHERE status IN ('PENDING', 'RINGING');
CREATE TRIGGER trg_call_invitations_guard_insert
    BEFORE INSERT ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION calling_call_invitations_guard_insert();
CREATE TRIGGER trg_call_invitations_immutable
    BEFORE UPDATE ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'participant_id', 'principal_id', 'endpoint_id', 'tenant_id',
        'application_id', 'environment_id', 'transfer_id', 'purpose', 'invitation_generation',
        'issued_at', 'expires_at', 'created_at');
CREATE TRIGGER trg_call_invitations_set_once
    BEFORE UPDATE ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'session_id', 'ringing_at', 'accepted_at', 'declined_at', 'cancelled_at', 'finished_at',
        'terminal_reason');
-- Terminal invitation states are final: an accepted invitation can never become declined,
-- an answered-elsewhere or expired one can never become accepted.
CREATE TRIGGER trg_call_invitations_status_transition
    BEFORE UPDATE ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'PENDING>RINGING', 'PENDING>ANSWERED_ELSEWHERE', 'PENDING>CANCELLED', 'PENDING>EXPIRED',
        'RINGING>ACCEPTED', 'RINGING>DECLINED', 'RINGING>ANSWERED_ELSEWHERE', 'RINGING>CANCELLED',
        'RINGING>EXPIRED');
CREATE TRIGGER trg_call_invitations_no_delete
    BEFORE DELETE ON call_invitations
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_invitations;
DROP FUNCTION IF EXISTS calling_call_invitations_guard_insert();
DROP TABLE IF EXISTS call_endpoints;
ALTER TABLE call_participants DROP CONSTRAINT IF EXISTS uq_call_participants_owner_principal;
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

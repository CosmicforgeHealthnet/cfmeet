//! Phase 2 — additive meeting-domain schema (spec v1.1, Part B).
//!
//! Adds the permanent Phase 2 aggregates next to the legacy `meetings` /
//! `participants` tables. Nothing legacy is altered or dropped.
//!
//! Enumerated columns are `TEXT` + `CHECK` constraints (not Postgres enum types)
//! so later phases can extend value sets additively.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- meeting_spaces: persistent meeting definition (NOT a live call)
-- ---------------------------------------------------------------------------
CREATE TABLE meeting_spaces (
    id                    UUID PRIMARY KEY,
    legacy_meeting_id     UUID UNIQUE REFERENCES meetings(id) ON DELETE SET NULL,
    organization_id       UUID,
    owner_user_id         UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    meeting_code          VARCHAR(32) NOT NULL UNIQUE,
    title                 VARCHAR(255) NOT NULL,
    description           TEXT,
    meeting_type          TEXT NOT NULL DEFAULT 'STANDARD'
        CHECK (meeting_type IN ('STANDARD','HEALTHCARE','WEBINAR','TOWN_HALL','TRAINING')),
    status                TEXT NOT NULL DEFAULT 'ACTIVE'
        CHECK (status IN ('ACTIVE','ARCHIVED','DELETED')),
    timezone              VARCHAR(64) NOT NULL DEFAULT 'UTC',
    access_policy         TEXT NOT NULL DEFAULT 'AUTHENTICATED'
        CHECK (access_policy IN ('PUBLIC','PUBLIC_PASSCODE','AUTHENTICATED','INVITE_ONLY',
                                 'ORGANIZATION_ONLY','DOMAIN_RESTRICTED','HEALTHCARE_APPOINTMENT','CUSTOM')),
    waiting_room_policy   TEXT NOT NULL DEFAULT 'OFF'
        CHECK (waiting_room_policy IN ('OFF','EVERYONE','GUESTS_ONLY','UNAUTHENTICATED_ONLY',
                                       'NON_MEMBERS_ONLY','EXTERNAL_ONLY','CUSTOM')),
    security_profile      TEXT NOT NULL DEFAULT 'STANDARD'
        CHECK (security_profile IN ('STANDARD','BUSINESS','ENTERPRISE','HEALTHCARE','HIGH_SECURITY')),
    default_role          TEXT NOT NULL DEFAULT 'PARTICIPANT'
        CHECK (default_role IN ('PRESENTER','PARTICIPANT','VIEWER')),
    passcode_hash         TEXT,
    interactive_capacity  INTEGER NOT NULL DEFAULT 100 CHECK (interactive_capacity > 0),
    viewer_capacity       INTEGER NOT NULL DEFAULT 0 CHECK (viewer_capacity >= 0),
    recording_policy      TEXT NOT NULL DEFAULT 'HOST_OPTIONAL'
        CHECK (recording_policy IN ('DISABLED','HOST_OPTIONAL','AUTO_START','ORGANIZATION_CONTROLLED')),
    transcription_policy  TEXT NOT NULL DEFAULT 'DISABLED'
        CHECK (transcription_policy IN ('DISABLED','HOST_OPTIONAL','AUTO_START','ORGANIZATION_CONTROLLED')),
    retention_policy_ref  VARCHAR(128),
    access_config         JSONB NOT NULL DEFAULT '{}'::jsonb,
    role_policy           JSONB NOT NULL DEFAULT '{}'::jsonb,
    schedule_policy       JSONB NOT NULL DEFAULT '{}'::jsonb,
    origin                TEXT NOT NULL DEFAULT 'V2' CHECK (origin IN ('V2','LEGACY')),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    archived_at           TIMESTAMPTZ,
    version               INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    CHECK (status <> 'ARCHIVED' OR archived_at IS NOT NULL),
    CHECK (access_policy <> 'PUBLIC_PASSCODE' OR passcode_hash IS NOT NULL)
);
CREATE INDEX idx_meeting_spaces_org ON meeting_spaces (organization_id) WHERE organization_id IS NOT NULL;
CREATE INDEX idx_meeting_spaces_owner ON meeting_spaces (owner_user_id, status);

-- ---------------------------------------------------------------------------
-- meeting_recurrence_rules: RFC 5545 RRULE interpreted in the space timezone
-- ---------------------------------------------------------------------------
CREATE TABLE meeting_recurrence_rules (
    id                  UUID PRIMARY KEY,
    meeting_space_id    UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    rrule               TEXT NOT NULL,
    timezone            VARCHAR(64) NOT NULL,
    dtstart_local       TIMESTAMP NOT NULL,
    duration_minutes    INTEGER NOT NULL CHECK (duration_minutes > 0 AND duration_minutes <= 1440),
    status              TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','ENDED')),
    materialized_until  TIMESTAMPTZ,
    created_by          UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at            TIMESTAMPTZ,
    version             INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1)
);
CREATE UNIQUE INDEX uq_recurrence_one_active_per_space
    ON meeting_recurrence_rules (meeting_space_id) WHERE status = 'ACTIVE';
CREATE INDEX idx_recurrence_materialization
    ON meeting_recurrence_rules (status, materialized_until);

-- ---------------------------------------------------------------------------
-- conference_sessions: one live/scheduled occurrence of a meeting space
-- ---------------------------------------------------------------------------
CREATE TABLE conference_sessions (
    id                    UUID PRIMARY KEY,
    meeting_space_id      UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    occurrence_key        VARCHAR(128) NOT NULL,
    kind                  TEXT NOT NULL CHECK (kind IN ('INSTANT','SCHEDULED','RECURRING','LEGACY')),
    origin                TEXT NOT NULL DEFAULT 'V2' CHECK (origin IN ('V2','LEGACY')),
    legacy_meeting_id     UUID UNIQUE REFERENCES meetings(id) ON DELETE SET NULL,
    recurrence_rule_id    UUID REFERENCES meeting_recurrence_rules(id) ON DELETE SET NULL,
    is_exception          BOOLEAN NOT NULL DEFAULT false,
    original_start_at     TIMESTAMPTZ,
    status                TEXT NOT NULL DEFAULT 'CREATED'
        CHECK (status IN ('CREATED','PREPARING','WAITING','LIVE','ENDING','ENDED','CANCELLED','FAILED')),
    scheduled_start_at    TIMESTAMPTZ,
    scheduled_end_at      TIMESTAMPTZ,
    started_at            TIMESTAMPTZ,
    started_by            VARCHAR(128),
    ending_requested_at   TIMESTAMPTZ,
    ended_at              TIMESTAMPTZ,
    ended_by              VARCHAR(128),
    cancelled_at          TIMESTAMPTZ,
    cancelled_by          VARCHAR(128),
    termination_reason    TEXT CHECK (termination_reason IN
        ('HOST_ENDED','SCHEDULED_END','ADMIN','FAILURE','EMPTY_TIMEOUT','CANCELLED','LEGACY')),
    failure_reason        VARCHAR(64),
    is_locked             BOOLEAN NOT NULL DEFAULT false,
    locked_at             TIMESTAMPTZ,
    locked_by             VARCHAR(128),
    rtc_provider          TEXT NOT NULL DEFAULT 'LIVEKIT',
    rtc_room_name         VARCHAR(128) UNIQUE,
    rtc_room_id           VARCHAR(128),
    region                VARCHAR(64),
    recording_state       TEXT NOT NULL DEFAULT 'OFF',
    transcription_state   TEXT NOT NULL DEFAULT 'OFF',
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    version               INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    CONSTRAINT uq_conference_occurrence UNIQUE (meeting_space_id, occurrence_key),
    CHECK (scheduled_end_at IS NULL OR scheduled_start_at IS NULL OR scheduled_end_at > scheduled_start_at),
    CHECK (is_locked = false OR locked_at IS NOT NULL),
    CHECK (status <> 'LIVE' OR started_at IS NOT NULL OR origin = 'LEGACY'),
    CHECK (status <> 'FAILED' OR failure_reason IS NOT NULL)
);
-- Exactly one LIVE v2 conference authority per meeting space.
CREATE UNIQUE INDEX uq_conference_one_live_per_space
    ON conference_sessions (meeting_space_id) WHERE status = 'LIVE' AND origin = 'V2';
CREATE INDEX idx_conference_space_start ON conference_sessions (meeting_space_id, scheduled_start_at);
CREATE INDEX idx_conference_status ON conference_sessions (status);
CREATE INDEX idx_conference_scheduler_end
    ON conference_sessions (scheduled_end_at) WHERE status IN ('LIVE','CREATED','PREPARING','WAITING');
CREATE INDEX idx_conference_scheduler_start
    ON conference_sessions (scheduled_start_at) WHERE status = 'CREATED';
CREATE INDEX idx_conference_recurrence ON conference_sessions (recurrence_rule_id) WHERE recurrence_rule_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- meeting_members: persistent membership (NOT a conference participant)
-- ---------------------------------------------------------------------------
CREATE TABLE meeting_members (
    id                 UUID PRIMARY KEY,
    meeting_space_id   UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    principal_type     TEXT NOT NULL CHECK (principal_type IN ('USER','EMAIL')),
    principal_ref      VARCHAR(320) NOT NULL,
    user_id            UUID REFERENCES users(id) ON DELETE CASCADE,
    role               TEXT NOT NULL
        CHECK (role IN ('HOST','CO_HOST','MODERATOR','PRESENTER','PARTICIPANT','VIEWER')),
    status             TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','REVOKED')),
    created_by         UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at         TIMESTAMPTZ,
    revoked_by         UUID REFERENCES users(id) ON DELETE SET NULL,
    version            INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    CHECK ((principal_type = 'USER') = (user_id IS NOT NULL)),
    CHECK (principal_type <> 'USER' OR principal_ref = user_id::text),
    CHECK (status <> 'REVOKED' OR revoked_at IS NOT NULL)
);
CREATE UNIQUE INDEX uq_meeting_member_active
    ON meeting_members (meeting_space_id, principal_type, principal_ref) WHERE status = 'ACTIVE';
CREATE INDEX idx_meeting_members_user ON meeting_members (user_id) WHERE status = 'ACTIVE';

-- ---------------------------------------------------------------------------
-- meeting_invitations: hash-only bearer invitations
-- ---------------------------------------------------------------------------
CREATE TABLE meeting_invitations (
    id                 UUID PRIMARY KEY,
    meeting_space_id   UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    conference_id      UUID REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    inviter_user_id    UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    invitee_email      VARCHAR(320),
    requested_role     TEXT NOT NULL DEFAULT 'PARTICIPANT'
        CHECK (requested_role IN ('HOST','CO_HOST','MODERATOR','PRESENTER','PARTICIPANT','VIEWER')),
    token_hash         VARCHAR(64) NOT NULL UNIQUE,
    expires_at         TIMESTAMPTZ NOT NULL,
    use_limit          INTEGER CHECK (use_limit IS NULL OR use_limit > 0),
    usage_count        INTEGER NOT NULL DEFAULT 0 CHECK (usage_count >= 0),
    status             TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','REVOKED')),
    revoked_at         TIMESTAMPTZ,
    revoked_by         UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (use_limit IS NULL OR usage_count <= use_limit),
    CHECK (status <> 'REVOKED' OR revoked_at IS NOT NULL),
    CHECK (expires_at > created_at)
);
CREATE INDEX idx_invitations_space ON meeting_invitations (meeting_space_id, status);

-- ---------------------------------------------------------------------------
-- conference_participants: ONE logical person/entity per conference
-- ---------------------------------------------------------------------------
CREATE TABLE conference_participants (
    id                     UUID PRIMARY KEY,
    conference_id          UUID NOT NULL REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    identity_type          TEXT NOT NULL
        CHECK (identity_type IN ('REGISTERED_USER','INVITED_EXTERNAL','GUEST','PHONE','SERVICE','BOT')),
    user_id                UUID REFERENCES users(id) ON DELETE RESTRICT,
    external_ref           VARCHAR(320),
    guest_identity         VARCHAR(64),
    credential_hash        VARCHAR(64) UNIQUE,
    invitation_id          UUID REFERENCES meeting_invitations(id) ON DELETE SET NULL,
    legacy_participant_id  UUID UNIQUE,
    display_name           VARCHAR(100) NOT NULL,
    role                   TEXT NOT NULL
        CHECK (role IN ('OWNER','HOST','CO_HOST','MODERATOR','PRESENTER','PARTICIPANT','VIEWER')),
    role_source            TEXT NOT NULL
        CHECK (role_source IN ('OWNER','MEMBERSHIP','INVITATION','DEFAULT','ASSIGNED','LEGACY')),
    admission_state        TEXT NOT NULL
        CHECK (admission_state IN ('WAITING','ADMITTED','DENIED','REMOVED','BANNED')),
    requested_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    admitted_at            TIMESTAMPTZ,
    admitted_by            VARCHAR(128),
    denied_at              TIMESTAMPTZ,
    denied_by              VARCHAR(128),
    denial_reason_code     VARCHAR(64),
    denial_message         VARCHAR(500),
    denial_internal_note   TEXT,
    removed_at             TIMESTAMPTZ,
    removed_by             VARCHAR(128),
    banned_at              TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    version                INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    CONSTRAINT uq_participant_scope UNIQUE (id, conference_id),
    CHECK (identity_type <> 'REGISTERED_USER' OR user_id IS NOT NULL),
    CHECK (identity_type <> 'GUEST' OR guest_identity IS NOT NULL),
    CHECK (admission_state <> 'ADMITTED' OR admitted_at IS NOT NULL),
    CHECK (admission_state <> 'BANNED' OR banned_at IS NOT NULL)
);
CREATE UNIQUE INDEX uq_participant_registered_user
    ON conference_participants (conference_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX uq_participant_guest_identity
    ON conference_participants (conference_id, guest_identity) WHERE guest_identity IS NOT NULL;
CREATE INDEX idx_participants_waiting
    ON conference_participants (conference_id, requested_at) WHERE admission_state = 'WAITING';
CREATE INDEX idx_participants_conference_state ON conference_participants (conference_id, admission_state);

-- ---------------------------------------------------------------------------
-- participant_sessions: ONE device/network/media session
-- ---------------------------------------------------------------------------
CREATE TABLE participant_sessions (
    id                         UUID PRIMARY KEY,
    conference_participant_id  UUID NOT NULL,
    conference_id              UUID NOT NULL,
    media_identity             VARCHAR(128) NOT NULL,
    state                      TEXT NOT NULL
        CHECK (state IN ('PENDING','CONNECTED','DISCONNECTED','REPLACED','TERMINATED','EXPIRED','HISTORICAL')),
    established_at             TIMESTAMPTZ NOT NULL,
    connected_at               TIMESTAMPTZ,
    disconnected_at            TIMESTAMPTZ,
    expires_at                 TIMESTAMPTZ,
    end_reason                 VARCHAR(64),
    reconnect_of_session_id    UUID REFERENCES participant_sessions(id) ON DELETE SET NULL,
    client_kind                VARCHAR(32),
    legacy_participant_id      UUID UNIQUE,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_session_participant_scope FOREIGN KEY (conference_participant_id, conference_id)
        REFERENCES conference_participants (id, conference_id) ON DELETE RESTRICT,
    CONSTRAINT uq_session_media_identity UNIQUE (conference_id, media_identity),
    CHECK (disconnected_at IS NULL OR disconnected_at >= established_at)
);
CREATE INDEX idx_sessions_participant_state ON participant_sessions (conference_participant_id, state);
CREATE INDEX idx_sessions_conference_active
    ON participant_sessions (conference_id) WHERE state IN ('PENDING','CONNECTED');

-- Invariant: only an ADMITTED participant may hold an active session.
CREATE FUNCTION cf_session_requires_admission() RETURNS trigger AS $$
DECLARE st TEXT;
BEGIN
    IF NEW.state IN ('PENDING','CONNECTED') THEN
        SELECT admission_state INTO st FROM conference_participants
            WHERE id = NEW.conference_participant_id AND conference_id = NEW.conference_id;
        IF st IS DISTINCT FROM 'ADMITTED' THEN
            RAISE EXCEPTION 'participant session requires ADMITTED participant (state=%)', st
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;
CREATE TRIGGER trg_session_requires_admission
    BEFORE INSERT OR UPDATE OF state ON participant_sessions
    FOR EACH ROW EXECUTE FUNCTION cf_session_requires_admission();

-- Invariant: leaving ADMITTED terminates every active session in the same transaction.
CREATE FUNCTION cf_terminate_sessions_on_unadmit() RETURNS trigger AS $$
BEGIN
    IF OLD.admission_state = 'ADMITTED' AND NEW.admission_state <> 'ADMITTED' THEN
        UPDATE participant_sessions
           SET state = 'TERMINATED',
               disconnected_at = COALESCE(disconnected_at, now()),
               end_reason = COALESCE(end_reason, 'ADMISSION_' || NEW.admission_state),
               updated_at = now()
         WHERE conference_participant_id = NEW.id AND state IN ('PENDING','CONNECTED');
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;
CREATE TRIGGER trg_terminate_sessions_on_unadmit
    AFTER UPDATE OF admission_state ON conference_participants
    FOR EACH ROW EXECUTE FUNCTION cf_terminate_sessions_on_unadmit();

-- ---------------------------------------------------------------------------
-- conference_bans
-- ---------------------------------------------------------------------------
CREATE TABLE conference_bans (
    id                 UUID PRIMARY KEY,
    meeting_space_id   UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    conference_id      UUID REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    scope              TEXT NOT NULL CHECK (scope IN ('SPACE','CONFERENCE')),
    subject_type       TEXT NOT NULL CHECK (subject_type IN ('USER','EXTERNAL','GUEST')),
    subject_ref        VARCHAR(320) NOT NULL,
    participant_id     UUID REFERENCES conference_participants(id) ON DELETE SET NULL,
    reason_code        VARCHAR(64),
    reason             TEXT,
    created_by         VARCHAR(128) NOT NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at         TIMESTAMPTZ,
    revoked_at         TIMESTAMPTZ,
    revoked_by         VARCHAR(128),
    CHECK ((scope = 'CONFERENCE') = (conference_id IS NOT NULL))
);
CREATE INDEX idx_bans_lookup
    ON conference_bans (meeting_space_id, subject_type, subject_ref) WHERE revoked_at IS NULL;

-- ---------------------------------------------------------------------------
-- Foundations for later parts (schema only; behaviour arrives in Parts F/G)
-- ---------------------------------------------------------------------------
CREATE TABLE meeting_consents (
    id                 UUID PRIMARY KEY,
    conference_id      UUID NOT NULL,
    participant_id     UUID NOT NULL,
    consent_type       TEXT NOT NULL
        CHECK (consent_type IN ('RECORDING','TRANSCRIPTION','AI_PROCESSING','HEALTHCARE_TERMS','PRIVACY_NOTICE')),
    consent_version    VARCHAR(32) NOT NULL,
    decision           TEXT NOT NULL CHECK (decision IN ('GRANTED','DECLINED','WITHDRAWN')),
    decided_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (participant_id, conference_id) REFERENCES conference_participants (id, conference_id),
    UNIQUE (participant_id, consent_type, consent_version)
);

CREATE TABLE meeting_artifacts (
    id                    UUID PRIMARY KEY,
    meeting_space_id      UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    conference_id         UUID NOT NULL REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    artifact_type         TEXT NOT NULL
        CHECK (artifact_type IN ('RECORDING','TRANSCRIPT','CHAT_EXPORT','ATTENDANCE_REPORT','WHITEBOARD','SUMMARY')),
    provider              VARCHAR(32),
    provider_ref          VARCHAR(256),
    storage_state         TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (storage_state IN ('PENDING','PROCESSING','AVAILABLE','FAILED','DELETED')),
    storage_uri           TEXT,
    size_bytes            BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    checksum_sha256       VARCHAR(64),
    retention_policy_ref  VARCHAR(128),
    created_by            VARCHAR(128),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    available_at          TIMESTAMPTZ,
    deleted_at            TIMESTAMPTZ
);
CREATE INDEX idx_artifacts_conference ON meeting_artifacts (conference_id);

CREATE TABLE breakout_rooms (
    id               UUID PRIMARY KEY,
    conference_id    UUID NOT NULL REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    name             VARCHAR(100) NOT NULL,
    status           TEXT NOT NULL DEFAULT 'CREATED' CHECK (status IN ('CREATED','OPEN','CLOSED')),
    position         INTEGER NOT NULL DEFAULT 0,
    rtc_room_name    VARCHAR(128) UNIQUE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    opened_at        TIMESTAMPTZ,
    closed_at        TIMESTAMPTZ,
    UNIQUE (id, conference_id)
);

CREATE TABLE breakout_assignments (
    id                 UUID PRIMARY KEY,
    breakout_room_id   UUID NOT NULL,
    conference_id      UUID NOT NULL,
    participant_id     UUID NOT NULL,
    assigned_by        VARCHAR(128),
    assigned_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    unassigned_at      TIMESTAMPTZ,
    FOREIGN KEY (breakout_room_id, conference_id) REFERENCES breakout_rooms (id, conference_id),
    FOREIGN KEY (participant_id, conference_id) REFERENCES conference_participants (id, conference_id)
);
CREATE UNIQUE INDEX uq_breakout_active_assignment
    ON breakout_assignments (participant_id) WHERE unassigned_at IS NULL;

CREATE TABLE meeting_polls (
    id                         UUID PRIMARY KEY,
    conference_id              UUID NOT NULL REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    created_by_participant_id  UUID,
    question                   TEXT NOT NULL,
    options                    JSONB NOT NULL DEFAULT '[]'::jsonb,
    poll_type                  TEXT NOT NULL DEFAULT 'SINGLE' CHECK (poll_type IN ('SINGLE','MULTIPLE')),
    is_anonymous               BOOLEAN NOT NULL DEFAULT false,
    status                     TEXT NOT NULL DEFAULT 'DRAFT' CHECK (status IN ('DRAFT','OPEN','CLOSED')),
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    opened_at                  TIMESTAMPTZ,
    closed_at                  TIMESTAMPTZ,
    version                    INTEGER NOT NULL DEFAULT 1,
    UNIQUE (id, conference_id),
    FOREIGN KEY (created_by_participant_id, conference_id) REFERENCES conference_participants (id, conference_id)
);

CREATE TABLE meeting_poll_responses (
    id                UUID PRIMARY KEY,
    poll_id           UUID NOT NULL,
    conference_id     UUID NOT NULL,
    participant_id    UUID NOT NULL,
    selected_options  JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (poll_id, conference_id) REFERENCES meeting_polls (id, conference_id),
    FOREIGN KEY (participant_id, conference_id) REFERENCES conference_participants (id, conference_id),
    UNIQUE (poll_id, participant_id)
);

CREATE TABLE meeting_questions (
    id                UUID PRIMARY KEY,
    conference_id     UUID NOT NULL REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    participant_id    UUID NOT NULL,
    body              TEXT NOT NULL,
    is_anonymous      BOOLEAN NOT NULL DEFAULT false,
    status            TEXT NOT NULL DEFAULT 'OPEN' CHECK (status IN ('OPEN','ANSWERED','DISMISSED','HIDDEN')),
    answer_text       TEXT,
    answered_by       VARCHAR(128),
    upvote_count      INTEGER NOT NULL DEFAULT 0 CHECK (upvote_count >= 0),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (id, conference_id),
    FOREIGN KEY (participant_id, conference_id) REFERENCES conference_participants (id, conference_id)
);

CREATE TABLE meeting_question_votes (
    id                UUID PRIMARY KEY,
    question_id       UUID NOT NULL,
    conference_id     UUID NOT NULL,
    participant_id    UUID NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (question_id, conference_id) REFERENCES meeting_questions (id, conference_id),
    FOREIGN KEY (participant_id, conference_id) REFERENCES conference_participants (id, conference_id),
    UNIQUE (question_id, participant_id)
);

-- ---------------------------------------------------------------------------
-- conference_audit_events: immutable meeting-domain audit trail
-- ---------------------------------------------------------------------------
CREATE TABLE conference_audit_events (
    id                 UUID PRIMARY KEY,
    meeting_space_id   UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    conference_id      UUID REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    actor_type         VARCHAR(32) NOT NULL,
    actor_ref          VARCHAR(128),
    action             VARCHAR(64) NOT NULL,
    target_type        VARCHAR(32),
    target_ref         VARCHAR(128),
    metadata           JSONB NOT NULL DEFAULT '{}'::jsonb,
    request_id         VARCHAR(64),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_audit_space ON conference_audit_events (meeting_space_id, created_at);
CREATE INDEX idx_audit_conference ON conference_audit_events (conference_id, created_at) WHERE conference_id IS NOT NULL;

CREATE FUNCTION cf_audit_immutable() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'conference_audit_events is append-only' USING ERRCODE = 'insufficient_privilege';
END $$ LANGUAGE plpgsql;
CREATE TRIGGER trg_audit_immutable
    BEFORE UPDATE OR DELETE ON conference_audit_events
    FOR EACH ROW EXECUTE FUNCTION cf_audit_immutable();

-- ---------------------------------------------------------------------------
-- outbox_events: transactional outbox (delivery, never authority)
-- ---------------------------------------------------------------------------
CREATE TABLE outbox_events (
    id                 UUID PRIMARY KEY,
    aggregate_type     VARCHAR(64) NOT NULL,
    aggregate_id       UUID NOT NULL,
    event_type         VARCHAR(96) NOT NULL,
    payload            JSONB NOT NULL DEFAULT '{}'::jsonb,
    meeting_space_id   UUID,
    conference_id      UUID,
    status             TEXT NOT NULL DEFAULT 'PENDING' CHECK (status IN ('PENDING','PUBLISHED','FAILED')),
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at       TIMESTAMPTZ,
    last_error         TEXT,
    request_id         VARCHAR(64),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_outbox_pending ON outbox_events (available_at) WHERE status = 'PENDING';
CREATE INDEX idx_outbox_aggregate ON outbox_events (aggregate_type, aggregate_id, created_at);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS outbox_events;
DROP TRIGGER IF EXISTS trg_audit_immutable ON conference_audit_events;
DROP TABLE IF EXISTS conference_audit_events;
DROP FUNCTION IF EXISTS cf_audit_immutable();
DROP TABLE IF EXISTS meeting_question_votes;
DROP TABLE IF EXISTS meeting_questions;
DROP TABLE IF EXISTS meeting_poll_responses;
DROP TABLE IF EXISTS meeting_polls;
DROP TABLE IF EXISTS breakout_assignments;
DROP TABLE IF EXISTS breakout_rooms;
DROP TABLE IF EXISTS meeting_artifacts;
DROP TABLE IF EXISTS meeting_consents;
DROP TABLE IF EXISTS conference_bans;
DROP TABLE IF EXISTS participant_sessions;
DROP FUNCTION IF EXISTS cf_session_requires_admission();
DROP TABLE IF EXISTS conference_participants;
DROP FUNCTION IF EXISTS cf_terminate_sessions_on_unadmit();
DROP TABLE IF EXISTS meeting_invitations;
DROP TABLE IF EXISTS meeting_members;
DROP TABLE IF EXISTS conference_sessions;
DROP TABLE IF EXISTS meeting_recurrence_rules;
DROP TABLE IF EXISTS meeting_spaces;
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

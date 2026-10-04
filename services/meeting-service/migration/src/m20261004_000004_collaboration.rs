//! Phase 2 Part F — media controls & collaboration (additive).
//!
//! - `meeting_spaces.collaboration_policy` / `conference_sessions.collaboration_policy`:
//!   sparse policy overrides (space defaults → conference overrides) for chat,
//!   reactions, hand raise, Q&A, polls, screen-share policy, conference-wide media locks
//!   and pre-join consent requirements. `collaboration_version` versions the
//!   conference-level policy.
//! - `conference_participants`: authoritative per-participant media restrictions
//!   (audio / video / screen-share locks), chat restriction, hand raise, spotlight and
//!   the privileged breakout visit of a host.
//! - `participant_sessions.screen_share_*`: provider-evidenced active screen share.
//! - `conference_messages`: v2 meeting messaging storage behind
//!   `MeetingMessagingService` (main / breakout / private scope). The legacy
//!   `chat_messages` table is bound to legacy meetings and stays untouched.
//! - breakout rooms / assignments, polls, poll responses, questions: extended in place.
//! - `meeting_poll_ballots`: who voted (duplicate prevention), kept apart from the
//!   answer rows so anonymous answers carry no participant identity.
//! - `outbox_events.audience`: server-side realtime audience (private chat, breakout
//!   scope, moderators only). Never part of the client envelope.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE meeting_spaces
    ADD COLUMN collaboration_policy JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE conference_sessions
    ADD COLUMN collaboration_policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN collaboration_version INTEGER NOT NULL DEFAULT 1 CHECK (collaboration_version >= 1);

ALTER TABLE conference_participants
    ADD COLUMN audio_locked_at        TIMESTAMPTZ,
    ADD COLUMN audio_locked_by        VARCHAR(128),
    ADD COLUMN video_locked_at        TIMESTAMPTZ,
    ADD COLUMN video_locked_by        VARCHAR(128),
    ADD COLUMN screen_share_locked_at TIMESTAMPTZ,
    ADD COLUMN screen_share_locked_by VARCHAR(128),
    ADD COLUMN chat_restricted_at     TIMESTAMPTZ,
    ADD COLUMN chat_restricted_by     VARCHAR(128),
    ADD COLUMN hand_raised_at         TIMESTAMPTZ,
    ADD COLUMN hand_lowered_at        TIMESTAMPTZ,
    ADD COLUMN spotlighted_at         TIMESTAMPTZ,
    ADD COLUMN spotlighted_by         VARCHAR(128),
    ADD COLUMN breakout_visit_room_id UUID,
    ADD COLUMN collab_version         INTEGER NOT NULL DEFAULT 1 CHECK (collab_version >= 1);
CREATE INDEX idx_participants_raised_hands
    ON conference_participants (conference_id, hand_raised_at, id) WHERE hand_raised_at IS NOT NULL;
CREATE INDEX idx_participants_spotlight
    ON conference_participants (conference_id, spotlighted_at) WHERE spotlighted_at IS NOT NULL;

ALTER TABLE participant_sessions
    ADD COLUMN screen_share_started_at TIMESTAMPTZ,
    ADD COLUMN screen_share_track_ref  VARCHAR(160);
CREATE INDEX idx_sessions_screen_share
    ON participant_sessions (conference_id) WHERE screen_share_started_at IS NOT NULL;

-- Breakouts (owned by the ConferenceSession, never by a MeetingSpace).
ALTER TABLE breakout_rooms DROP CONSTRAINT IF EXISTS breakout_rooms_status_check;
ALTER TABLE breakout_rooms
    ADD CONSTRAINT breakout_rooms_status_check CHECK (status IN ('CREATED','OPEN','CLOSING','CLOSED')),
    ADD COLUMN capacity            INTEGER CHECK (capacity IS NULL OR capacity BETWEEN 1 AND 1000),
    ADD COLUMN created_by          VARCHAR(128),
    ADD COLUMN closing_at          TIMESTAMPTZ,
    ADD COLUMN close_requested_at  TIMESTAMPTZ,
    ADD COLUMN recording_policy    TEXT NOT NULL DEFAULT 'INHERIT_MAIN_POLICY'
        CHECK (recording_policy IN ('INHERIT_MAIN_POLICY','DISABLED','SEPARATE_ALLOWED')),
    ADD COLUMN config              JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN rtc_room_ready_at   TIMESTAMPTZ,
    ADD COLUMN version             INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    ADD CONSTRAINT breakout_closing_deadline CHECK (status <> 'CLOSING' OR closing_at IS NOT NULL);
CREATE UNIQUE INDEX uq_breakout_name_active
    ON breakout_rooms (conference_id, lower(name)) WHERE status <> 'CLOSED';
CREATE INDEX idx_breakout_conference_status ON breakout_rooms (conference_id, status, position);
CREATE INDEX idx_breakout_closing_due ON breakout_rooms (closing_at) WHERE status = 'CLOSING';
ALTER TABLE conference_participants
    ADD CONSTRAINT fk_participant_breakout_visit
        FOREIGN KEY (breakout_visit_room_id, conference_id) REFERENCES breakout_rooms (id, conference_id);

ALTER TABLE breakout_assignments
    ADD COLUMN mode TEXT NOT NULL DEFAULT 'MANUAL' CHECK (mode IN ('MANUAL','RANDOM','BALANCED','MOVE'));
CREATE INDEX idx_breakout_assignments_room_active
    ON breakout_assignments (breakout_room_id) WHERE unassigned_at IS NULL;
CREATE INDEX idx_breakout_assignments_conference_active
    ON breakout_assignments (conference_id) WHERE unassigned_at IS NULL;

-- Meeting messaging (v2).
CREATE TABLE conference_messages (
    id                       UUID PRIMARY KEY,
    conference_id            UUID NOT NULL REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    breakout_room_id         UUID,
    sender_participant_id    UUID NOT NULL,
    recipient_participant_id UUID,
    kind                     TEXT NOT NULL DEFAULT 'MESSAGE'
        CHECK (kind IN ('MESSAGE','ANNOUNCEMENT','BREAKOUT_ANNOUNCEMENT')),
    body                     TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 4000),
    created_at               TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    deleted_at               TIMESTAMPTZ,
    deleted_by               VARCHAR(128),
    FOREIGN KEY (sender_participant_id, conference_id) REFERENCES conference_participants (id, conference_id),
    FOREIGN KEY (recipient_participant_id, conference_id) REFERENCES conference_participants (id, conference_id),
    FOREIGN KEY (breakout_room_id, conference_id) REFERENCES breakout_rooms (id, conference_id),
    CHECK (recipient_participant_id IS NULL OR breakout_room_id IS NULL),
    CHECK (recipient_participant_id IS NULL OR recipient_participant_id <> sender_participant_id)
);
CREATE INDEX idx_messages_room_cursor
    ON conference_messages (conference_id, breakout_room_id, created_at DESC, id DESC)
    WHERE recipient_participant_id IS NULL;
CREATE INDEX idx_messages_private_cursor
    ON conference_messages (conference_id, sender_participant_id, recipient_participant_id, created_at DESC, id DESC)
    WHERE recipient_participant_id IS NOT NULL;
CREATE INDEX idx_messages_recipient
    ON conference_messages (conference_id, recipient_participant_id, created_at DESC)
    WHERE recipient_participant_id IS NOT NULL;

-- Polls / quizzes.
ALTER TABLE meeting_polls DROP CONSTRAINT IF EXISTS meeting_polls_status_check;
ALTER TABLE meeting_polls DROP CONSTRAINT IF EXISTS meeting_polls_poll_type_check;
ALTER TABLE meeting_polls
    ADD CONSTRAINT meeting_polls_status_check CHECK (status IN ('DRAFT','OPEN','CLOSED','ARCHIVED')),
    ADD CONSTRAINT meeting_polls_poll_type_check CHECK (poll_type IN ('SINGLE','MULTIPLE','MIXED')),
    ADD COLUMN definition          JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN quiz_mode           BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN results_visibility  TEXT NOT NULL DEFAULT 'AFTER_CLOSE'
        CHECK (results_visibility IN ('HOST_ONLY','AFTER_VOTE','AFTER_CLOSE')),
    ADD COLUMN created_by          VARCHAR(128),
    ADD COLUMN archived_at         TIMESTAMPTZ,
    ADD COLUMN updated_at          TIMESTAMPTZ NOT NULL DEFAULT now();
CREATE INDEX idx_polls_conference ON meeting_polls (conference_id, status, created_at);

ALTER TABLE meeting_poll_responses
    ALTER COLUMN participant_id DROP NOT NULL,
    ADD COLUMN answers   JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN score     DOUBLE PRECISION,
    ADD COLUMN max_score DOUBLE PRECISION;
CREATE INDEX idx_poll_responses_poll ON meeting_poll_responses (poll_id);

CREATE TABLE meeting_poll_ballots (
    poll_id          UUID NOT NULL,
    conference_id    UUID NOT NULL,
    participant_id   UUID NOT NULL,
    request_digest   VARCHAR(64),
    voted_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (poll_id, participant_id),
    FOREIGN KEY (poll_id, conference_id) REFERENCES meeting_polls (id, conference_id),
    FOREIGN KEY (participant_id, conference_id) REFERENCES conference_participants (id, conference_id)
);

-- Q&A.
ALTER TABLE meeting_questions DROP CONSTRAINT IF EXISTS meeting_questions_status_check;
ALTER TABLE meeting_questions
    ADD CONSTRAINT meeting_questions_status_check
        CHECK (status IN ('OPEN','ANSWERED','DISMISSED','HIDDEN','ARCHIVED')),
    ADD COLUMN pinned_at     TIMESTAMPTZ,
    ADD COLUMN pinned_by     VARCHAR(128),
    ADD COLUMN answered_at   TIMESTAMPTZ,
    ADD COLUMN dismissed_at  TIMESTAMPTZ,
    ADD COLUMN archived_at   TIMESTAMPTZ,
    ADD COLUMN version       INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1);
CREATE INDEX idx_questions_conference ON meeting_questions (conference_id, status, created_at);
CREATE INDEX idx_question_votes_question ON meeting_question_votes (question_id);

-- Realtime audience (server-side filtering only).
ALTER TABLE outbox_events ADD COLUMN audience JSONB;
"#;

const DOWN: &str = r#"
ALTER TABLE outbox_events DROP COLUMN IF EXISTS audience;

DROP INDEX IF EXISTS idx_question_votes_question;
DROP INDEX IF EXISTS idx_questions_conference;
ALTER TABLE meeting_questions
    DROP COLUMN IF EXISTS version,
    DROP COLUMN IF EXISTS archived_at,
    DROP COLUMN IF EXISTS dismissed_at,
    DROP COLUMN IF EXISTS answered_at,
    DROP COLUMN IF EXISTS pinned_by,
    DROP COLUMN IF EXISTS pinned_at;
UPDATE meeting_questions SET status = 'HIDDEN' WHERE status = 'ARCHIVED';
ALTER TABLE meeting_questions DROP CONSTRAINT IF EXISTS meeting_questions_status_check;
ALTER TABLE meeting_questions
    ADD CONSTRAINT meeting_questions_status_check CHECK (status IN ('OPEN','ANSWERED','DISMISSED','HIDDEN'));

DROP TABLE IF EXISTS meeting_poll_ballots;
DROP INDEX IF EXISTS idx_poll_responses_poll;
DELETE FROM meeting_poll_responses WHERE participant_id IS NULL;
ALTER TABLE meeting_poll_responses
    DROP COLUMN IF EXISTS max_score,
    DROP COLUMN IF EXISTS score,
    DROP COLUMN IF EXISTS answers,
    ALTER COLUMN participant_id SET NOT NULL;
DROP INDEX IF EXISTS idx_polls_conference;
ALTER TABLE meeting_polls
    DROP COLUMN IF EXISTS updated_at,
    DROP COLUMN IF EXISTS archived_at,
    DROP COLUMN IF EXISTS created_by,
    DROP COLUMN IF EXISTS results_visibility,
    DROP COLUMN IF EXISTS quiz_mode,
    DROP COLUMN IF EXISTS definition;
UPDATE meeting_polls SET status = 'CLOSED' WHERE status = 'ARCHIVED';
UPDATE meeting_polls SET poll_type = 'MULTIPLE' WHERE poll_type = 'MIXED';
ALTER TABLE meeting_polls DROP CONSTRAINT IF EXISTS meeting_polls_status_check;
ALTER TABLE meeting_polls DROP CONSTRAINT IF EXISTS meeting_polls_poll_type_check;
ALTER TABLE meeting_polls
    ADD CONSTRAINT meeting_polls_status_check CHECK (status IN ('DRAFT','OPEN','CLOSED')),
    ADD CONSTRAINT meeting_polls_poll_type_check CHECK (poll_type IN ('SINGLE','MULTIPLE'));

DROP TABLE IF EXISTS conference_messages;

DROP INDEX IF EXISTS idx_breakout_assignments_conference_active;
DROP INDEX IF EXISTS idx_breakout_assignments_room_active;
ALTER TABLE breakout_assignments DROP COLUMN IF EXISTS mode;
ALTER TABLE conference_participants DROP CONSTRAINT IF EXISTS fk_participant_breakout_visit;
DROP INDEX IF EXISTS idx_breakout_closing_due;
DROP INDEX IF EXISTS idx_breakout_conference_status;
DROP INDEX IF EXISTS uq_breakout_name_active;
UPDATE breakout_rooms SET status = 'CLOSED' WHERE status = 'CLOSING';
ALTER TABLE breakout_rooms
    DROP CONSTRAINT IF EXISTS breakout_closing_deadline,
    DROP COLUMN IF EXISTS version,
    DROP COLUMN IF EXISTS rtc_room_ready_at,
    DROP COLUMN IF EXISTS config,
    DROP COLUMN IF EXISTS recording_policy,
    DROP COLUMN IF EXISTS close_requested_at,
    DROP COLUMN IF EXISTS closing_at,
    DROP COLUMN IF EXISTS created_by,
    DROP COLUMN IF EXISTS capacity;
ALTER TABLE breakout_rooms DROP CONSTRAINT IF EXISTS breakout_rooms_status_check;
ALTER TABLE breakout_rooms
    ADD CONSTRAINT breakout_rooms_status_check CHECK (status IN ('CREATED','OPEN','CLOSED'));

DROP INDEX IF EXISTS idx_sessions_screen_share;
ALTER TABLE participant_sessions
    DROP COLUMN IF EXISTS screen_share_track_ref,
    DROP COLUMN IF EXISTS screen_share_started_at;

DROP INDEX IF EXISTS idx_participants_spotlight;
DROP INDEX IF EXISTS idx_participants_raised_hands;
ALTER TABLE conference_participants
    DROP COLUMN IF EXISTS collab_version,
    DROP COLUMN IF EXISTS breakout_visit_room_id,
    DROP COLUMN IF EXISTS spotlighted_by,
    DROP COLUMN IF EXISTS spotlighted_at,
    DROP COLUMN IF EXISTS hand_lowered_at,
    DROP COLUMN IF EXISTS hand_raised_at,
    DROP COLUMN IF EXISTS chat_restricted_by,
    DROP COLUMN IF EXISTS chat_restricted_at,
    DROP COLUMN IF EXISTS screen_share_locked_by,
    DROP COLUMN IF EXISTS screen_share_locked_at,
    DROP COLUMN IF EXISTS video_locked_by,
    DROP COLUMN IF EXISTS video_locked_at,
    DROP COLUMN IF EXISTS audio_locked_by,
    DROP COLUMN IF EXISTS audio_locked_at;

ALTER TABLE conference_sessions
    DROP COLUMN IF EXISTS collaboration_version,
    DROP COLUMN IF EXISTS collaboration_policy;
ALTER TABLE meeting_spaces DROP COLUMN IF EXISTS collaboration_policy;
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

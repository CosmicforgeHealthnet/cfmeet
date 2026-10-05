//! Phase 2 Parts G, H and I — recording & artefacts, specialized conference profiles,
//! quality & operations (additive only).
//!
//! Part G
//! - `meeting_artifacts` becomes the unified artefact model: full recording state
//!   machine (`status`), opaque storage reference (`storage_bucket` / `storage_key`,
//!   never a public URL), typed metadata, retention, integrity fields, worker leases and
//!   one-active-composite-recording / provider-reference / idempotency uniqueness.
//! - `meeting_consents` gains DATA_SHARING and withdrawal / provenance columns.
//! - `transcription_jobs`, `transcript_segments` (FINAL segments only, deduplicated per
//!   provider segment id) and `transcript_translations` (downstream, never overwrites).
//!
//! Part H
//! - sparse `profile_policy` on organizations / spaces / conferences.
//! - external (e.g. clinical) role reference, authorization reference + validity window
//!   and backstage context on `conference_participants` — references only, never
//!   clinical payloads.
//! - `webinar_registrations` (approved fields only; the admission token is a normal
//!   hashed `meeting_invitations` row).
//!
//! Part I
//! - low-frequency quality aggregates on `participant_sessions` / `conference_sessions`
//!   (raw samples go to the telemetry sink, never to these tables).
//! - `rtc_drift_events` generalised into operational incidents (status, dedupe key,
//!   occurrences) instead of a second drift store.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ===================================================================== Part G
ALTER TABLE meeting_artifacts DROP CONSTRAINT IF EXISTS meeting_artifacts_artifact_type_check;
ALTER TABLE meeting_artifacts ADD CONSTRAINT meeting_artifacts_artifact_type_check
    CHECK (artifact_type IN ('RECORDING','TRANSCRIPT','CAPTION_TRACK','ATTENDANCE_REPORT',
                             'CHAT_EXPORT','POLL_REPORT','MEETING_SUMMARY','WHITEBOARD_SNAPSHOT',
                             'WHITEBOARD','SUMMARY'));
ALTER TABLE meeting_artifacts
    ADD COLUMN status TEXT NOT NULL DEFAULT 'REQUESTED'
        CHECK (status IN ('REQUESTED','RECORDING','STOPPING','PROCESSING','READY','FAILED','DELETED')),
    ADD COLUMN storage_bucket      VARCHAR(128),
    ADD COLUMN storage_key         VARCHAR(512),
    ADD COLUMN content_type        VARCHAR(128),
    ADD COLUMN duration_ms         BIGINT CHECK (duration_ms IS NULL OR duration_ms >= 0),
    ADD COLUMN layout              VARCHAR(32),
    ADD COLUMN metadata            JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN retention_at        TIMESTAMPTZ,
    ADD COLUMN started_at          TIMESTAMPTZ,
    ADD COLUMN ended_at            TIMESTAMPTZ,
    ADD COLUMN finalized_at        TIMESTAMPTZ,
    ADD COLUMN stop_requested_at   TIMESTAMPTZ,
    ADD COLUMN delete_requested_at TIMESTAMPTZ,
    ADD COLUMN delete_reason       VARCHAR(32),
    ADD COLUMN failure_code        VARCHAR(64),
    ADD COLUMN failure_detail      TEXT,
    ADD COLUMN attempts            INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    ADD COLUMN next_attempt_at     TIMESTAMPTZ,
    ADD COLUMN locked_by           VARCHAR(128),
    ADD COLUMN locked_until        TIMESTAMPTZ,
    ADD COLUMN verified_at         TIMESTAMPTZ,
    ADD COLUMN request_id          VARCHAR(128),
    ADD COLUMN idempotency_key     VARCHAR(128),
    ADD COLUMN source_artifact_id  UUID REFERENCES meeting_artifacts(id) ON DELETE RESTRICT,
    ADD COLUMN version             INTEGER NOT NULL DEFAULT 1;
UPDATE meeting_artifacts
   SET status = CASE storage_state WHEN 'AVAILABLE' THEN 'READY'
                                   WHEN 'PENDING' THEN 'REQUESTED'
                                   ELSE storage_state END;
ALTER TABLE meeting_artifacts ADD CONSTRAINT uq_artifact_id_conference UNIQUE (id, conference_id);
-- READY only with a validated object; DELETED only after confirmed deletion.
ALTER TABLE meeting_artifacts ADD CONSTRAINT ck_artifact_ready_validated
    CHECK (status <> 'READY' OR (storage_key IS NOT NULL AND checksum_sha256 IS NOT NULL
                                 AND size_bytes IS NOT NULL AND finalized_at IS NOT NULL)) NOT VALID;
ALTER TABLE meeting_artifacts ADD CONSTRAINT ck_artifact_deleted_confirmed
    CHECK (status <> 'DELETED' OR deleted_at IS NOT NULL) NOT VALID;
CREATE UNIQUE INDEX uq_artifact_active_recording ON meeting_artifacts (conference_id)
    WHERE artifact_type = 'RECORDING' AND status IN ('REQUESTED','RECORDING','STOPPING');
CREATE UNIQUE INDEX uq_artifact_provider_ref ON meeting_artifacts (provider, provider_ref)
    WHERE provider_ref IS NOT NULL;
CREATE UNIQUE INDEX uq_artifact_idempotency ON meeting_artifacts (conference_id, artifact_type, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX idx_artifact_lifecycle ON meeting_artifacts (status, updated_at)
    WHERE status IN ('REQUESTED','RECORDING','STOPPING','PROCESSING');
CREATE INDEX idx_artifact_deletion_due ON meeting_artifacts (retention_at)
    WHERE status <> 'DELETED' AND (retention_at IS NOT NULL OR delete_requested_at IS NOT NULL);
CREATE INDEX idx_artifact_conference_type ON meeting_artifacts (conference_id, artifact_type, created_at);

ALTER TABLE meeting_consents DROP CONSTRAINT IF EXISTS meeting_consents_consent_type_check;
ALTER TABLE meeting_consents ADD CONSTRAINT meeting_consents_consent_type_check
    CHECK (consent_type IN ('RECORDING','TRANSCRIPTION','AI_PROCESSING','DATA_SHARING',
                            'HEALTHCARE_TERMS','PRIVACY_NOTICE'));
ALTER TABLE meeting_consents
    ADD COLUMN withdrawn_at TIMESTAMPTZ,
    ADD COLUMN source       VARCHAR(32),
    ADD COLUMN request_id   VARCHAR(128),
    ADD COLUMN updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD CONSTRAINT ck_consent_withdrawn CHECK (decision <> 'WITHDRAWN' OR withdrawn_at IS NOT NULL) NOT VALID;
CREATE INDEX idx_consents_conference_type ON meeting_consents (conference_id, consent_type, consent_version);

CREATE TABLE transcription_jobs (
    id                 UUID PRIMARY KEY,
    conference_id      UUID NOT NULL REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    artifact_id        UUID NOT NULL,
    mode               TEXT NOT NULL CHECK (mode IN ('LIVE','POST_MEETING')),
    status             TEXT NOT NULL
        CHECK (status IN ('REQUESTED','ACTIVE','STOPPING','PROCESSING','READY','FAILED','CANCELLED')),
    provider           VARCHAR(32) NOT NULL,
    provider_job_ref   VARCHAR(256),
    language           VARCHAR(16) NOT NULL,
    source_artifact_id UUID,
    requested_by       VARCHAR(128),
    segment_count      INTEGER NOT NULL DEFAULT 0 CHECK (segment_count >= 0),
    failure_code       VARCHAR(64),
    started_at         TIMESTAMPTZ,
    stop_requested_at  TIMESTAMPTZ,
    ended_at           TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    version            INTEGER NOT NULL DEFAULT 1,
    UNIQUE (id, conference_id),
    FOREIGN KEY (artifact_id, conference_id) REFERENCES meeting_artifacts (id, conference_id)
);
CREATE UNIQUE INDEX uq_transcription_active_live ON transcription_jobs (conference_id)
    WHERE mode = 'LIVE' AND status IN ('REQUESTED','ACTIVE','STOPPING');
CREATE UNIQUE INDEX uq_transcription_provider_ref ON transcription_jobs (provider, provider_job_ref)
    WHERE provider_job_ref IS NOT NULL;

CREATE TABLE transcript_segments (
    id                     UUID PRIMARY KEY,
    conference_id          UUID NOT NULL,
    job_id                 UUID NOT NULL,
    artifact_id            UUID NOT NULL,
    provider_segment_id    VARCHAR(128) NOT NULL,
    sequence               BIGINT NOT NULL CHECK (sequence >= 0),
    speaker_participant_id UUID,
    start_ms               BIGINT NOT NULL CHECK (start_ms >= 0),
    end_ms                 BIGINT NOT NULL,
    language               VARCHAR(16) NOT NULL,
    text                   TEXT NOT NULL CHECK (char_length(text) <= 10000),
    confidence             REAL CHECK (confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (end_ms >= start_ms),
    UNIQUE (job_id, provider_segment_id),
    FOREIGN KEY (job_id, conference_id) REFERENCES transcription_jobs (id, conference_id) ON DELETE CASCADE,
    FOREIGN KEY (speaker_participant_id, conference_id) REFERENCES conference_participants (id, conference_id)
);
CREATE INDEX idx_segments_job_sequence ON transcript_segments (job_id, sequence);

CREATE TABLE transcript_translations (
    id              UUID PRIMARY KEY,
    segment_id      UUID NOT NULL REFERENCES transcript_segments(id) ON DELETE CASCADE,
    conference_id   UUID NOT NULL,
    source_language VARCHAR(16) NOT NULL,
    target_language VARCHAR(16) NOT NULL,
    text            TEXT NOT NULL CHECK (char_length(text) <= 10000),
    provider        VARCHAR(32) NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (segment_id, target_language)
);

-- ===================================================================== Part H
ALTER TABLE organizations ADD COLUMN profile_policy JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE meeting_spaces
    ADD COLUMN profile_policy         JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN profile_policy_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE conference_sessions
    ADD COLUMN profile_policy      JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN backstage_room_name VARCHAR(128) UNIQUE,
    ADD COLUMN backstage_ready_at  TIMESTAMPTZ;
ALTER TABLE conference_participants
    ADD COLUMN external_role              VARCHAR(32),
    ADD COLUMN external_authz_ref         VARCHAR(128),
    ADD COLUMN external_authz_valid_until TIMESTAMPTZ,
    ADD COLUMN backstage_at               TIMESTAMPTZ,
    ADD COLUMN registration_id            UUID;
CREATE INDEX idx_participants_backstage ON conference_participants (conference_id)
    WHERE backstage_at IS NOT NULL;

CREATE TABLE webinar_registrations (
    id                     UUID PRIMARY KEY,
    meeting_space_id       UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    conference_id          UUID REFERENCES conference_sessions(id) ON DELETE RESTRICT,
    status                 TEXT NOT NULL DEFAULT 'REGISTERED' CHECK (status IN ('REGISTERED','CANCELLED')),
    display_name           VARCHAR(100) NOT NULL,
    email                  VARCHAR(254),
    organization_name      VARCHAR(120),
    custom_fields          JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (pg_column_size(custom_fields) <= 4096),
    user_id                UUID,
    invitation_id          UUID NOT NULL REFERENCES meeting_invitations(id) ON DELETE RESTRICT,
    idempotency_key_digest VARCHAR(64),
    request_digest         VARCHAR(64),
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    cancelled_at           TIMESTAMPTZ,
    cancelled_by           VARCHAR(128)
);
CREATE UNIQUE INDEX uq_registration_idempotency ON webinar_registrations (meeting_space_id, idempotency_key_digest)
    WHERE idempotency_key_digest IS NOT NULL;
CREATE UNIQUE INDEX uq_registration_email ON webinar_registrations
    (meeting_space_id, COALESCE(conference_id, '00000000-0000-0000-0000-000000000000'::uuid), lower(email))
    WHERE email IS NOT NULL AND status = 'REGISTERED';
CREATE INDEX idx_registrations_space ON webinar_registrations (meeting_space_id, created_at);

-- ===================================================================== Part I
ALTER TABLE participant_sessions
    ADD COLUMN join_requested_at  TIMESTAMPTZ,
    ADD COLUMN join_latency_ms    INTEGER CHECK (join_latency_ms IS NULL OR join_latency_ms >= 0),
    ADD COLUMN join_outcome       VARCHAR(16),
    ADD COLUMN reconnect_count    INTEGER NOT NULL DEFAULT 0 CHECK (reconnect_count >= 0),
    ADD COLUMN quality_samples    INTEGER NOT NULL DEFAULT 0 CHECK (quality_samples >= 0),
    ADD COLUMN avg_rtt_ms         REAL,
    ADD COLUMN max_rtt_ms         REAL,
    ADD COLUMN p95_rtt_ms         REAL,
    ADD COLUMN avg_jitter_ms      REAL,
    ADD COLUMN packet_loss_pct    REAL,
    ADD COLUMN avg_bitrate_kbps   REAL,
    ADD COLUMN min_fps            REAL,
    ADD COLUMN last_resolution    VARCHAR(16),
    ADD COLUMN rtt_histogram      JSONB NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN quality_score      SMALLINT CHECK (quality_score IS NULL OR (quality_score >= 0 AND quality_score <= 100)),
    ADD COLUMN quality_state      VARCHAR(16),
    ADD COLUMN last_quality_at    TIMESTAMPTZ,
    ADD COLUMN last_quality_batch VARCHAR(64),
    ADD COLUMN publish_failures   INTEGER NOT NULL DEFAULT 0 CHECK (publish_failures >= 0),
    ADD COLUMN subscribe_failures INTEGER NOT NULL DEFAULT 0 CHECK (subscribe_failures >= 0),
    ADD COLUMN provider_region    VARCHAR(64),
    ADD COLUMN provider_node      VARCHAR(64);
ALTER TABLE conference_sessions
    ADD COLUMN join_attempts               BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN join_successes              BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN join_media_connected        BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN join_denials                BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN join_provider_failures      BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN join_timeouts               BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN join_client_failures        BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN reconnect_count             BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN publish_failure_count       BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN subscribe_failure_count     BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN provider_drift_count        BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN recording_failure_count     BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN transcription_failure_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN unhealthy_participant_count INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN health_state                VARCHAR(16) NOT NULL DEFAULT 'UNKNOWN'
        CHECK (health_state IN ('HEALTHY','DEGRADED','UNHEALTHY','UNKNOWN')),
    ADD COLUMN health_evaluated_at         TIMESTAMPTZ,
    ADD COLUMN last_incident_at            TIMESTAMPTZ;

ALTER TABLE rtc_drift_events
    ADD COLUMN status       VARCHAR(16) NOT NULL DEFAULT 'CLOSED' CHECK (status IN ('OPEN','RESOLVED','CLOSED')),
    ADD COLUMN category     VARCHAR(32) NOT NULL DEFAULT 'RTC',
    ADD COLUMN severity     VARCHAR(16) NOT NULL DEFAULT 'WARNING' CHECK (severity IN ('INFO','WARNING','CRITICAL')),
    ADD COLUMN dedupe_key   VARCHAR(256),
    ADD COLUMN occurrences  INTEGER NOT NULL DEFAULT 1 CHECK (occurrences >= 1),
    ADD COLUMN last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN resolved_at  TIMESTAMPTZ;
CREATE UNIQUE INDEX uq_incident_open ON rtc_drift_events (dedupe_key)
    WHERE status = 'OPEN' AND dedupe_key IS NOT NULL;
CREATE INDEX idx_incidents_status ON rtc_drift_events (status, last_seen_at);
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_incidents_status;
DROP INDEX IF EXISTS uq_incident_open;
ALTER TABLE rtc_drift_events
    DROP COLUMN IF EXISTS resolved_at,
    DROP COLUMN IF EXISTS last_seen_at,
    DROP COLUMN IF EXISTS occurrences,
    DROP COLUMN IF EXISTS dedupe_key,
    DROP COLUMN IF EXISTS severity,
    DROP COLUMN IF EXISTS category,
    DROP COLUMN IF EXISTS status;
ALTER TABLE conference_sessions
    DROP COLUMN IF EXISTS last_incident_at,
    DROP COLUMN IF EXISTS health_evaluated_at,
    DROP COLUMN IF EXISTS health_state,
    DROP COLUMN IF EXISTS unhealthy_participant_count,
    DROP COLUMN IF EXISTS transcription_failure_count,
    DROP COLUMN IF EXISTS recording_failure_count,
    DROP COLUMN IF EXISTS provider_drift_count,
    DROP COLUMN IF EXISTS subscribe_failure_count,
    DROP COLUMN IF EXISTS publish_failure_count,
    DROP COLUMN IF EXISTS reconnect_count,
    DROP COLUMN IF EXISTS join_client_failures,
    DROP COLUMN IF EXISTS join_timeouts,
    DROP COLUMN IF EXISTS join_provider_failures,
    DROP COLUMN IF EXISTS join_denials,
    DROP COLUMN IF EXISTS join_media_connected,
    DROP COLUMN IF EXISTS join_successes,
    DROP COLUMN IF EXISTS join_attempts;
ALTER TABLE participant_sessions
    DROP COLUMN IF EXISTS provider_node,
    DROP COLUMN IF EXISTS provider_region,
    DROP COLUMN IF EXISTS subscribe_failures,
    DROP COLUMN IF EXISTS publish_failures,
    DROP COLUMN IF EXISTS last_quality_batch,
    DROP COLUMN IF EXISTS last_quality_at,
    DROP COLUMN IF EXISTS quality_state,
    DROP COLUMN IF EXISTS quality_score,
    DROP COLUMN IF EXISTS rtt_histogram,
    DROP COLUMN IF EXISTS last_resolution,
    DROP COLUMN IF EXISTS min_fps,
    DROP COLUMN IF EXISTS avg_bitrate_kbps,
    DROP COLUMN IF EXISTS packet_loss_pct,
    DROP COLUMN IF EXISTS avg_jitter_ms,
    DROP COLUMN IF EXISTS p95_rtt_ms,
    DROP COLUMN IF EXISTS max_rtt_ms,
    DROP COLUMN IF EXISTS avg_rtt_ms,
    DROP COLUMN IF EXISTS quality_samples,
    DROP COLUMN IF EXISTS reconnect_count,
    DROP COLUMN IF EXISTS join_outcome,
    DROP COLUMN IF EXISTS join_latency_ms,
    DROP COLUMN IF EXISTS join_requested_at;

DROP TABLE IF EXISTS webinar_registrations;
DROP INDEX IF EXISTS idx_participants_backstage;
ALTER TABLE conference_participants
    DROP COLUMN IF EXISTS registration_id,
    DROP COLUMN IF EXISTS backstage_at,
    DROP COLUMN IF EXISTS external_authz_valid_until,
    DROP COLUMN IF EXISTS external_authz_ref,
    DROP COLUMN IF EXISTS external_role;
ALTER TABLE conference_sessions
    DROP COLUMN IF EXISTS backstage_ready_at,
    DROP COLUMN IF EXISTS backstage_room_name,
    DROP COLUMN IF EXISTS profile_policy;
ALTER TABLE meeting_spaces
    DROP COLUMN IF EXISTS profile_policy_version,
    DROP COLUMN IF EXISTS profile_policy;
ALTER TABLE organizations DROP COLUMN IF EXISTS profile_policy;

DROP TABLE IF EXISTS transcript_translations;
DROP TABLE IF EXISTS transcript_segments;
DROP TABLE IF EXISTS transcription_jobs;

DROP INDEX IF EXISTS idx_consents_conference_type;
ALTER TABLE meeting_consents
    DROP CONSTRAINT IF EXISTS ck_consent_withdrawn,
    DROP COLUMN IF EXISTS updated_at,
    DROP COLUMN IF EXISTS request_id,
    DROP COLUMN IF EXISTS source,
    DROP COLUMN IF EXISTS withdrawn_at;
DELETE FROM meeting_consents WHERE consent_type = 'DATA_SHARING';
ALTER TABLE meeting_consents DROP CONSTRAINT IF EXISTS meeting_consents_consent_type_check;
ALTER TABLE meeting_consents ADD CONSTRAINT meeting_consents_consent_type_check
    CHECK (consent_type IN ('RECORDING','TRANSCRIPTION','AI_PROCESSING','HEALTHCARE_TERMS','PRIVACY_NOTICE'));

DROP INDEX IF EXISTS idx_artifact_conference_type;
DROP INDEX IF EXISTS idx_artifact_deletion_due;
DROP INDEX IF EXISTS idx_artifact_lifecycle;
DROP INDEX IF EXISTS uq_artifact_idempotency;
DROP INDEX IF EXISTS uq_artifact_provider_ref;
DROP INDEX IF EXISTS uq_artifact_active_recording;
ALTER TABLE meeting_artifacts
    DROP CONSTRAINT IF EXISTS ck_artifact_deleted_confirmed,
    DROP CONSTRAINT IF EXISTS ck_artifact_ready_validated,
    DROP CONSTRAINT IF EXISTS uq_artifact_id_conference;
UPDATE meeting_artifacts
   SET storage_state = CASE status WHEN 'READY' THEN 'AVAILABLE'
                                   WHEN 'PROCESSING' THEN 'PROCESSING'
                                   WHEN 'FAILED' THEN 'FAILED'
                                   WHEN 'DELETED' THEN 'DELETED'
                                   ELSE 'PENDING' END;
ALTER TABLE meeting_artifacts
    DROP COLUMN IF EXISTS version,
    DROP COLUMN IF EXISTS source_artifact_id,
    DROP COLUMN IF EXISTS idempotency_key,
    DROP COLUMN IF EXISTS request_id,
    DROP COLUMN IF EXISTS verified_at,
    DROP COLUMN IF EXISTS locked_until,
    DROP COLUMN IF EXISTS locked_by,
    DROP COLUMN IF EXISTS next_attempt_at,
    DROP COLUMN IF EXISTS attempts,
    DROP COLUMN IF EXISTS failure_detail,
    DROP COLUMN IF EXISTS failure_code,
    DROP COLUMN IF EXISTS delete_reason,
    DROP COLUMN IF EXISTS delete_requested_at,
    DROP COLUMN IF EXISTS stop_requested_at,
    DROP COLUMN IF EXISTS finalized_at,
    DROP COLUMN IF EXISTS ended_at,
    DROP COLUMN IF EXISTS started_at,
    DROP COLUMN IF EXISTS retention_at,
    DROP COLUMN IF EXISTS metadata,
    DROP COLUMN IF EXISTS layout,
    DROP COLUMN IF EXISTS duration_ms,
    DROP COLUMN IF EXISTS content_type,
    DROP COLUMN IF EXISTS storage_key,
    DROP COLUMN IF EXISTS storage_bucket,
    DROP COLUMN IF EXISTS status;
DELETE FROM meeting_artifacts
 WHERE artifact_type IN ('CAPTION_TRACK','POLL_REPORT','MEETING_SUMMARY','WHITEBOARD_SNAPSHOT');
ALTER TABLE meeting_artifacts DROP CONSTRAINT IF EXISTS meeting_artifacts_artifact_type_check;
ALTER TABLE meeting_artifacts ADD CONSTRAINT meeting_artifacts_artifact_type_check
    CHECK (artifact_type IN ('RECORDING','TRANSCRIPT','CHAT_EXPORT','ATTENDANCE_REPORT','WHITEBOARD','SUMMARY'));
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

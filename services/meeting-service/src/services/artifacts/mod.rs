//! Phase 2 Part G — recording, artefacts & intelligence readiness (internal modules of
//! the meeting-service; none of these is a deployable service).
//!
//! Authorities: PostgreSQL owns artefact state, LiveKit Egress only executes captures,
//! object storage only holds bytes, Redis / WebSocket only deliver events.
//!
//! - [`storage`]: `ArtifactStorage` (S3-compatible; opaque keys; signed reads).
//! - [`recorder`]: `RecordingProvider` (LiveKit Egress composite capture).
//! - [`providers`]: `TranscriptionProvider`, `TranslationProvider`,
//!   `IntelligenceProvider` (provider-neutral, location-aware).
//! - [`recording`]: `RecordingService` (state machine, commands, provider evidence,
//!   finalization, reconciliation).
//! - [`service`]: `ArtifactService` (list/get/authorize/signed access/delete/reports).
//! - [`consent`]: `ConsentService` (independent consent types, re-evaluation,
//!   withdrawal semantics).
//! - [`retention`]: `ArtifactRetentionWorker` (+ finalization retries).
//! - [`transcription`]: `TranscriptionService` (jobs, segments, captions, translation).
//! - [`intelligence`]: `IntelligenceService` (policy-gated summary readiness).

pub mod consent;
pub mod intelligence;
pub mod providers;
pub mod recorder;
pub mod recording;
pub mod retention;
pub mod service;
pub mod storage;
pub mod transcription;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::{AdmissionState, RecordOrigin, Role},
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, conference_sessions, meeting_artifacts, meeting_spaces},
    services::{
        actor::{ActorContext, ActorKind},
        meeting_core::{
            conference_authority, find_conference, find_space, not_found_conference,
            permission_denied, Core,
        },
        meeting_events::AuditActor,
        meeting_permissions::{Permission, PermissionContext},
        profiles::{MeetingProfileResolver, ResolvedProfile},
    },
};

use self::{
    providers::{
        IntelligenceProvider, TranscriptionProvider, TranslationProvider,
        UnconfiguredIntelligenceProvider, UnconfiguredTranscriptionProvider,
        UnconfiguredTranslationProvider,
    },
    recorder::{RecordingProvider, UnconfiguredRecorder},
    storage::{ArtifactStorage, UnconfiguredStorage},
};

/// `SELECT … FOR SHARE` on the meeting space: serializes capture starts with
/// profile-policy updates (which take `FOR UPDATE`) without serializing starts with
/// each other. Callers lock the space before the conference (global lock order).
pub(crate) async fn lock_space_shared<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
) -> ApiResult<meeting_spaces::Model> {
    use sea_orm::{EntityTrait, QuerySelect};
    meeting_spaces::Entity::find_by_id(id)
        .lock(sea_orm::sea_query::LockType::Share)
        .one(conn)
        .await?
        .ok_or_else(|| ApiError::domain(ErrorCode::MeetingNotFound, "Meeting not found"))
}

/// Provider backends (deployments and tests replace the defaults).
#[derive(Clone)]
pub struct ArtifactBackends {
    pub recorder: Arc<dyn RecordingProvider>,
    pub storage: Arc<dyn ArtifactStorage>,
    pub transcription: Arc<dyn TranscriptionProvider>,
    pub translation: Arc<dyn TranslationProvider>,
    pub intelligence: Arc<dyn IntelligenceProvider>,
}

impl Default for ArtifactBackends {
    fn default() -> Self {
        Self {
            recorder: Arc::new(UnconfiguredRecorder),
            storage: Arc::new(UnconfiguredStorage),
            transcription: Arc::new(UnconfiguredTranscriptionProvider),
            translation: Arc::new(UnconfiguredTranslationProvider),
            intelligence: Arc::new(UnconfiguredIntelligenceProvider),
        }
    }
}

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
        pub enum $name { $(#[serde(rename = $s)] $variant),+ }
        // Hand-written schema: `#[serde(rename = $s)]` inside a macro is invisible to the
        // `ToSchema` derive, which would otherwise publish the Rust variant names instead
        // of the wire values (Part J contract defect).
        impl<'__s> utoipa::ToSchema<'__s> for $name {
            fn schema() -> (
                &'__s str,
                utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
            ) {
                (
                    stringify!($name),
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::SchemaType::String)
                        .enum_values(Some([$($s),+]))
                        .into(),
                )
            }
        }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub fn as_str(&self) -> &'static str { match self { $($name::$variant => $s),+ } }
            pub fn parse(v: &str) -> Option<Self> { match v { $($s => Some($name::$variant),)+ _ => None } }
        }
    };
}

str_enum!(
    /// Unified artefact types (Part G §23). Not every generator is active yet.
    ArtifactType {
        Recording = "RECORDING",
        Transcript = "TRANSCRIPT",
        CaptionTrack = "CAPTION_TRACK",
        AttendanceReport = "ATTENDANCE_REPORT",
        ChatExport = "CHAT_EXPORT",
        PollReport = "POLL_REPORT",
        MeetingSummary = "MEETING_SUMMARY",
        WhiteboardSnapshot = "WHITEBOARD_SNAPSHOT",
    }
);

str_enum!(
    /// Authoritative artefact / recording lifecycle (Part G §6).
    ArtifactStatus {
        Requested = "REQUESTED",
        Recording = "RECORDING",
        Stopping = "STOPPING",
        Processing = "PROCESSING",
        Ready = "READY",
        Failed = "FAILED",
        Deleted = "DELETED",
    }
);

impl ArtifactStatus {
    /// Capture still in progress (one per conference for composite recording).
    pub fn is_active_capture(&self) -> bool {
        matches!(self, Self::Requested | Self::Recording | Self::Stopping)
    }

    /// Terminal states: nothing leaves DELETED; FAILED / READY only to DELETED.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Deleted)
    }

    /// The authoritative artefact lifecycle (Part G §6):
    ///
    /// ```text
    /// REQUESTED → RECORDING → STOPPING → PROCESSING → READY → DELETED
    ///     │           │          │           │
    ///     └───────────┴──────────┴───────────┴──→ FAILED → DELETED
    /// ```
    ///
    /// REQUESTED may go straight to STOPPING (stop before provider evidence) and
    /// REQUESTED / RECORDING straight to PROCESSING (provider completion evidence without
    /// a prior stop). READY is reachable only from PROCESSING (after storage
    /// verification); an active capture is never deleted.
    pub fn can_transition_to(&self, next: ArtifactStatus) -> bool {
        use ArtifactStatus::*;
        matches!(
            (self, next),
            (Requested, Recording | Stopping | Processing | Failed)
                | (Recording, Stopping | Processing | Failed)
                | (Stopping, Processing | Failed)
                | (Processing, Ready | Failed | Deleted)
                | (Ready, Deleted)
                | (Failed, Deleted)
        )
    }

    /// Legacy `storage_state` kept in sync for Part B readers.
    pub fn storage_state(&self) -> &'static str {
        match self {
            Self::Requested | Self::Recording | Self::Stopping => "PENDING",
            Self::Processing => "PROCESSING",
            Self::Ready => "AVAILABLE",
            Self::Failed => "FAILED",
            Self::Deleted => "DELETED",
        }
    }
}

pub fn status_of(a: &meeting_artifacts::Model) -> ArtifactStatus {
    ArtifactStatus::parse(&a.status).unwrap_or(ArtifactStatus::Failed)
}

pub fn type_of(a: &meeting_artifacts::Model) -> Option<ArtifactType> {
    ArtifactType::parse(&a.artifact_type)
}

/// Public artefact projection: never exposes storage keys, provider references,
/// internal diagnostics or signed URLs.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactView {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub artifact_type: ArtifactType,
    pub status: ArtifactStatus,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_ms: Option<i64>,
    pub layout: Option<String>,
    pub checksum_sha256: Option<String>,
    /// Server-generated, type-specific metadata.
    #[schema(value_type = Object)]
    pub metadata: Value,
    pub retention_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub finalized_at: Option<DateTime<Utc>>,
    /// Public-safe failure category (internal diagnostics are never returned).
    pub failure_code: Option<String>,
    pub source_artifact_id: Option<Uuid>,
    /// Deletion requested; no new access is issued.
    pub deletion_pending: bool,
}

impl ArtifactView {
    pub fn of(a: &meeting_artifacts::Model) -> Self {
        Self {
            id: a.id,
            conference_id: a.conference_id,
            artifact_type: type_of(a).unwrap_or(ArtifactType::Recording),
            status: status_of(a),
            content_type: a.content_type.clone(),
            size_bytes: a.size_bytes,
            duration_ms: a.duration_ms,
            layout: a.layout.clone(),
            checksum_sha256: a.checksum_sha256.clone(),
            metadata: a.metadata.clone(),
            retention_at: a.retention_at,
            created_at: a.created_at,
            updated_at: a.updated_at,
            started_at: a.started_at,
            ended_at: a.ended_at,
            finalized_at: a.finalized_at,
            failure_code: a.failure_code.clone(),
            source_artifact_id: a.source_artifact_id,
            deletion_pending: a.delete_requested_at.is_some()
                && status_of(a) != ArtifactStatus::Deleted,
        }
    }
}

/// Everything an artefact action needs, resolved from authoritative state. Works for
/// LIVE and historical (ENDED) conferences: authority comes from the persistent
/// participant role or space role, never from knowing an id.
#[derive(Debug, Clone)]
pub struct ArtifactCtx {
    pub conference: conference_sessions::Model,
    pub space: meeting_spaces::Model,
    pub pctx: PermissionContext,
    pub role: Role,
    pub participant: Option<conference_participants::Model>,
    pub audit: AuditActor,
    pub profile: ResolvedProfile,
}

impl ArtifactCtx {
    pub async fn load(core: &Core, ctx: &ActorContext, conference_id: Uuid) -> ApiResult<Self> {
        if matches!(ctx.kind, ActorKind::Anonymous | ActorKind::LegacyGuest(_)) {
            return Err(ApiError::domain(
                ErrorCode::AuthenticationRequired,
                "Sign in or present your participant credential",
            ));
        }
        let conference = find_conference(&core.db, conference_id).await?;
        if conference.origin == RecordOrigin::Legacy {
            return Err(not_found_conference());
        }
        let space = find_space(&core.db, conference.meeting_space_id).await?;
        let authority = conference_authority(&core.db, &space, conference_id, ctx)
            .await?
            .ok_or_else(|| permission_denied("You are not a participant of this conference"))?;
        let profile = MeetingProfileResolver::load(&core.db, &space, Some(&conference)).await?;
        Ok(Self {
            pctx: PermissionContext::for_space(&space),
            role: authority.role,
            participant: authority
                .participant
                .filter(|p| p.admission_state == AdmissionState::Admitted),
            audit: authority.audit,
            conference,
            space,
            profile,
        })
    }

    pub fn allows(&self, p: Permission) -> bool {
        self.pctx.allows(self.role, p)
    }

    pub fn require(&self, p: Permission, message: &str) -> ApiResult<()> {
        if self.allows(p) {
            Ok(())
        } else {
            Err(permission_denied(message))
        }
    }

    pub fn me(&self) -> ApiResult<&conference_participants::Model> {
        self.participant
            .as_ref()
            .ok_or_else(|| permission_denied("Only admitted participants can do this"))
    }
}

pub fn not_found_artifact() -> ApiError {
    ApiError::domain(ErrorCode::ArtifactNotFound, "Artifact not found")
}

pub fn storage_unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::ArtifactStorageUnavailable,
        "Artifact storage is temporarily unavailable",
    )
}

/// Conference counters maintained for operations (Part I aggregates). Only the listed
/// columns can be incremented (no dynamic SQL from callers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConferenceCounter {
    JoinAttempts,
    JoinSuccesses,
    JoinMediaConnected,
    JoinDenials,
    JoinProviderFailures,
    JoinTimeouts,
    JoinClientFailures,
    Reconnects,
    PublishFailures,
    SubscribeFailures,
    ProviderDrift,
    RecordingFailures,
    TranscriptionFailures,
}

impl ConferenceCounter {
    fn column(&self) -> &'static str {
        match self {
            Self::JoinAttempts => "join_attempts",
            Self::JoinSuccesses => "join_successes",
            Self::JoinMediaConnected => "join_media_connected",
            Self::JoinDenials => "join_denials",
            Self::JoinProviderFailures => "join_provider_failures",
            Self::JoinTimeouts => "join_timeouts",
            Self::JoinClientFailures => "join_client_failures",
            Self::Reconnects => "reconnect_count",
            Self::PublishFailures => "publish_failure_count",
            Self::SubscribeFailures => "subscribe_failure_count",
            Self::ProviderDrift => "provider_drift_count",
            Self::RecordingFailures => "recording_failure_count",
            Self::TranscriptionFailures => "transcription_failure_count",
        }
    }
}

/// Atomic, relational, low-frequency counter increment (never per raw sample).
pub async fn bump<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    counter: ConferenceCounter,
    by: i64,
) -> ApiResult<()> {
    let column = counter.column();
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("UPDATE conference_sessions SET {column} = {column} + $2 WHERE id = $1"),
        [conference_id.into(), by.into()],
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_lifecycle_transition_table() {
        use ArtifactStatus::*;
        let allowed = [
            (Requested, Recording),
            (Requested, Stopping),
            (Requested, Processing),
            (Requested, Failed),
            (Recording, Stopping),
            (Recording, Processing),
            (Recording, Failed),
            (Stopping, Processing),
            (Stopping, Failed),
            (Processing, Ready),
            (Processing, Failed),
            (Processing, Deleted),
            (Ready, Deleted),
            (Failed, Deleted),
        ];
        for from in ArtifactStatus::ALL {
            for to in ArtifactStatus::ALL {
                assert_eq!(
                    from.can_transition_to(*to),
                    allowed.contains(&(*from, *to)),
                    "{from:?} → {to:?}"
                );
            }
        }
        // READY only after processing (storage verification), never from a capture state.
        for s in [Requested, Recording, Stopping] {
            assert!(!s.can_transition_to(Ready));
            assert!(
                !s.can_transition_to(Deleted),
                "active capture is never deleted"
            );
        }
        assert!(Deleted.is_terminal());
        assert!(ArtifactStatus::ALL
            .iter()
            .all(|s| !Deleted.can_transition_to(*s)));
        assert!(
            !Ready.can_transition_to(Failed),
            "a verified object is not re-failed"
        );
    }

    #[test]
    fn statuses_map_to_legacy_storage_state() {
        assert_eq!(ArtifactStatus::Ready.storage_state(), "AVAILABLE");
        assert_eq!(ArtifactStatus::Recording.storage_state(), "PENDING");
        assert!(ArtifactStatus::Stopping.is_active_capture());
        assert!(!ArtifactStatus::Processing.is_active_capture());
        for t in ArtifactType::ALL {
            assert_eq!(ArtifactType::parse(t.as_str()), Some(*t));
        }
    }
}

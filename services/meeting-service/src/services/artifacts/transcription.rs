//! TranscriptionService — transcript / caption architecture (Part G §29–§33).
//!
//! - Jobs (`transcription_jobs`): LIVE (one active per conference) or POST_MEETING
//!   (from a READY recording). Started only after flag, permission, profile policy,
//!   provider-location policy and consent gates pass; the provider is called after the
//!   job commits. ACTIVE only on provider confirmation.
//! - Segments: PARTIAL segments are ephemeral (transient realtime only); FINAL
//!   segments are persisted once (`UNIQUE (job_id, provider_segment_id)`), so provider
//!   retries never duplicate them. Speakers are mapped from the server-generated media
//!   identity to an internal participant of the SAME conference — raw provider speaker
//!   ids are never stored or exposed and a forged identity maps to nothing.
//! - Captions are a presentation of transcript segments (`caption.segment`); turning
//!   captions off on a client never disables transcription, and captions cannot exist
//!   without an authorized transcription.
//! - On completion the transcript (JSON) and a caption track (WebVTT) are stored as
//!   READY artefacts. Translation is downstream and never overwrites the original.

use chrono::Utc;
use hmac::{Hmac, Mac};
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction,
    DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    bump, not_found_artifact,
    providers::{
        location_permitted, LiveTranscriptionRequest, PostMeetingTranscriptionRequest,
        ProviderError, ProviderJob, ProviderJobStatus, TranscriptionMode,
    },
    service::ArtifactService,
    status_of, ArtifactCtx, ArtifactStatus, ArtifactType, ArtifactView, ConferenceCounter,
};
use crate::{
    domain::ConferenceStatus,
    error::{ApiError, ApiResult, ErrorCode},
    models::{
        conference_sessions, meeting_artifacts, participant_sessions, transcript_segments,
        transcription_jobs,
    },
    services::{
        actor::ActorContext,
        collaboration::{policy::ConsentKind, publish_transient},
        meeting_core::{
            find_conference, is_unique_violation, lock_conference, permission_denied, Core,
        },
        meeting_events::{action, audit, emit, event, AuditActor, AuditRecord, OutboxRecord},
        meeting_permissions::Permission,
        ops_metrics::{inc, OPS_METRICS as OM},
        quality::{
            incidents::{self, kind, Incident, IncidentAction},
            sink::PipelineStage,
        },
        rtc_provider::RtcRoomRef,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TranscriptionModeDto {
    Live,
    PostMeeting,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartTranscriptionRequest {
    pub mode: Option<TranscriptionModeDto>,
    /// BCP-47-like language tag (e.g. `en`, `en-GB`).
    pub language: Option<String>,
    /// POST_MEETING: the READY recording to transcribe.
    pub source_recording_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranslateRequest {
    pub target_language: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionJobView {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub artifact_id: Uuid,
    pub mode: String,
    pub status: String,
    pub language: String,
    pub segment_count: i32,
    pub failure_code: Option<String>,
    pub started_at: Option<chrono::DateTime<Utc>>,
    pub ended_at: Option<chrono::DateTime<Utc>>,
}

impl TranscriptionJobView {
    pub fn of(j: &transcription_jobs::Model) -> Self {
        Self {
            id: j.id,
            conference_id: j.conference_id,
            artifact_id: j.artifact_id,
            mode: j.mode.clone(),
            status: j.status.clone(),
            language: j.language.clone(),
            segment_count: j.segment_count,
            failure_code: j.failure_code.clone(),
            started_at: j.started_at,
            ended_at: j.ended_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SegmentView {
    pub sequence: i64,
    pub speaker_participant_id: Option<Uuid>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub language: String,
    pub text: String,
    pub confidence: Option<f32>,
}

impl SegmentView {
    fn of(s: &transcript_segments::Model) -> Self {
        Self {
            sequence: s.sequence,
            speaker_participant_id: s.speaker_participant_id,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            language: s.language.clone(),
            text: s.text.clone(),
            confidence: s.confidence,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptView {
    pub artifact: ArtifactView,
    pub job: Option<TranscriptionJobView>,
    pub segments: Vec<SegmentView>,
    /// Cursor for the next page (`after` = last sequence).
    pub next_after: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranslationResponse {
    pub artifact_id: Uuid,
    pub target_language: String,
    pub translated_segments: u64,
}

/// Provider-neutral callback payload (`POST /internal/transcription/events`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", tag = "type", deny_unknown_fields)]
pub enum ProviderTranscriptionEvent {
    #[serde(rename = "JOB_STATUS", rename_all = "camelCase")]
    JobStatus {
        job_ref: String,
        status: String,
        #[serde(default)]
        error: Option<String>,
    },
    #[serde(rename = "SEGMENT", rename_all = "camelCase")]
    Segment {
        job_ref: String,
        segment_id: String,
        sequence: i64,
        /// Server-generated media identity of the speaker (never trusted beyond a
        /// same-conference lookup).
        #[serde(default)]
        speaker_identity: Option<String>,
        start_ms: i64,
        end_ms: i64,
        language: String,
        text: String,
        #[serde(default)]
        confidence: Option<f32>,
        #[serde(rename = "final")]
        is_final: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    Applied,
    Duplicate,
    Late,
    Unknown,
}

impl IngestOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Applied => "APPLIED",
            Self::Duplicate => "DUPLICATE",
            Self::Late => "LATE",
            Self::Unknown => "UNKNOWN",
        }
    }
}

type HmacSha256 = Hmac<Sha256>;

/// Verify a provider callback: `X-CF-Signature: sha256=<hex(HMAC(secret, ts + "." + body))>`
/// with `X-CF-Timestamp` within 5 minutes.
pub fn verify_callback(secret: &str, timestamp: &str, body: &str, signature: &str) -> bool {
    let Ok(ts) = timestamp.parse::<i64>() else {
        return false;
    };
    if (Utc::now().timestamp() - ts).abs() > 300 {
        return false;
    }
    let Some(hex_sig) = signature.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(format!("{timestamp}.{body}").as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// Sign a callback (provider adapters / tests).
pub fn sign_callback(secret: &str, timestamp: &str, body: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(format!("{timestamp}.{body}").as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn valid_language(l: &str) -> bool {
    !l.is_empty() && l.len() <= 16 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn disabled() -> ApiError {
    ApiError::domain(
        ErrorCode::TranscriptionDisabled,
        "Transcription is not enabled",
    )
}

fn not_allowed(message: &str) -> ApiError {
    ApiError::domain(ErrorCode::TranscriptionNotAllowed, message)
}

fn provider_unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::TranscriptionProviderUnavailable,
        "Transcription is temporarily unavailable",
    )
}

fn job_event<'a>(j: &transcription_jobs::Model, space_id: Uuid, t: &'a str) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "transcription",
        aggregate_id: j.id,
        event_type: t,
        meeting_space_id: Some(space_id),
        conference_id: Some(j.conference_id),
        payload: json!({
            "conferenceId": j.conference_id,
            "jobId": j.id,
            "artifactId": j.artifact_id,
            "mode": j.mode,
            "status": j.status,
            "language": j.language,
            "failureCode": j.failure_code,
        }),
    }
}

async fn set_state<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    state: &str,
) -> ApiResult<()> {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE conference_sessions SET transcription_state = $2, updated_at = now() WHERE id = $1",
        [conference_id.into(), state.into()],
    ))
    .await?;
    Ok(())
}

fn vtt_time(ms: i64) -> String {
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    format!("{h:02}:{m:02}:{s:02}.{:03}", ms % 1000)
}

/// WebVTT caption track from FINAL segments (presentation of the transcript).
pub fn to_webvtt(segments: &[transcript_segments::Model]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for (i, s) in segments.iter().enumerate() {
        let text = s.text.replace("-->", "->").replace('\n', " ");
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            vtt_time(s.start_ms),
            vtt_time(s.end_ms),
            text
        ));
    }
    out
}

pub struct TranscriptionService {
    core: Core,
}

impl TranscriptionService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    pub async fn start(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: &StartTranscriptionRequest,
    ) -> ApiResult<TranscriptionJobView> {
        if !self.core.flags.transcription_v2_enabled {
            return Err(disabled());
        }
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        if !c.allows(Permission::TranscriptionStart) {
            return Err(
                if c.pctx.transcription_policy == crate::domain::MediaCapturePolicy::Disabled {
                    not_allowed("Transcription is disabled for this meeting")
                } else {
                    permission_denied("You are not allowed to start transcription")
                },
            );
        }
        let policy = &c.profile.policy.transcription;
        if !policy.allowed {
            inc(&OM.profile_policy_denials);
            return Err(not_allowed(
                "Transcription is not permitted by this meeting's policy",
            ));
        }
        let provider = self.core.artifacts.transcription.clone();
        let caps = provider.capabilities();
        // Policy over external / self-hosted processing: no provider call when the
        // provider's location is not permitted.
        if caps.location.is_some() && !location_permitted(policy.location, caps.location) {
            inc(&OM.profile_policy_denials);
            return Err(not_allowed(
                "The configured transcription provider is not permitted by policy",
            ));
        }
        let mode = match req.mode.unwrap_or(TranscriptionModeDto::Live) {
            TranscriptionModeDto::Live => TranscriptionMode::Live,
            TranscriptionModeDto::PostMeeting => TranscriptionMode::PostMeeting,
        };
        let supported = match mode {
            TranscriptionMode::Live => caps.live,
            TranscriptionMode::PostMeeting => caps.post_meeting,
        };
        if !supported || caps.location.is_none() {
            return Err(provider_unavailable());
        }
        let language = req.language.clone().unwrap_or_else(|| "en".into());
        if !valid_language(&language) {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "invalid language",
            ));
        }
        let consent = super::consent::ConsentService::new(self.core.clone());
        let source = match mode {
            TranscriptionMode::Live => {
                if c.conference.status != ConferenceStatus::Live {
                    return Err(ApiError::domain(
                        ErrorCode::ConferenceNotLive,
                        "Live transcription requires a live conference",
                    ));
                }
                consent
                    .require_for_processing(&c, ConsentKind::Transcription)
                    .await?;
                None
            }
            TranscriptionMode::PostMeeting => {
                let rid = req.source_recording_id.ok_or_else(|| {
                    ApiError::domain(ErrorCode::ValidationError, "sourceRecordingId is required")
                })?;
                let rec = meeting_artifacts::Entity::find_by_id(rid)
                    .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
                    .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
                    .one(&self.core.db)
                    .await?
                    .ok_or_else(not_found_artifact)?;
                if status_of(&rec) != ArtifactStatus::Ready || rec.delete_requested_at.is_some() {
                    return Err(ApiError::domain(
                        ErrorCode::ArtifactNotReady,
                        "The source recording is not ready",
                    ));
                }
                consent
                    .require_for_post_meeting(&c, ConsentKind::Transcription)
                    .await?;
                Some(rec)
            }
        };
        let job_id = Uuid::new_v4();
        let artifact_id = Uuid::new_v4();
        let now = Utc::now();
        let txn = self.core.db.begin().await?;
        let space = super::lock_space_shared(&txn, c.space.id).await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let current_profile = crate::services::profiles::MeetingProfileResolver::load(
            &txn,
            &space,
            Some(&conference),
        )
        .await?;
        if !current_profile.policy.transcription.allowed {
            inc(&OM.profile_policy_denials);
            return Err(not_allowed(
                "Transcription is not permitted by this meeting's policy",
            ));
        }
        // Re-check explicit consent under the conference lock (withdrawals serialize on
        // the same lock), so a concurrent withdrawal can never be bypassed.
        if mode == TranscriptionMode::Live
            && super::consent::mode_for(&current_profile, ConsentKind::Transcription)
                == crate::services::profiles::ConsentMode::ExplicitConsentRequired
        {
            let version = super::consent::ConsentService::version(&c);
            let (missing, _) = super::consent::missing_consent(
                &txn,
                conference_id,
                ConsentKind::Transcription,
                &version,
                true,
            )
            .await?;
            if !missing.is_empty() {
                return Err(ApiError::domain(
                    ErrorCode::ConsentRequired,
                    "Transcription consent changed; every participant must consent",
                ));
            }
        }
        insert_pending_artifact(
            &txn,
            &conference,
            artifact_id,
            ArtifactType::Transcript,
            source.as_ref().map(|r| r.id),
            &c,
            &language,
        )
        .await?;
        let job = transcription_jobs::ActiveModel {
            id: Set(job_id),
            conference_id: Set(conference_id),
            artifact_id: Set(artifact_id),
            mode: Set(mode.as_str().into()),
            status: Set("REQUESTED".into()),
            provider: Set(provider.name().into()),
            provider_job_ref: Set(None),
            language: Set(language.clone()),
            source_artifact_id: Set(source.as_ref().map(|r| r.id)),
            requested_by: Set(c.audit.actor_ref.clone()),
            segment_count: Set(0),
            failure_code: Set(None),
            started_at: Set(None),
            stop_requested_at: Set(None),
            ended_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            version: Set(1),
        };
        let job = match job.insert(&txn).await {
            Ok(j) => j,
            Err(e) if is_unique_violation(&e) => {
                return Err(ApiError::domain(
                    ErrorCode::ConferenceStateConflict,
                    "Live transcription is already active for this conference",
                ))
            }
            Err(e) => return Err(e.into()),
        };
        if mode == TranscriptionMode::Live {
            set_state(&txn, conference_id, "REQUESTED").await?;
        }
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: c.space.id,
                conference_id: Some(conference_id),
                action: action::TRANSCRIPTION_START,
                target_type: Some("TRANSCRIPTION"),
                target_ref: Some(job_id.to_string()),
                metadata: json!({ "mode": mode.as_str(), "language": language }),
            },
        )
        .await?;
        txn.commit().await?;
        let _ = self
            .core
            .telemetry
            .record_transcription(PipelineStage::Requested, None);

        let result = match mode {
            TranscriptionMode::Live => {
                provider
                    .start_live_transcription(&LiveTranscriptionRequest {
                        job_id,
                        conference_id,
                        room_name: RtcRoomRef::for_conference(&c.conference).room_name,
                        language: language.clone(),
                    })
                    .await
            }
            TranscriptionMode::PostMeeting => {
                let rec = source.expect("checked");
                let url = self
                    .core
                    .artifacts
                    .storage
                    .signed_read_url(
                        rec.storage_key.as_deref().unwrap_or_default(),
                        std::time::Duration::from_secs(3600),
                        None,
                    )
                    .map_err(|_| super::storage_unavailable())?;
                provider
                    .submit_post_meeting_transcription(&PostMeetingTranscriptionRequest {
                        job_id,
                        conference_id,
                        source_url: url.url,
                        language: language.clone(),
                    })
                    .await
            }
        };
        match result {
            Ok(pj) => {
                self.attach(&job, &pj).await?;
            }
            Err(e) => {
                tracing::warn!(job_id = %job_id, "transcription provider start failed: {e}");
                self.fail(job.id, "PROVIDER_UNAVAILABLE").await?;
                return Err(provider_unavailable());
            }
        }
        let job = self.job(conference_id, job_id).await?;
        Ok(TranscriptionJobView::of(&job))
    }

    async fn job(&self, conference_id: Uuid, job_id: Uuid) -> ApiResult<transcription_jobs::Model> {
        transcription_jobs::Entity::find_by_id(job_id)
            .filter(transcription_jobs::Column::ConferenceId.eq(conference_id))
            .one(&self.core.db)
            .await?
            .ok_or_else(|| {
                ApiError::domain(
                    ErrorCode::TranscriptionNotFound,
                    "Transcription job not found",
                )
            })
    }

    async fn attach(&self, job: &transcription_jobs::Model, pj: &ProviderJob) -> ApiResult<()> {
        self.core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE transcription_jobs SET provider_job_ref = $2, updated_at = now()
                  WHERE id = $1 AND provider_job_ref IS NULL",
                [job.id.into(), pj.provider_job_ref.clone().into()],
            ))
            .await?;
        self.apply_status(job.id, pj.status, None).await?;
        Ok(())
    }

    pub async fn stop(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        job_id: Uuid,
    ) -> ApiResult<TranscriptionJobView> {
        if !self.core.flags.transcription_v2_enabled {
            return Err(disabled());
        }
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::TranscriptionStop,
            "You are not allowed to stop transcription",
        )?;
        let job = self.job(conference_id, job_id).await?;
        self.stop_job(&job, &c.audit, "HOST_STOP").await?;
        Ok(TranscriptionJobView::of(
            &self.job(conference_id, job_id).await?,
        ))
    }

    /// System stop (consent withdrawal / policy). Returns whether a job stopped.
    pub async fn system_stop(&self, conference_id: Uuid, reason: &str) -> ApiResult<bool> {
        let Some(job) = transcription_jobs::Entity::find()
            .filter(transcription_jobs::Column::ConferenceId.eq(conference_id))
            .filter(transcription_jobs::Column::Mode.eq("LIVE"))
            .filter(transcription_jobs::Column::Status.is_in(["REQUESTED", "ACTIVE"]))
            .one(&self.core.db)
            .await?
        else {
            return Ok(false);
        };
        self.stop_job(&job, &AuditActor::system("transcription-policy"), reason)
            .await?;
        Ok(true)
    }

    async fn stop_job(
        &self,
        job: &transcription_jobs::Model,
        actor: &AuditActor,
        reason: &str,
    ) -> ApiResult<()> {
        if !matches!(job.status.as_str(), "REQUESTED" | "ACTIVE") {
            return Ok(());
        }
        let conference = find_conference(&self.core.db, job.conference_id).await?;
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, job.conference_id).await?;
        let current = lock_job(&txn, job.id).await?;
        if !matches!(current.status.as_str(), "REQUESTED" | "ACTIVE") {
            txn.commit().await?;
            return Ok(());
        }
        let mut am: transcription_jobs::ActiveModel = current.clone().into();
        am.status = Set("STOPPING".into());
        am.stop_requested_at = Set(Some(Utc::now()));
        am.version = Set(current.version + 1);
        am.updated_at = Set(Utc::now());
        am.update(&txn).await?;
        set_state(&txn, job.conference_id, "STOPPING").await?;
        audit(
            &txn,
            actor,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(job.conference_id),
                action: action::TRANSCRIPTION_STOP,
                target_type: Some("TRANSCRIPTION"),
                target_ref: Some(job.id.to_string()),
                metadata: json!({ "reason": reason }),
            },
        )
        .await?;
        txn.commit().await?;
        let provider = self.core.artifacts.transcription.clone();
        if let Some(r) = current.provider_job_ref.as_deref() {
            match provider.stop_live_transcription(r).await {
                Ok(pj) => {
                    self.apply_status(job.id, pj.status, None).await?;
                }
                Err(e) => {
                    tracing::warn!(job_id = %job.id, "provider stop failed (completion awaited): {e}")
                }
            }
        } else {
            self.fail(job.id, "STOPPED_BEFORE_START").await?;
        }
        Ok(())
    }

    /// Provider job status (callback / response / reconciler query).
    pub async fn apply_status(
        &self,
        job_id: Uuid,
        status: ProviderJobStatus,
        error: Option<String>,
    ) -> ApiResult<IngestOutcome> {
        let Some(job) = transcription_jobs::Entity::find_by_id(job_id)
            .one(&self.core.db)
            .await?
        else {
            return Ok(IngestOutcome::Unknown);
        };
        match status {
            ProviderJobStatus::Active => {
                let conference = find_conference(&self.core.db, job.conference_id).await?;
                let txn = self.core.db.begin().await?;
                lock_conference(&txn, job.conference_id).await?;
                let current = lock_job(&txn, job_id).await?;
                if current.status != "REQUESTED" {
                    txn.commit().await?;
                    return Ok(IngestOutcome::Duplicate);
                }
                let mut am: transcription_jobs::ActiveModel = current.clone().into();
                am.status = Set("ACTIVE".into());
                am.started_at = Set(Some(Utc::now()));
                am.version = Set(current.version + 1);
                am.updated_at = Set(Utc::now());
                let updated = am.update(&txn).await?;
                if updated.mode == "LIVE" {
                    set_state(&txn, job.conference_id, "ACTIVE").await?;
                }
                emit(
                    &txn,
                    job_event(
                        &updated,
                        conference.meeting_space_id,
                        event::TRANSCRIPTION_STARTED,
                    ),
                )
                .await?;
                txn.commit().await?;
                inc(&OM.transcription_started);
                let _ = self
                    .core
                    .telemetry
                    .record_transcription(PipelineStage::Started, None);
                Ok(IngestOutcome::Applied)
            }
            ProviderJobStatus::Completed => self.complete(job_id).await,
            ProviderJobStatus::Failed | ProviderJobStatus::Cancelled => {
                if matches!(job.status.as_str(), "READY" | "FAILED" | "CANCELLED") {
                    return Ok(IngestOutcome::Duplicate);
                }
                if let Some(e) = error {
                    tracing::warn!(job_id = %job_id, "transcription provider failure: {e}");
                }
                self.fail(
                    job_id,
                    if status == ProviderJobStatus::Failed {
                        "PROVIDER_FAILED"
                    } else {
                        "PROVIDER_CANCELLED"
                    },
                )
                .await?;
                Ok(IngestOutcome::Applied)
            }
            ProviderJobStatus::Pending => Ok(IngestOutcome::Duplicate),
        }
    }

    async fn fail(&self, job_id: Uuid, code: &str) -> ApiResult<()> {
        let Some(job) = transcription_jobs::Entity::find_by_id(job_id)
            .one(&self.core.db)
            .await?
        else {
            return Ok(());
        };
        let conference = find_conference(&self.core.db, job.conference_id).await?;
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, job.conference_id).await?;
        let current = lock_job(&txn, job_id).await?;
        if matches!(current.status.as_str(), "READY" | "FAILED" | "CANCELLED") {
            txn.commit().await?;
            return Ok(());
        }
        let mut am: transcription_jobs::ActiveModel = current.clone().into();
        am.status = Set("FAILED".into());
        am.failure_code = Set(Some(code.into()));
        am.ended_at = Set(Some(Utc::now()));
        am.version = Set(current.version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE meeting_artifacts SET status = 'FAILED', storage_state = 'FAILED', failure_code = $2,
                    updated_at = now(), version = version + 1
              WHERE id = $1 AND status NOT IN ('READY','DELETED')",
            [current.artifact_id.into(), code.into()],
        ))
        .await?;
        if current.mode == "LIVE" {
            set_state(&txn, job.conference_id, "OFF").await?;
        }
        bump(
            &txn,
            job.conference_id,
            ConferenceCounter::TranscriptionFailures,
            1,
        )
        .await?;
        emit(
            &txn,
            job_event(
                &updated,
                conference.meeting_space_id,
                event::TRANSCRIPTION_FAILED,
            ),
        )
        .await?;
        txn.commit().await?;
        inc(&OM.transcription_failed);
        let _ = self
            .core
            .telemetry
            .record_transcription(PipelineStage::Failed, None);
        Ok(())
    }

    /// Provider finished: persist transcript + caption track as READY artefacts.
    async fn complete(&self, job_id: Uuid) -> ApiResult<IngestOutcome> {
        let conference;
        {
            let Some(job) = transcription_jobs::Entity::find_by_id(job_id)
                .one(&self.core.db)
                .await?
            else {
                return Ok(IngestOutcome::Unknown);
            };
            conference = find_conference(&self.core.db, job.conference_id).await?;
            let txn = self.core.db.begin().await?;
            lock_conference(&txn, job.conference_id).await?;
            let current = lock_job(&txn, job_id).await?;
            if !matches!(current.status.as_str(), "REQUESTED" | "ACTIVE" | "STOPPING") {
                txn.commit().await?;
                return Ok(IngestOutcome::Duplicate);
            }
            let mut am: transcription_jobs::ActiveModel = current.clone().into();
            am.status = Set("PROCESSING".into());
            am.ended_at = Set(Some(Utc::now()));
            am.version = Set(current.version + 1);
            am.updated_at = Set(Utc::now());
            let updated = am.update(&txn).await?;
            if current.mode == "LIVE" {
                set_state(&txn, current.conference_id, "OFF").await?;
            }
            emit(
                &txn,
                job_event(
                    &updated,
                    conference.meeting_space_id,
                    event::TRANSCRIPTION_PROCESSING,
                ),
            )
            .await?;
            txn.commit().await?;
        }
        let job = transcription_jobs::Entity::find_by_id(job_id)
            .one(&self.core.db)
            .await?
            .ok_or_else(not_found_artifact)?;
        let segments = transcript_segments::Entity::find()
            .filter(transcript_segments::Column::JobId.eq(job_id))
            .order_by_asc(transcript_segments::Column::Sequence)
            .all(&self.core.db)
            .await?;
        let transcript = json!({
            "conferenceId": job.conference_id,
            "jobId": job.id,
            "language": job.language,
            "segments": segments.iter().map(SegmentView::of).collect::<Vec<_>>(),
        });
        let artifacts = ArtifactService::new(self.core.clone());
        let stored = artifacts
            .store_generated(
                &conference,
                ArtifactType::Transcript,
                serde_json::to_vec(&transcript)
                    .map_err(|e| ApiError::InternalError(e.to_string()))?,
                "application/json",
                json!({
                    "language": job.language,
                    "provider": job.provider,
                    "segmentCount": segments.len(),
                    "status": "FINAL",
                    "mode": job.mode,
                }),
                job.source_artifact_id,
                job.requested_by.clone(),
                Some(job.artifact_id),
            )
            .await;
        if stored.is_err() {
            self.fail(job_id, "ARTIFACT_STORAGE_UNAVAILABLE").await?;
            return Ok(IngestOutcome::Applied);
        }
        let _ = artifacts
            .store_generated(
                &conference,
                ArtifactType::CaptionTrack,
                to_webvtt(&segments).into_bytes(),
                "text/vtt",
                json!({
                    "language": job.language,
                    "generated": true,
                    "live": job.mode == "LIVE",
                    "sourceTranscriptId": job.artifact_id,
                }),
                Some(job.artifact_id),
                job.requested_by.clone(),
                None,
            )
            .await;
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, job.conference_id).await?;
        let current = lock_job(&txn, job_id).await?;
        let mut am: transcription_jobs::ActiveModel = current.clone().into();
        am.status = Set("READY".into());
        am.version = Set(current.version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        emit(
            &txn,
            job_event(
                &updated,
                conference.meeting_space_id,
                event::TRANSCRIPTION_READY,
            ),
        )
        .await?;
        txn.commit().await?;
        inc(&OM.transcription_ready);
        let latency = job
            .ended_at
            .map(|e| (Utc::now() - e).num_milliseconds() as f64 / 1000.0);
        let _ = self
            .core
            .telemetry
            .record_transcription(PipelineStage::Ready, latency);
        Ok(IngestOutcome::Applied)
    }

    /// Verified provider callback.
    pub async fn ingest(&self, e: &ProviderTranscriptionEvent) -> ApiResult<IngestOutcome> {
        let provider_name = self.core.artifacts.transcription.name();
        let job_ref = match e {
            ProviderTranscriptionEvent::JobStatus { job_ref, .. }
            | ProviderTranscriptionEvent::Segment { job_ref, .. } => job_ref,
        };
        let Some(job) = transcription_jobs::Entity::find()
            .filter(transcription_jobs::Column::Provider.eq(provider_name))
            .filter(transcription_jobs::Column::ProviderJobRef.eq(job_ref.clone()))
            .one(&self.core.db)
            .await?
        else {
            return Ok(IngestOutcome::Unknown);
        };
        match e {
            ProviderTranscriptionEvent::JobStatus { status, error, .. } => {
                let s = match status.as_str() {
                    "ACTIVE" => ProviderJobStatus::Active,
                    "COMPLETED" => ProviderJobStatus::Completed,
                    "FAILED" => ProviderJobStatus::Failed,
                    "CANCELLED" => ProviderJobStatus::Cancelled,
                    _ => ProviderJobStatus::Pending,
                };
                self.apply_status(job.id, s, error.clone()).await
            }
            ProviderTranscriptionEvent::Segment {
                segment_id,
                sequence,
                speaker_identity,
                start_ms,
                end_ms,
                language,
                text,
                confidence,
                is_final,
                ..
            } => {
                if segment_id.is_empty()
                    || segment_id.len() > 128
                    || *sequence < 0
                    || *start_ms < 0
                    || end_ms < start_ms
                    || text.chars().count() > 10_000
                    || !valid_language(language)
                    || confidence
                        .map(|c| !(0.0..=1.0).contains(&c))
                        .unwrap_or(false)
                {
                    return Err(ApiError::domain(
                        ErrorCode::ValidationError,
                        "invalid segment",
                    ));
                }
                let conference = find_conference(&self.core.db, job.conference_id).await?;
                // Speaker: server-generated media identity → participant of THIS conference.
                let speaker = match speaker_identity {
                    Some(identity) => participant_sessions::Entity::find()
                        .filter(participant_sessions::Column::ConferenceId.eq(job.conference_id))
                        .filter(participant_sessions::Column::MediaIdentity.eq(identity.clone()))
                        .one(&self.core.db)
                        .await?
                        .map(|s| s.conference_participant_id),
                    None => None,
                };
                let payload = json!({
                    "conferenceId": job.conference_id,
                    "jobId": job.id,
                    "sequence": sequence,
                    "speakerParticipantId": speaker,
                    "startMs": start_ms,
                    "endMs": end_ms,
                    "language": language,
                    "text": text,
                    "final": is_final,
                });
                if !is_final {
                    if job.status != "ACTIVE" {
                        return Ok(IngestOutcome::Late);
                    }
                    publish_transient(
                        self.core.redis.as_ref(),
                        &conference,
                        event::TRANSCRIPTION_SEGMENT_PARTIAL,
                        payload.clone(),
                        None,
                    )
                    .await;
                    publish_transient(
                        self.core.redis.as_ref(),
                        &conference,
                        event::CAPTION_SEGMENT,
                        payload,
                        None,
                    )
                    .await;
                    return Ok(IngestOutcome::Applied);
                }
                if !matches!(
                    job.status.as_str(),
                    "REQUESTED" | "ACTIVE" | "STOPPING" | "PROCESSING"
                ) {
                    inc(&OM.transcription_segments_late);
                    return Ok(IngestOutcome::Late);
                }
                let txn = self.core.db.begin().await?;
                lock_conference(&txn, job.conference_id).await?;
                let inserted = txn
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "INSERT INTO transcript_segments
                            (id, conference_id, job_id, artifact_id, provider_segment_id, sequence,
                             speaker_participant_id, start_ms, end_ms, language, text, confidence)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                         ON CONFLICT (job_id, provider_segment_id) DO NOTHING",
                        [
                            Uuid::new_v4().into(),
                            job.conference_id.into(),
                            job.id.into(),
                            job.artifact_id.into(),
                            segment_id.clone().into(),
                            (*sequence).into(),
                            speaker.into(),
                            (*start_ms).into(),
                            (*end_ms).into(),
                            language.clone().into(),
                            text.clone().into(),
                            (*confidence).into(),
                        ],
                    ))
                    .await?
                    .rows_affected();
                if inserted == 0 {
                    txn.commit().await?;
                    inc(&OM.transcription_segments_duplicate);
                    return Ok(IngestOutcome::Duplicate);
                }
                txn.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE transcription_jobs SET segment_count = segment_count + 1, updated_at = now() WHERE id = $1",
                    [job.id.into()],
                ))
                .await?;
                emit(
                    &txn,
                    OutboxRecord {
                        aggregate_type: "transcription",
                        aggregate_id: job.id,
                        event_type: event::TRANSCRIPTION_SEGMENT_FINAL,
                        meeting_space_id: Some(conference.meeting_space_id),
                        conference_id: Some(job.conference_id),
                        payload: payload.clone(),
                    },
                )
                .await?;
                txn.commit().await?;
                inc(&OM.transcription_segments_final);
                publish_transient(
                    self.core.redis.as_ref(),
                    &conference,
                    event::CAPTION_SEGMENT,
                    payload,
                    None,
                )
                .await;
                Ok(IngestOutcome::Applied)
            }
        }
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<Vec<TranscriptionJobView>> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::TranscriptRead,
            "You cannot read transcripts of this conference",
        )?;
        Ok(transcription_jobs::Entity::find()
            .filter(transcription_jobs::Column::ConferenceId.eq(conference_id))
            .order_by_desc(transcription_jobs::Column::CreatedAt)
            .limit(100)
            .all(&self.core.db)
            .await?
            .iter()
            .map(TranscriptionJobView::of)
            .collect())
    }

    pub async fn get(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        artifact_id: Uuid,
        after: Option<i64>,
        limit: Option<u64>,
    ) -> ApiResult<TranscriptView> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::TranscriptRead,
            "You cannot read transcripts of this conference",
        )?;
        let artifact = meeting_artifacts::Entity::find_by_id(artifact_id)
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
            .filter(meeting_artifacts::Column::ArtifactType.eq("TRANSCRIPT"))
            .one(&self.core.db)
            .await?
            .ok_or_else(not_found_artifact)?;
        if status_of(&artifact) == ArtifactStatus::Deleted {
            return Err(ApiError::domain(
                ErrorCode::ArtifactDeleted,
                "This transcript was deleted",
            ));
        }
        let job = transcription_jobs::Entity::find()
            .filter(transcription_jobs::Column::ConferenceId.eq(conference_id))
            .filter(transcription_jobs::Column::ArtifactId.eq(artifact_id))
            .one(&self.core.db)
            .await?;
        let limit = limit.unwrap_or(200).clamp(1, 1000);
        let mut segments = match &job {
            Some(j) => {
                transcript_segments::Entity::find()
                    .filter(transcript_segments::Column::JobId.eq(j.id))
                    .filter(transcript_segments::Column::Sequence.gt(after.unwrap_or(-1)))
                    .order_by_asc(transcript_segments::Column::Sequence)
                    .limit(limit + 1)
                    .all(&self.core.db)
                    .await?
            }
            None => vec![],
        };
        let more = segments.len() as u64 > limit;
        segments.truncate(limit as usize);
        Ok(TranscriptView {
            artifact: ArtifactView::of(&artifact),
            job: job.as_ref().map(TranscriptionJobView::of),
            next_after: if more {
                segments.last().map(|s| s.sequence)
            } else {
                None
            },
            segments: segments.iter().map(SegmentView::of).collect(),
        })
    }

    /// Downstream translation into a new language (original untouched).
    pub async fn translate(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        artifact_id: Uuid,
        req: &TranslateRequest,
    ) -> ApiResult<TranslationResponse> {
        if !self.core.flags.transcription_v2_enabled {
            return Err(disabled());
        }
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::TranscriptRead,
            "You cannot read transcripts of this conference",
        )?;
        c.require(
            Permission::TranscriptionStart,
            "You are not allowed to request translations",
        )?;
        if !valid_language(&req.target_language) {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "invalid targetLanguage",
            ));
        }
        let translator = self.core.artifacts.translation.clone();
        if !location_permitted(
            c.profile.policy.transcription.location,
            translator.location(),
        ) {
            return Err(if translator.location().is_none() {
                provider_unavailable()
            } else {
                not_allowed("The configured translation provider is not permitted by policy")
            });
        }
        let job = transcription_jobs::Entity::find()
            .filter(transcription_jobs::Column::ConferenceId.eq(conference_id))
            .filter(transcription_jobs::Column::ArtifactId.eq(artifact_id))
            .one(&self.core.db)
            .await?
            .ok_or_else(not_found_artifact)?;
        if job.status != "READY" {
            return Err(ApiError::domain(
                ErrorCode::ArtifactNotReady,
                "The transcript is not ready",
            ));
        }
        let segments = transcript_segments::Entity::find()
            .filter(transcript_segments::Column::JobId.eq(job.id))
            .order_by_asc(transcript_segments::Column::Sequence)
            .all(&self.core.db)
            .await?;
        let mut count = 0;
        for s in &segments {
            let text = translator
                .translate(&s.text, &s.language, &req.target_language)
                .await
                .map_err(|e| match e {
                    ProviderError::NotConfigured => provider_unavailable(),
                    _ => provider_unavailable(),
                })?;
            count += self
                .core
                .db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO transcript_translations
                        (id, segment_id, conference_id, source_language, target_language, text, provider)
                     VALUES ($1,$2,$3,$4,$5,$6,$7)
                     ON CONFLICT (segment_id, target_language) DO NOTHING",
                    [
                        Uuid::new_v4().into(),
                        s.id.into(),
                        conference_id.into(),
                        s.language.clone().into(),
                        req.target_language.clone().into(),
                        text.chars().take(10_000).collect::<String>().into(),
                        translator.name().into(),
                    ],
                ))
                .await?
                .rows_affected();
        }
        inc(&OM.translation_requests);
        audit(
            &self.core.db,
            &c.audit,
            AuditRecord {
                meeting_space_id: c.space.id,
                conference_id: Some(conference_id),
                action: action::TRANSLATION_REQUEST,
                target_type: Some("ARTIFACT"),
                target_ref: Some(artifact_id.to_string()),
                metadata: json!({ "targetLanguage": req.target_language }),
            },
        )
        .await?;
        Ok(TranslationResponse {
            artifact_id,
            target_language: req.target_language.clone(),
            translated_segments: count,
        })
    }

    /// Reconcile active jobs against the provider (drift is escalated, not guessed).
    pub async fn reconcile(core: &Core, batch: u64) -> ApiResult<u64> {
        let provider = core.artifacts.transcription.clone();
        if provider.capabilities().location.is_none() {
            return Ok(0);
        }
        let jobs = transcription_jobs::Entity::find()
            .filter(transcription_jobs::Column::Status.is_in(["ACTIVE", "STOPPING"]))
            .filter(transcription_jobs::Column::ProviderJobRef.is_not_null())
            .order_by_asc(transcription_jobs::Column::UpdatedAt)
            .limit(batch)
            .all(&core.db)
            .await?;
        let service = Self::new(core.clone());
        let mut n = 0;
        for j in jobs {
            match provider
                .get_job(j.provider_job_ref.as_deref().unwrap_or_default())
                .await
            {
                Ok(Some(pj)) if pj.status != ProviderJobStatus::Active => {
                    service.apply_status(j.id, pj.status, None).await?;
                    n += 1;
                }
                Ok(None) => {
                    let _ = incidents::record(
                        &core.db,
                        &Incident {
                            kind: kind::TRANSCRIPTION_PROVIDER_DRIFT,
                            category: "TRANSCRIPTION",
                            conference_id: Some(j.conference_id),
                            room_name: None,
                            subject: Some(format!("transcription:{}", j.id)),
                            action: IncidentAction::Escalated,
                            critical: false,
                            detail: json!({ "jobId": j.id, "reason": "provider has no such job" }),
                        },
                    )
                    .await;
                    let _ = core.telemetry.record_provider_drift("transcription");
                    service.fail(j.id, "PROVIDER_JOB_LOST").await?;
                    n += 1;
                }
                _ => {}
            }
        }
        Ok(n)
    }
}

async fn lock_job(txn: &DatabaseTransaction, id: Uuid) -> ApiResult<transcription_jobs::Model> {
    transcription_jobs::Entity::find_by_id(id)
        .lock(LockType::Update)
        .one(txn)
        .await?
        .ok_or_else(|| {
            ApiError::domain(
                ErrorCode::TranscriptionNotFound,
                "Transcription job not found",
            )
        })
}

/// A pending (REQUESTED) artefact row for a job that will produce it later.
async fn insert_pending_artifact(
    txn: &DatabaseTransaction,
    conference: &conference_sessions::Model,
    id: Uuid,
    t: ArtifactType,
    source: Option<Uuid>,
    c: &ArtifactCtx,
    language: &str,
) -> ApiResult<()> {
    let now = Utc::now();
    meeting_artifacts::ActiveModel {
        id: Set(id),
        meeting_space_id: Set(conference.meeting_space_id),
        conference_id: Set(conference.id),
        artifact_type: Set(t.as_str().into()),
        provider: Set(Some("cfmeet".into())),
        provider_ref: Set(None),
        storage_state: Set(ArtifactStatus::Requested.storage_state().into()),
        storage_uri: Set(None),
        size_bytes: Set(None),
        checksum_sha256: Set(None),
        retention_policy_ref: Set(None),
        created_by: Set(c.audit.actor_ref.clone()),
        created_at: Set(now),
        updated_at: Set(now),
        available_at: Set(None),
        deleted_at: Set(None),
        status: Set(ArtifactStatus::Requested.as_str().into()),
        storage_bucket: Set(None),
        storage_key: Set(None),
        content_type: Set(Some("application/json".into())),
        duration_ms: Set(None),
        layout: Set(None),
        metadata: Set(json!({ "language": language, "status": "IN_PROGRESS" })),
        retention_at: Set(None),
        started_at: Set(Some(now)),
        ended_at: Set(None),
        finalized_at: Set(None),
        stop_requested_at: Set(None),
        delete_requested_at: Set(None),
        delete_reason: Set(None),
        failure_code: Set(None),
        failure_detail: Set(None),
        attempts: Set(0),
        next_attempt_at: Set(None),
        locked_by: Set(None),
        locked_until: Set(None),
        verified_at: Set(None),
        request_id: Set(crate::middleware::request_id::current_request_id()),
        idempotency_key: Set(None),
        source_artifact_id: Set(source),
        version: Set(1),
    }
    .insert(txn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webvtt_is_well_formed() {
        let seg = |seq: i64, start: i64, end: i64, text: &str| transcript_segments::Model {
            id: Uuid::nil(),
            conference_id: Uuid::nil(),
            job_id: Uuid::nil(),
            artifact_id: Uuid::nil(),
            provider_segment_id: format!("s{seq}"),
            sequence: seq,
            speaker_participant_id: None,
            start_ms: start,
            end_ms: end,
            language: "en".into(),
            text: text.into(),
            confidence: None,
            created_at: Utc::now(),
        };
        let vtt = to_webvtt(&[
            seg(0, 0, 1500, "hello"),
            seg(1, 3_661_001, 3_662_000, "a --> b"),
        ]);
        assert!(vtt.starts_with("WEBVTT\n\n1\n00:00:00.000 --> 00:00:01.500\nhello\n"));
        assert!(vtt.contains("01:01:01.001 --> 01:01:02.000\na -> b"));
    }

    #[test]
    fn callback_signatures_are_verified_and_bounded_in_time() {
        let ts = Utc::now().timestamp().to_string();
        let sig = sign_callback("secret", &ts, "{}");
        assert!(verify_callback("secret", &ts, "{}", &sig));
        assert!(!verify_callback("other", &ts, "{}", &sig));
        assert!(!verify_callback("secret", &ts, "{\"x\":1}", &sig));
        let old = (Utc::now().timestamp() - 3600).to_string();
        let old_sig = sign_callback("secret", &old, "{}");
        assert!(!verify_callback("secret", &old, "{}", &old_sig));
    }
}

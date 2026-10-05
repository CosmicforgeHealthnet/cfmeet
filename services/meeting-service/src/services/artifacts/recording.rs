//! RecordingService — authoritative recording lifecycle (Part G §6–§13).
//!
//! ```text
//! REQUESTED ─(provider evidence: ACTIVE)→ RECORDING ─(stop)→ STOPPING
//!   ─(provider evidence: COMPLETE + output)→ PROCESSING ─(object verified)→ READY ─→ DELETED
//! any non-terminal ─(provider failure / start timeout / lost capture)→ FAILED
//! ```
//!
//! - Start: identity → conference → authority → LIVE → `recording.start` → space +
//!   profile policy → feature flag → consent → REQUESTED commits with audit + outbox +
//!   an `rtc.recording_start_requested` command. The provider is called only after
//!   that commit. One active composite recording per conference (unique index); two
//!   hosts racing produce one capture.
//! - REQUESTED → RECORDING only on provider evidence (verified webhook, provider
//!   response or reconciler query reporting ACTIVE). STOPPING → READY only after the
//!   provider finalized AND the object was verified in storage (`ArtifactService`).
//! - Provider callbacks are deduplicated (`rtc_webhook_events`), mapped by the internal
//!   provider reference, checked against the conference's provider room, and applied
//!   only as forward transitions (out-of-order / duplicate evidence is a no-op).
//! - Conference end stops capture but never waits for it: a recording may stay
//!   PROCESSING after the conference ended.

use chrono::{Duration, Utc};
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction,
    DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    bump,
    recorder::{
        CompositeRecordingRequest, ProviderRecording, ProviderRecordingStatus, RecordingError,
    },
    service::ArtifactService,
    status_of, ArtifactCtx, ArtifactStatus, ArtifactType, ArtifactView, ConferenceCounter,
};
use crate::{
    domain::ConferenceStatus,
    error::{ApiError, ApiResult, ErrorCode},
    middleware::request_id::current_request_id,
    models::{conference_sessions, meeting_artifacts, outbox_events},
    services::{
        actor::ActorContext,
        collaboration::policy::ConsentKind,
        meeting_core::{
            find_conference, is_unique_violation, lock_conference, permission_denied, Core,
        },
        meeting_events::{action, audit, emit, event, AuditActor, AuditRecord, OutboxRecord},
        meeting_permissions::{baseline, Cell, Permission},
        ops_metrics::{inc, OPS_METRICS as OM},
        quality::{
            incidents::{self, kind, Incident, IncidentAction},
            sink::PipelineStage,
        },
        rtc_control::{CommandResult, Enforcement, RtcControl},
        rtc_provider::RtcRoomRef,
    },
};

/// `POST /recordings` body. The client never selects provider ids, storage buckets
/// or keys.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRecordingRequest {
    /// `GRID` (default from policy) or `SPEAKER`.
    pub layout: Option<String>,
    /// Client idempotency key (repeats return the same recording).
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProviderDispatch {
    /// The provider accepted the request (capture evidence follows asynchronously).
    Dispatched,
    /// Committed; the provider call is queued and retried.
    PendingRetry,
    /// Nothing to dispatch (idempotent repeat).
    NotRequired,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecordingResponse {
    pub recording: ArtifactView,
    pub provider_dispatch: ProviderDispatch,
    /// False for an idempotent repeat.
    pub created: bool,
}

/// Result of applying provider evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceOutcome {
    Applied(ArtifactStatus),
    /// Duplicate / out-of-order evidence (no transition).
    Ignored,
    /// No recording with this provider reference.
    Unknown,
    /// Provider reference / room mismatch: never mutates another conference.
    Rejected,
}

pub struct RecordingService {
    core: Core,
}

fn recording_disabled() -> ApiError {
    ApiError::domain(ErrorCode::RecordingDisabled, "Recording is not enabled")
}

fn not_allowed(message: &str) -> ApiError {
    ApiError::domain(ErrorCode::RecordingNotAllowed, message)
}

fn not_found() -> ApiError {
    ApiError::domain(ErrorCode::RecordingNotFound, "Recording not found")
}

fn provider_unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::RecordingProviderUnavailable,
        "Recording is temporarily unavailable",
    )
}

fn normalize_layout(raw: Option<&str>, default: &str) -> ApiResult<String> {
    match raw.map(|l| l.trim().to_ascii_lowercase()) {
        None => Ok(default.to_string()),
        Some(l) if l == "grid" || l == "speaker" => Ok(l),
        Some(_) => Err(ApiError::domain(
            ErrorCode::ValidationError,
            "layout must be GRID or SPEAKER",
        )),
    }
}

/// Conference-level semantic recording indicator kept in sync with the artefact.
fn indicator(status: ArtifactStatus) -> &'static str {
    match status {
        ArtifactStatus::Requested => "REQUESTED",
        ArtifactStatus::Recording => "RECORDING",
        ArtifactStatus::Stopping => "STOPPING",
        _ => "OFF",
    }
}

pub fn recording_payload(a: &meeting_artifacts::Model) -> serde_json::Value {
    json!({
        "conferenceId": a.conference_id,
        "recordingId": a.id,
        "artifactId": a.id,
        "status": a.status,
        "layout": a.layout,
        "startedAt": a.started_at,
        "endedAt": a.ended_at,
        "failureCode": a.failure_code,
    })
}

fn recording_event<'a>(a: &meeting_artifacts::Model, event_type: &'a str) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "recording",
        aggregate_id: a.id,
        event_type,
        meeting_space_id: Some(a.meeting_space_id),
        conference_id: Some(a.conference_id),
        payload: recording_payload(a),
    }
}

async fn set_indicator<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    status: ArtifactStatus,
) -> ApiResult<()> {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE conference_sessions SET recording_state = $2, updated_at = now() WHERE id = $1",
        [conference_id.into(), indicator(status).into()],
    ))
    .await?;
    Ok(())
}

/// Transition helper: updates status (+ legacy storage_state) and version.
fn transition(a: &meeting_artifacts::Model, to: ArtifactStatus) -> meeting_artifacts::ActiveModel {
    let mut am: meeting_artifacts::ActiveModel = a.clone().into();
    am.status = Set(to.as_str().to_string());
    am.storage_state = Set(to.storage_state().to_string());
    am.version = Set(a.version + 1);
    am.updated_at = Set(Utc::now());
    am
}

async fn lock_artifact(
    txn: &DatabaseTransaction,
    conference_id: Uuid,
    id: Uuid,
) -> ApiResult<meeting_artifacts::Model> {
    meeting_artifacts::Entity::find_by_id(id)
        .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
        .lock(LockType::Update)
        .one(txn)
        .await?
        .ok_or_else(not_found)
}

/// Mark a recording FAILED on a locked row (events + counters + indicator).
pub async fn fail_in_txn(
    txn: &DatabaseTransaction,
    core: &Core,
    a: &meeting_artifacts::Model,
    code: &str,
    detail: Option<String>,
    actor: &AuditActor,
) -> ApiResult<meeting_artifacts::Model> {
    let was_active = status_of(a).is_active_capture();
    let mut am = transition(a, ArtifactStatus::Failed);
    am.failure_code = Set(Some(code.to_string()));
    am.failure_detail = Set(detail);
    if a.ended_at.is_none() {
        am.ended_at = Set(Some(Utc::now()));
    }
    let updated = am.update(txn).await?;
    if was_active {
        set_indicator(txn, a.conference_id, ArtifactStatus::Failed).await?;
    }
    bump(
        txn,
        a.conference_id,
        ConferenceCounter::RecordingFailures,
        1,
    )
    .await?;
    audit(
        txn,
        actor,
        AuditRecord {
            meeting_space_id: a.meeting_space_id,
            conference_id: Some(a.conference_id),
            action: action::RECORDING_FAILED,
            target_type: Some("RECORDING"),
            target_ref: Some(a.id.to_string()),
            metadata: json!({ "failureCode": code }),
        },
    )
    .await?;
    emit(txn, recording_event(&updated, event::RECORDING_FAILED)).await?;
    emit(
        txn,
        OutboxRecord {
            aggregate_type: "artifact",
            aggregate_id: updated.id,
            event_type: event::ARTIFACT_FAILED,
            meeting_space_id: Some(updated.meeting_space_id),
            conference_id: Some(updated.conference_id),
            payload: json!({
                "conferenceId": updated.conference_id,
                "artifactId": updated.id,
                "artifactType": updated.artifact_type,
                "failureCode": code,
            }),
        },
    )
    .await?;
    inc(&OM.recording_failed);
    let _ = core.telemetry.record_recording(PipelineStage::Failed, None);
    Ok(updated)
}

impl RecordingService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    fn require_enabled(&self) -> ApiResult<()> {
        if self.core.flags.recording_v2_enabled {
            Ok(())
        } else {
            Err(recording_disabled())
        }
    }

    /// Distinguish "your role cannot" from "policy forbids capture here".
    fn require_capture_permission(c: &ArtifactCtx, p: Permission) -> ApiResult<()> {
        if c.allows(p) {
            return Ok(());
        }
        let role_could = match baseline(c.role, p) {
            Cell::Yes => true,
            Cell::No => false,
            Cell::Policy(default) => c
                .pctx
                .role_policy
                .overrides
                .get(&c.role)
                .and_then(|m| m.get(p.key()))
                .copied()
                .unwrap_or(default),
        };
        if role_could {
            Err(not_allowed("Recording is disabled for this meeting"))
        } else {
            Err(permission_denied(
                "You are not allowed to control recording",
            ))
        }
    }

    pub async fn start(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: &StartRecordingRequest,
    ) -> ApiResult<RecordingResponse> {
        self.require_enabled()?;
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        if c.conference.status != ConferenceStatus::Live {
            return Err(ApiError::domain(
                ErrorCode::ConferenceNotLive,
                "Recording can only start in a live conference",
            ));
        }
        Self::require_capture_permission(&c, Permission::RecordingStart)?;
        if !c.profile.policy.recording.allowed {
            inc(&OM.profile_policy_denials);
            return Err(not_allowed(
                "Recording is not permitted by this meeting's policy",
            ));
        }
        let layout = normalize_layout(req.layout.as_deref(), &c.profile.policy.recording.layout)?;
        let idem = match &req.idempotency_key {
            Some(k) if k.trim().is_empty() || k.len() > 128 => {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "idempotencyKey must be 1-128 characters",
                ))
            }
            Some(k) => Some(crate::services::secrets::hash_secret(k)),
            None => None,
        };
        if let Some(key) = &idem {
            if let Some(existing) = meeting_artifacts::Entity::find()
                .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
                .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
                .filter(meeting_artifacts::Column::IdempotencyKey.eq(key.clone()))
                .one(&self.core.db)
                .await?
            {
                return Ok(RecordingResponse {
                    recording: ArtifactView::of(&existing),
                    provider_dispatch: ProviderDispatch::NotRequired,
                    created: false,
                });
            }
        }
        let backends = &self.core.artifacts;
        if !backends.recorder.capabilities().composite {
            return Err(provider_unavailable());
        }
        if !backends.storage.is_configured() {
            return Err(super::storage_unavailable());
        }
        // Consent (Part G §19): explicit-consent policy blocks the provider start until
        // every affected participant consented; they receive a targeted request.
        super::consent::ConsentService::new(self.core.clone())
            .require_for_processing(&c, ConsentKind::Recording)
            .await?;

        let id = Uuid::new_v4();
        let key = backends.storage.object_key(&format!(
            "recordings/{}/{}.mp4",
            conference_id.simple(),
            id.simple()
        ));
        let txn = self.core.db.begin().await?;
        // Lock order space → conference (as everywhere else). The shared space lock
        // serializes with profile-policy updates: a restriction committed first is seen
        // here; one committed after finds this recording and stops it.
        let space = super::lock_space_shared(&txn, c.space.id).await?;
        let conference = lock_conference(&txn, conference_id).await?;
        if conference.status != ConferenceStatus::Live {
            return Err(ApiError::domain(
                ErrorCode::ConferenceNotLive,
                "Recording can only start in a live conference",
            ));
        }
        let current_profile = crate::services::profiles::MeetingProfileResolver::load(
            &txn,
            &space,
            Some(&conference),
        )
        .await?;
        if !current_profile.policy.recording.allowed {
            inc(&OM.profile_policy_denials);
            return Err(not_allowed(
                "Recording is not permitted by this meeting's policy",
            ));
        }
        // Re-check under the conference lock: a withdrawal (which also takes the lock)
        // either commits first and blocks this start, or commits after and stops it.
        if super::consent::mode_for(&current_profile, ConsentKind::Recording)
            == crate::services::profiles::ConsentMode::ExplicitConsentRequired
        {
            let version = super::consent::ConsentService::version(&c);
            let (missing, _) = super::consent::missing_consent(
                &txn,
                conference_id,
                ConsentKind::Recording,
                &version,
                true,
            )
            .await?;
            if !missing.is_empty() {
                return Err(ApiError::domain(
                    ErrorCode::ConsentRequired,
                    "Recording consent changed; every participant must consent",
                ));
            }
        }
        let now = Utc::now();
        let insert = meeting_artifacts::ActiveModel {
            id: Set(id),
            meeting_space_id: Set(conference.meeting_space_id),
            conference_id: Set(conference_id),
            artifact_type: Set(ArtifactType::Recording.as_str().into()),
            provider: Set(Some(backends.recorder.name().into())),
            provider_ref: Set(None),
            storage_state: Set(ArtifactStatus::Requested.storage_state().into()),
            storage_uri: Set(None),
            size_bytes: Set(None),
            checksum_sha256: Set(None),
            retention_policy_ref: Set(c.space.retention_policy_ref.clone()),
            created_by: Set(c.audit.actor_ref.clone()),
            created_at: Set(now),
            updated_at: Set(now),
            available_at: Set(None),
            deleted_at: Set(None),
            status: Set(ArtifactStatus::Requested.as_str().into()),
            storage_bucket: Set(Some(backends.storage.bucket().to_string())),
            storage_key: Set(Some(key)),
            content_type: Set(Some("video/mp4".into())),
            duration_ms: Set(None),
            layout: Set(Some(layout.clone())),
            metadata: Set(json!({
                "kind": "COMPOSITE",
                "layout": layout,
                "profile": c.profile.profile,
                "consentMode": c.profile.policy.recording.consent_mode,
            })),
            retention_at: Set(None),
            started_at: Set(None),
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
            request_id: Set(current_request_id()),
            idempotency_key: Set(idem),
            source_artifact_id: Set(None),
            version: Set(1),
        };
        let artifact = match insert.insert(&txn).await {
            Ok(a) => a,
            Err(e) if is_unique_violation(&e) => {
                return Err(ApiError::domain(
                    ErrorCode::RecordingAlreadyActive,
                    "A recording is already active for this conference",
                ))
            }
            Err(e) => return Err(e.into()),
        };
        set_indicator(&txn, conference_id, ArtifactStatus::Requested).await?;
        audit(
            &txn,
            &c.audit,
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::RECORDING_START,
                target_type: Some("RECORDING"),
                target_ref: Some(id.to_string()),
                metadata: json!({ "layout": layout }),
            },
        )
        .await?;
        emit(&txn, recording_event(&artifact, event::RECORDING_REQUESTED)).await?;
        emit(
            &txn,
            OutboxRecord {
                aggregate_type: "recording",
                aggregate_id: id,
                event_type: event::RTC_RECORDING_START_REQUESTED,
                meeting_space_id: Some(conference.meeting_space_id),
                conference_id: Some(conference_id),
                payload: json!({ "conferenceId": conference_id, "recordingId": id }),
            },
        )
        .await?;
        txn.commit().await?;
        inc(&OM.recording_requested);
        let _ = self
            .core
            .telemetry
            .record_recording(PipelineStage::Requested, None);
        let dispatch = self.dispatch(conference_id).await;
        let current = meeting_artifacts::Entity::find_by_id(id)
            .one(&self.core.db)
            .await?
            .unwrap_or(artifact);
        Ok(RecordingResponse {
            recording: ArtifactView::of(&current),
            provider_dispatch: dispatch,
            created: true,
        })
    }

    async fn dispatch(&self, conference_id: Uuid) -> ProviderDispatch {
        match RtcControl::new(self.core.clone())
            .dispatch_for_conference(conference_id)
            .await
        {
            Enforcement::Enforced | Enforcement::NotRequired => ProviderDispatch::Dispatched,
            Enforcement::Pending => ProviderDispatch::PendingRetry,
        }
    }

    /// Host stop (idempotent: a recording already stopping / processing / ready is
    /// returned unchanged — one effective stop).
    pub async fn stop(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        recording_id: Uuid,
    ) -> ApiResult<RecordingResponse> {
        self.require_enabled()?;
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        Self::require_capture_permission(&c, Permission::RecordingStop)?;
        let (artifact, queued) = self
            .stop_inner(conference_id, recording_id, &c.audit, "HOST_STOP")
            .await?;
        let dispatch = if queued {
            self.dispatch(conference_id).await
        } else {
            ProviderDispatch::NotRequired
        };
        let current = meeting_artifacts::Entity::find_by_id(artifact.id)
            .one(&self.core.db)
            .await?
            .unwrap_or(artifact);
        Ok(RecordingResponse {
            recording: ArtifactView::of(&current),
            provider_dispatch: dispatch,
            created: false,
        })
    }

    /// System stop (consent withdrawal, policy change). Returns whether anything stopped.
    pub async fn system_stop(&self, conference_id: Uuid, reason: &str) -> ApiResult<bool> {
        let Some(active) = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .filter(meeting_artifacts::Column::Status.is_in(["REQUESTED", "RECORDING"]))
            .one(&self.core.db)
            .await?
        else {
            return Ok(false);
        };
        let (_, queued) = self
            .stop_inner(
                conference_id,
                active.id,
                &AuditActor::system("recording-policy"),
                reason,
            )
            .await?;
        if queued {
            self.dispatch(conference_id).await;
        }
        Ok(true)
    }

    async fn stop_inner(
        &self,
        conference_id: Uuid,
        recording_id: Uuid,
        actor: &AuditActor,
        reason: &str,
    ) -> ApiResult<(meeting_artifacts::Model, bool)> {
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, conference_id).await?;
        let a = lock_artifact(&txn, conference_id, recording_id).await?;
        if a.artifact_type != ArtifactType::Recording.as_str() {
            return Err(not_found());
        }
        let status = status_of(&a);
        match status {
            ArtifactStatus::Stopping | ArtifactStatus::Processing | ArtifactStatus::Ready => {
                txn.commit().await?;
                return Ok((a, false));
            }
            ArtifactStatus::Failed | ArtifactStatus::Deleted => {
                return Err(ApiError::domain(
                    ErrorCode::RecordingNotActive,
                    "This recording is not active",
                ))
            }
            ArtifactStatus::Requested | ArtifactStatus::Recording => {}
        }
        if status == ArtifactStatus::Requested && a.provider_ref.is_none() {
            // Nothing was captured yet: the pending start command becomes a no-op.
            let failed = fail_in_txn(
                &txn,
                &self.core,
                &a,
                "STOPPED_BEFORE_START",
                Some(reason.to_string()),
                actor,
            )
            .await?;
            txn.commit().await?;
            return Ok((failed, false));
        }
        let mut am = transition(&a, ArtifactStatus::Stopping);
        am.stop_requested_at = Set(Some(Utc::now()));
        let updated = am.update(&txn).await?;
        set_indicator(&txn, conference_id, ArtifactStatus::Stopping).await?;
        audit(
            &txn,
            actor,
            AuditRecord {
                meeting_space_id: a.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::RECORDING_STOP,
                target_type: Some("RECORDING"),
                target_ref: Some(a.id.to_string()),
                metadata: json!({ "reason": reason }),
            },
        )
        .await?;
        emit(&txn, recording_event(&updated, event::RECORDING_STOPPING)).await?;
        emit(
            &txn,
            OutboxRecord {
                aggregate_type: "recording",
                aggregate_id: a.id,
                event_type: event::RTC_RECORDING_STOP_REQUESTED,
                meeting_space_id: Some(a.meeting_space_id),
                conference_id: Some(conference_id),
                payload: json!({ "conferenceId": conference_id, "recordingId": a.id }),
            },
        )
        .await?;
        txn.commit().await?;
        inc(&OM.recording_stop_requested);
        Ok((updated, true))
    }

    /// Conference end (called on the locked conference transaction): stop capture,
    /// never wait for it.
    pub async fn stop_all_in_txn(
        txn: &DatabaseTransaction,
        core: &Core,
        conference: &conference_sessions::Model,
        actor: &AuditActor,
    ) -> ApiResult<()> {
        let active = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference.id))
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .filter(meeting_artifacts::Column::Status.is_in(["REQUESTED", "RECORDING"]))
            .lock(LockType::Update)
            .all(txn)
            .await?;
        for a in active {
            if a.provider_ref.is_none() {
                fail_in_txn(txn, core, &a, "CONFERENCE_ENDED", None, actor).await?;
                continue;
            }
            let mut am = transition(&a, ArtifactStatus::Stopping);
            am.stop_requested_at = Set(Some(Utc::now()));
            let updated = am.update(txn).await?;
            set_indicator(txn, conference.id, ArtifactStatus::Stopping).await?;
            emit(txn, recording_event(&updated, event::RECORDING_STOPPING)).await?;
            emit(
                txn,
                OutboxRecord {
                    aggregate_type: "recording",
                    aggregate_id: a.id,
                    event_type: event::RTC_RECORDING_STOP_REQUESTED,
                    meeting_space_id: Some(a.meeting_space_id),
                    conference_id: Some(conference.id),
                    payload: json!({ "conferenceId": conference.id, "recordingId": a.id, "reason": "CONFERENCE_ENDED" }),
                },
            )
            .await?;
        }
        Ok(())
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<Vec<ArtifactView>> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        if !(c.allows(Permission::ArtifactList)
            || c.allows(Permission::RecordingStart)
            || c.allows(Permission::RecordingStop))
        {
            return Err(permission_denied(
                "You cannot view recordings of this conference",
            ));
        }
        let rows = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .order_by_desc(meeting_artifacts::Column::CreatedAt)
            .limit(100)
            .all(&self.core.db)
            .await?;
        Ok(rows.iter().map(ArtifactView::of).collect())
    }

    pub async fn get(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        recording_id: Uuid,
    ) -> ApiResult<ArtifactView> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        if !(c.allows(Permission::ArtifactList)
            || c.allows(Permission::RecordingStart)
            || c.allows(Permission::RecordingStop))
        {
            return Err(permission_denied(
                "You cannot view recordings of this conference",
            ));
        }
        let a = meeting_artifacts::Entity::find_by_id(recording_id)
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .one(&self.core.db)
            .await?
            .ok_or_else(not_found)?;
        Ok(ArtifactView::of(&a))
    }

    /// Current recording indicator of a conference (recovery after reconnect).
    pub async fn active_recording<C: ConnectionTrait>(
        conn: &C,
        conference_id: Uuid,
    ) -> ApiResult<Option<meeting_artifacts::Model>> {
        Ok(meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .filter(meeting_artifacts::Column::Status.is_in(["REQUESTED", "RECORDING", "STOPPING"]))
            .one(conn)
            .await?)
    }

    // -----------------------------------------------------------------------
    // Provider commands (executed after commit by RtcControl / relay)
    // -----------------------------------------------------------------------

    pub async fn execute_command(
        core: &Core,
        conference: &conference_sessions::Model,
        row: &outbox_events::Model,
    ) -> CommandResult {
        let Some(recording_id) = row.payload["recordingId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return CommandResult::Dead("missing recordingId".into());
        };
        let artifact = match meeting_artifacts::Entity::find_by_id(recording_id)
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference.id))
            .one(&core.db)
            .await
        {
            Ok(Some(a)) => a,
            Ok(None) => {
                return CommandResult::Dead("recording is not part of this conference".into())
            }
            Err(e) => return CommandResult::Retry(e.to_string()),
        };
        let recorder = core.artifacts.recorder.clone();
        match row.event_type.as_str() {
            event::RTC_RECORDING_START_REQUESTED => {
                if status_of(&artifact) != ArtifactStatus::Requested
                    || artifact.provider_ref.is_some()
                {
                    return CommandResult::Done;
                }
                let room_name = RtcRoomRef::for_conference(conference).room_name;
                // Provider-side idempotency: adopt an active capture of this room that no
                // recording owns (a previous attempt whose reference was not stored).
                match recorder.list_active(&room_name).await {
                    Ok(active) => {
                        for p in active {
                            let owned = meeting_artifacts::Entity::find()
                                .filter(
                                    meeting_artifacts::Column::ProviderRef
                                        .eq(p.provider_ref.clone()),
                                )
                                .one(&core.db)
                                .await;
                            match owned {
                                Ok(None) => {
                                    return match Self::attach_provider(core, &artifact, &p).await {
                                        Ok(()) => CommandResult::Done,
                                        Err(e) => CommandResult::Retry(e.to_string()),
                                    }
                                }
                                Ok(Some(_)) => {}
                                Err(e) => return CommandResult::Retry(e.to_string()),
                            }
                        }
                    }
                    Err(RecordingError::Unavailable(e)) => {
                        inc(&OM.recording_provider_errors);
                        return CommandResult::Retry(e);
                    }
                    Err(_) => {}
                }
                let request = CompositeRecordingRequest {
                    room_name,
                    layout: artifact.layout.clone().unwrap_or_else(|| "grid".into()),
                    output_key: artifact.storage_key.clone().unwrap_or_default(),
                };
                match recorder.start_composite(&request).await {
                    Ok(p) => match Self::attach_provider(core, &artifact, &p).await {
                        Ok(()) => CommandResult::Done,
                        Err(e) => CommandResult::Retry(e.to_string()),
                    },
                    Err(RecordingError::Unavailable(e)) => {
                        inc(&OM.recording_provider_errors);
                        CommandResult::Retry(e)
                    }
                    Err(e) => {
                        inc(&OM.recording_provider_errors);
                        match Self::fail(
                            core,
                            artifact.id,
                            conference.id,
                            "PROVIDER_REJECTED",
                            Some(e.to_string()),
                        )
                        .await
                        {
                            Ok(()) => CommandResult::Done,
                            Err(err) => CommandResult::Retry(err.to_string()),
                        }
                    }
                }
            }
            event::RTC_RECORDING_STOP_REQUESTED => {
                if status_of(&artifact) != ArtifactStatus::Stopping {
                    return CommandResult::Done;
                }
                let Some(provider_ref) = artifact.provider_ref.clone() else {
                    return CommandResult::Done;
                };
                match recorder.stop(&provider_ref).await {
                    Ok(p) => {
                        let _ = Self::apply_evidence(core, &p, None).await;
                        CommandResult::Done
                    }
                    Err(RecordingError::NotFound) => {
                        match Self::fail(
                            core,
                            artifact.id,
                            conference.id,
                            "PROVIDER_RECORDING_LOST",
                            None,
                        )
                        .await
                        {
                            Ok(()) => CommandResult::Done,
                            Err(err) => CommandResult::Retry(err.to_string()),
                        }
                    }
                    Err(e) => {
                        inc(&OM.recording_provider_errors);
                        CommandResult::Retry(e.to_string())
                    }
                }
            }
            other => CommandResult::Dead(format!("unknown recording command {other}")),
        }
    }

    /// Store the provider reference, then apply whatever evidence the provider
    /// response already carries.
    async fn attach_provider(
        core: &Core,
        artifact: &meeting_artifacts::Model,
        p: &ProviderRecording,
    ) -> ApiResult<()> {
        core.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE meeting_artifacts SET provider_ref = $2, updated_at = now(),
                        metadata = metadata || jsonb_build_object('providerStatus', $3::text)
                  WHERE id = $1 AND provider_ref IS NULL",
                [
                    artifact.id.into(),
                    p.provider_ref.clone().into(),
                    format!("{:?}", p.status).to_ascii_uppercase().into(),
                ],
            ))
            .await?;
        Self::apply_evidence(core, p, None).await?;
        Ok(())
    }

    async fn fail(
        core: &Core,
        artifact_id: Uuid,
        conference_id: Uuid,
        code: &str,
        detail: Option<String>,
    ) -> ApiResult<()> {
        let txn = core.db.begin().await?;
        lock_conference(&txn, conference_id).await?;
        let a = lock_artifact(&txn, conference_id, artifact_id).await?;
        if matches!(
            status_of(&a),
            ArtifactStatus::Failed | ArtifactStatus::Deleted | ArtifactStatus::Ready
        ) {
            txn.commit().await?;
            return Ok(());
        }
        fail_in_txn(
            &txn,
            core,
            &a,
            code,
            detail,
            &AuditActor::system("recording-provider"),
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Provider evidence (verified webhook / provider response / reconciler query)
    // -----------------------------------------------------------------------

    /// Apply provider evidence. `room_name` (from a webhook) must match the recording's
    /// conference room or nothing is mutated.
    pub async fn apply_evidence(
        core: &Core,
        p: &ProviderRecording,
        room_name: Option<&str>,
    ) -> ApiResult<EvidenceOutcome> {
        if p.provider_ref.is_empty() {
            return Ok(EvidenceOutcome::Unknown);
        }
        let Some(found) = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ProviderRef.eq(p.provider_ref.clone()))
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .one(&core.db)
            .await?
        else {
            inc(&OM.recording_callbacks_unknown);
            return Ok(EvidenceOutcome::Unknown);
        };
        let conference = find_conference(&core.db, found.conference_id).await?;
        let expected_room = RtcRoomRef::for_conference(&conference).room_name;
        let claimed = room_name
            .or(Some(p.room_name.as_str()))
            .filter(|r| !r.is_empty());
        if let Some(r) = claimed {
            if r != expected_room {
                tracing::warn!(recording_id = %found.id, "provider evidence room mismatch; ignored");
                return Ok(EvidenceOutcome::Rejected);
            }
        }
        let actor = AuditActor::system("recording-provider");
        let txn = core.db.begin().await?;
        lock_conference(&txn, found.conference_id).await?;
        let a = lock_artifact(&txn, found.conference_id, found.id).await?;
        let current = status_of(&a);
        use ProviderRecordingStatus as P;
        let outcome = match (current, p.status) {
            (ArtifactStatus::Requested, P::Active) => {
                let mut am = transition(&a, ArtifactStatus::Recording);
                am.started_at = Set(Some(p.started_at.unwrap_or_else(Utc::now)));
                let updated = am.update(&txn).await?;
                set_indicator(&txn, a.conference_id, ArtifactStatus::Recording).await?;
                audit(
                    &txn,
                    &actor,
                    AuditRecord {
                        meeting_space_id: a.meeting_space_id,
                        conference_id: Some(a.conference_id),
                        action: action::RECORDING_PROVIDER_STARTED,
                        target_type: Some("RECORDING"),
                        target_ref: Some(a.id.to_string()),
                        metadata: json!({}),
                    },
                )
                .await?;
                emit(&txn, recording_event(&updated, event::RECORDING_STARTED)).await?;
                inc(&OM.recording_started);
                let latency = updated
                    .started_at
                    .map(|s| (s - a.created_at).num_milliseconds() as f64 / 1000.0);
                let _ = core
                    .telemetry
                    .record_recording(PipelineStage::Started, latency);
                EvidenceOutcome::Applied(ArtifactStatus::Recording)
            }
            (ArtifactStatus::Requested | ArtifactStatus::Recording, P::Ending) => {
                let mut am = transition(&a, ArtifactStatus::Stopping);
                if a.started_at.is_none() {
                    am.started_at = Set(p.started_at);
                }
                let updated = am.update(&txn).await?;
                set_indicator(&txn, a.conference_id, ArtifactStatus::Stopping).await?;
                emit(&txn, recording_event(&updated, event::RECORDING_STOPPING)).await?;
                EvidenceOutcome::Applied(ArtifactStatus::Stopping)
            }
            (
                ArtifactStatus::Requested | ArtifactStatus::Recording | ArtifactStatus::Stopping,
                P::Complete | P::LimitReached,
            ) if p.output.is_some() => {
                let out = p.output.clone().expect("checked");
                let key_matches = a
                    .storage_key
                    .as_deref()
                    .map(|k| {
                        k == out.filename || k.ends_with(&out.filename) || out.filename.ends_with(k)
                    })
                    .unwrap_or(false);
                let mut am = transition(&a, ArtifactStatus::Processing);
                if a.started_at.is_none() {
                    am.started_at = Set(p.started_at);
                }
                am.ended_at = Set(Some(p.ended_at.unwrap_or_else(Utc::now)));
                am.duration_ms = Set(out.duration_ms.map(|d| d as i64));
                am.next_attempt_at = Set(Some(Utc::now()));
                let mut meta = a.metadata.clone();
                meta["providerOutput"] = json!({
                    "sizeBytes": out.size_bytes,
                    "durationMs": out.duration_ms,
                    "keyMatches": key_matches,
                    "limitReached": p.status == P::LimitReached,
                });
                am.metadata = Set(meta);
                let updated = am.update(&txn).await?;
                set_indicator(&txn, a.conference_id, ArtifactStatus::Processing).await?;
                emit(&txn, recording_event(&updated, event::RECORDING_PROCESSING)).await?;
                inc(&OM.recording_processing);
                EvidenceOutcome::Applied(ArtifactStatus::Processing)
            }
            (
                ArtifactStatus::Requested | ArtifactStatus::Recording | ArtifactStatus::Stopping,
                P::Complete | P::LimitReached | P::Failed | P::Aborted,
            ) => {
                let code = match p.status {
                    P::Aborted => "PROVIDER_ABORTED",
                    P::Failed => "PROVIDER_FAILED",
                    _ => "PROVIDER_NO_OUTPUT",
                };
                fail_in_txn(&txn, core, &a, code, p.error.clone(), &actor).await?;
                EvidenceOutcome::Applied(ArtifactStatus::Failed)
            }
            _ => {
                inc(&OM.recording_callbacks_duplicate);
                EvidenceOutcome::Ignored
            }
        };
        txn.commit().await?;
        if outcome == EvidenceOutcome::Applied(ArtifactStatus::Processing) {
            // Verify the object now; retries happen in the artefact worker.
            let _ = ArtifactService::new(core.clone()).finalize(a.id).await;
        }
        Ok(outcome)
    }

    // -----------------------------------------------------------------------
    // Reconciliation (lost callbacks, start timeouts, provider drift)
    // -----------------------------------------------------------------------

    pub async fn reconcile(core: &Core, batch: u64) -> ApiResult<u64> {
        let mut repaired = 0;
        let cfg = &core.config.artifacts;
        let recorder = core.artifacts.recorder.clone();
        if !recorder.capabilities().query {
            return Ok(0);
        }
        let now = Utc::now();
        // 1. REQUESTED beyond the start timeout.
        let stale_requested = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .filter(meeting_artifacts::Column::Status.eq("REQUESTED"))
            .filter(
                meeting_artifacts::Column::CreatedAt
                    .lt(now - Duration::seconds(cfg.recording_start_timeout_secs)),
            )
            .limit(batch)
            .all(&core.db)
            .await?;
        for a in stale_requested {
            match a.provider_ref.as_deref() {
                Some(r) => match recorder.query(r).await {
                    Ok(Some(p)) if p.status != ProviderRecordingStatus::Starting => {
                        Self::apply_evidence(core, &p, None).await?;
                        repaired += 1;
                    }
                    Ok(None) => {
                        Self::fail(core, a.id, a.conference_id, "PROVIDER_RECORDING_LOST", None)
                            .await?;
                        Self::incident(
                            core,
                            &a,
                            kind::RECORDING_PROVIDER_DRIFT,
                            IncidentAction::Escalated,
                            "provider has no such capture",
                        )
                        .await;
                        repaired += 1;
                    }
                    Ok(Some(_))
                        if a.created_at
                            < now - Duration::seconds(cfg.recording_start_timeout_secs * 3) =>
                    {
                        let _ = recorder.stop(r).await;
                        Self::fail(core, a.id, a.conference_id, "RECORDING_START_TIMEOUT", None)
                            .await?;
                        Self::incident(
                            core,
                            &a,
                            kind::RECORDING_START_TIMEOUT,
                            IncidentAction::Escalated,
                            "provider never confirmed capture",
                        )
                        .await;
                        repaired += 1;
                    }
                    _ => {}
                },
                None => {
                    Self::fail(core, a.id, a.conference_id, "RECORDING_START_TIMEOUT", None)
                        .await?;
                    Self::incident(
                        core,
                        &a,
                        kind::RECORDING_START_TIMEOUT,
                        IncidentAction::Escalated,
                        "start command never reached the provider",
                    )
                    .await;
                    repaired += 1;
                }
            }
        }
        // 2. RECORDING / STOPPING whose completion callback may have been lost.
        let in_flight = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ArtifactType.eq("RECORDING"))
            .filter(meeting_artifacts::Column::Status.is_in(["RECORDING", "STOPPING"]))
            .filter(meeting_artifacts::Column::ProviderRef.is_not_null())
            .order_by_asc(meeting_artifacts::Column::UpdatedAt)
            .limit(batch)
            .all(&core.db)
            .await?;
        for a in in_flight {
            let overdue = status_of(&a) == ArtifactStatus::Recording
                || a.updated_at < now - Duration::seconds(cfg.recording_stop_timeout_secs);
            if !overdue {
                continue;
            }
            match recorder
                .query(a.provider_ref.as_deref().unwrap_or_default())
                .await
            {
                Ok(Some(p))
                    if p.status.is_terminal() || p.status == ProviderRecordingStatus::Ending =>
                {
                    if Self::apply_evidence(core, &p, None).await? != EvidenceOutcome::Ignored {
                        repaired += 1;
                        Self::incident(
                            core,
                            &a,
                            kind::RECORDING_PROVIDER_DRIFT,
                            IncidentAction::Repaired,
                            "lost provider callback applied from query",
                        )
                        .await;
                    }
                }
                Ok(None) => {
                    Self::fail(core, a.id, a.conference_id, "PROVIDER_RECORDING_LOST", None)
                        .await?;
                    Self::incident(
                        core,
                        &a,
                        kind::RECORDING_PROVIDER_DRIFT,
                        IncidentAction::Escalated,
                        "provider has no such capture",
                    )
                    .await;
                    repaired += 1;
                }
                _ => {}
            }
        }
        Ok(repaired)
    }

    /// Active provider captures of a LIVE conference that no recording owns
    /// (ambiguous: escalated, never stopped by guess).
    pub async fn detect_unowned(
        core: &Core,
        conference: &conference_sessions::Model,
    ) -> ApiResult<u64> {
        let recorder = core.artifacts.recorder.clone();
        if !recorder.capabilities().query {
            return Ok(0);
        }
        let room = RtcRoomRef::for_conference(conference).room_name;
        let Ok(active) = recorder.list_active(&room).await else {
            return Ok(0);
        };
        let mut found = 0;
        for p in active {
            let owned = meeting_artifacts::Entity::find()
                .filter(meeting_artifacts::Column::ProviderRef.eq(p.provider_ref.clone()))
                .one(&core.db)
                .await?;
            if owned.is_none() {
                found += 1;
                let _ = incidents::record(
                    &core.db,
                    &Incident {
                        kind: kind::RECORDING_PROVIDER_DRIFT,
                        category: "RECORDING",
                        conference_id: Some(conference.id),
                        room_name: Some(&room),
                        subject: Some(format!(
                            "unowned:{}",
                            &crate::services::secrets::hash_secret(&p.provider_ref)[..16]
                        )),
                        action: IncidentAction::Escalated,
                        critical: false,
                        detail: json!({ "reason": "provider capture without recording" }),
                    },
                )
                .await;
                let _ = core.telemetry.record_provider_drift("recording_unowned");
            }
        }
        Ok(found)
    }

    async fn incident(
        core: &Core,
        a: &meeting_artifacts::Model,
        kind: &str,
        action: IncidentAction,
        reason: &str,
    ) {
        let _ = core.telemetry.record_provider_drift("recording");
        let _ = incidents::record(
            &core.db,
            &Incident {
                kind,
                category: "RECORDING",
                conference_id: Some(a.conference_id),
                room_name: None,
                subject: Some(format!("recording:{}", a.id)),
                action,
                critical: action != IncidentAction::Repaired,
                detail: json!({ "recordingId": a.id, "reason": reason }),
            },
        )
        .await;
    }
}

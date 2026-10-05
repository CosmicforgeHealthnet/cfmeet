//! ArtifactService — the unified artefact domain (Part G §23–§28).
//!
//! Handlers never mutate `meeting_artifacts` directly. This service registers,
//! transitions, verifies, authorizes, signs, deletes and generates artefacts:
//!
//! - READY requires a provider-finalized (or server-generated) object that exists in
//!   storage, whose size matches the provider report and whose integrity (SHA-256, plus
//!   the single-part ETag MD5 when available) has been verified.
//! - Access: authorize (`artifact.read`, `transcript.read` for transcripts / captions)
//!   → READY and not pending deletion → short-lived read-only signed URL for exactly
//!   that object. The URL is never stored; storage credentials never leave the
//!   server. Knowing an artefact id is never authorization.
//! - Deletion (manual or retention): request recorded → object deleted → absence
//!   confirmed by a fresh HEAD → only then DELETED. Failures retry with backoff and
//!   escalate as incidents.

use chrono::{Duration, Utc};
use md5::{Digest, Md5};
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction,
    DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    not_found_artifact, status_of, storage::StorageError, storage_unavailable, type_of,
    ArtifactCtx, ArtifactStatus, ArtifactType, ArtifactView,
};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    middleware::request_id::current_request_id,
    models::{conference_participants, conference_sessions, meeting_artifacts, meeting_polls},
    services::{
        actor::ActorContext,
        meeting_core::{lock_conference, Core},
        meeting_events::{action, audit, emit, event, AuditActor, AuditRecord, OutboxRecord},
        meeting_permissions::Permission,
        ops_metrics::{inc, OPS_METRICS as OM},
        profiles::{MeetingProfileResolver, ResolvedProfile},
        quality::{
            incidents::{self, kind, Incident, IncidentAction},
            sink::PipelineStage,
        },
    },
};

/// Signed, short-lived, single-object read access (never persisted).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAccess {
    pub artifact_id: Uuid,
    pub url: String,
    pub expires_at: chrono::DateTime<Utc>,
    pub expires_in_seconds: u64,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerateReportRequest {
    /// ATTENDANCE_REPORT, POLL_REPORT or CHAT_EXPORT.
    pub artifact_type: ArtifactType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeOutcome {
    Ready,
    /// Object not (yet) present / storage unavailable: retried later.
    Pending,
    Failed,
    /// Not PROCESSING or claimed by another worker.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    Pending,
    Skipped,
}

pub struct ArtifactService {
    core: Core,
}

/// Retention by artefact family (profile / organization configuration).
pub fn retention_days(profile: &ResolvedProfile, t: ArtifactType) -> u32 {
    let p = &profile.policy;
    match t {
        ArtifactType::Recording => p.recording.retention_days,
        ArtifactType::Transcript | ArtifactType::CaptionTrack => p.transcription.retention_days,
        ArtifactType::MeetingSummary => p.intelligence.retention_days,
        ArtifactType::AttendanceReport
        | ArtifactType::ChatExport
        | ArtifactType::PollReport
        | ArtifactType::WhiteboardSnapshot => p.exports.retention_days,
    }
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((5i64 << attempts.clamp(0, 8)).min(3600))
}

fn artifact_event<'a>(a: &meeting_artifacts::Model, event_type: &'a str) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "artifact",
        aggregate_id: a.id,
        event_type,
        meeting_space_id: Some(a.meeting_space_id),
        conference_id: Some(a.conference_id),
        payload: json!({
            "conferenceId": a.conference_id,
            "artifactId": a.id,
            "artifactType": a.artifact_type,
            "status": a.status,
            "sizeBytes": a.size_bytes,
            "durationMs": a.duration_ms,
            "retentionAt": a.retention_at,
        }),
    }
}

fn requires_transcript_permission(a: &meeting_artifacts::Model) -> bool {
    matches!(
        type_of(a),
        Some(ArtifactType::Transcript | ArtifactType::CaptionTrack)
    )
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
        .ok_or_else(not_found_artifact)
}

impl ArtifactService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    /// Scoped lookup: conference id + artefact id (never id alone).
    async fn scoped(&self, conference_id: Uuid, id: Uuid) -> ApiResult<meeting_artifacts::Model> {
        meeting_artifacts::Entity::find_by_id(id)
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id))
            .one(&self.core.db)
            .await?
            .ok_or_else(not_found_artifact)
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        artifact_type: Option<ArtifactType>,
    ) -> ApiResult<Vec<ArtifactView>> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::ArtifactList,
            "You cannot list artifacts of this conference",
        )?;
        let mut q = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::ConferenceId.eq(conference_id));
        if let Some(t) = artifact_type {
            q = q.filter(meeting_artifacts::Column::ArtifactType.eq(t.as_str()));
        }
        let rows = q
            .order_by_desc(meeting_artifacts::Column::CreatedAt)
            .limit(200)
            .all(&self.core.db)
            .await?;
        Ok(rows
            .iter()
            .filter(|a| !requires_transcript_permission(a) || c.allows(Permission::TranscriptRead))
            .map(ArtifactView::of)
            .collect())
    }

    pub async fn get(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        artifact_id: Uuid,
    ) -> ApiResult<ArtifactView> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        let a = self.scoped(conference_id, artifact_id).await?;
        let allowed = (c.allows(Permission::ArtifactList) || c.allows(Permission::ArtifactRead))
            && (!requires_transcript_permission(&a) || c.allows(Permission::TranscriptRead));
        if !allowed {
            inc(&OM.artifact_access_denied);
            return Err(ApiError::domain(
                ErrorCode::ArtifactAccessDenied,
                "You are not allowed to view this artifact",
            ));
        }
        Ok(ArtifactView::of(&a))
    }

    /// Authorize first, then mint a short-lived signed read URL (never stored).
    pub async fn access(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        artifact_id: Uuid,
    ) -> ApiResult<ArtifactAccess> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        let a = self.scoped(conference_id, artifact_id).await?;
        let allowed = c.allows(Permission::ArtifactRead)
            && (!requires_transcript_permission(&a) || c.allows(Permission::TranscriptRead));
        if !allowed {
            inc(&OM.artifact_access_denied);
            return Err(ApiError::domain(
                ErrorCode::ArtifactAccessDenied,
                "You are not allowed to access this artifact",
            ));
        }
        match status_of(&a) {
            ArtifactStatus::Deleted => {
                return Err(ApiError::domain(
                    ErrorCode::ArtifactDeleted,
                    "This artifact was deleted",
                ))
            }
            _ if a.delete_requested_at.is_some() => {
                return Err(ApiError::domain(
                    ErrorCode::ArtifactDeleted,
                    "This artifact is being deleted",
                ))
            }
            ArtifactStatus::Ready => {}
            _ => {
                return Err(ApiError::domain(
                    ErrorCode::ArtifactNotReady,
                    "This artifact is not ready yet",
                ))
            }
        }
        let key = a.storage_key.clone().ok_or_else(storage_unavailable)?;
        let ttl = std::time::Duration::from_secs(self.core.config.artifacts.signed_url_ttl_secs);
        let ext = match type_of(&a) {
            Some(ArtifactType::Recording) => "mp4",
            Some(ArtifactType::CaptionTrack) => "vtt",
            _ => "json",
        };
        let signed = self
            .core
            .artifacts
            .storage
            .signed_read_url(&key, ttl, Some(&format!("{}.{ext}", a.id.simple())))
            .map_err(|e| {
                inc(&OM.artifact_sign_failures);
                tracing::warn!(artifact_id = %a.id, "artifact signing failed: {e}");
                storage_unavailable()
            })?;
        audit(
            &self.core.db,
            &c.audit,
            AuditRecord {
                meeting_space_id: a.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::ARTIFACT_ACCESS,
                target_type: Some("ARTIFACT"),
                target_ref: Some(a.id.to_string()),
                metadata: json!({
                    "artifactType": a.artifact_type,
                    "ttlSeconds": ttl.as_secs(),
                    "enhanced": c.profile.policy.audit.enhanced,
                }),
            },
        )
        .await?;
        inc(&OM.artifact_access_granted);
        Ok(ArtifactAccess {
            artifact_id: a.id,
            url: signed.url,
            expires_at: signed.expires_at,
            expires_in_seconds: ttl.as_secs(),
            content_type: a.content_type.clone(),
            size_bytes: a.size_bytes,
        })
    }

    /// Manual deletion: request → storage delete → confirmed absence → DELETED.
    pub async fn delete(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        artifact_id: Uuid,
    ) -> ApiResult<ArtifactView> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::ArtifactDelete,
            "You are not allowed to delete artifacts",
        )?;
        let current = self.scoped(conference_id, artifact_id).await?;
        if type_of(&current) == Some(ArtifactType::Recording)
            && !c.profile.policy.recording.manual_delete_allowed
        {
            inc(&OM.profile_policy_denials);
            return Err(ApiError::domain(
                ErrorCode::ProfilePolicyDenied,
                "Manual deletion of recordings is disabled by policy",
            ));
        }
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, conference_id).await?;
        let a = lock_artifact(&txn, conference_id, artifact_id).await?;
        let status = status_of(&a);
        if status == ArtifactStatus::Deleted || a.delete_requested_at.is_some() {
            txn.commit().await?;
        } else if status.is_active_capture() {
            return Err(ApiError::domain(
                ErrorCode::RecordingStateConflict,
                "Stop the recording before deleting it",
            ));
        } else {
            let mut am: meeting_artifacts::ActiveModel = a.clone().into();
            am.delete_requested_at = Set(Some(Utc::now()));
            am.delete_reason = Set(Some("MANUAL".into()));
            am.next_attempt_at = Set(Some(Utc::now()));
            am.updated_at = Set(Utc::now());
            am.update(&txn).await?;
            audit(
                &txn,
                &c.audit,
                AuditRecord {
                    meeting_space_id: a.meeting_space_id,
                    conference_id: Some(conference_id),
                    action: action::ARTIFACT_DELETE_REQUEST,
                    target_type: Some("ARTIFACT"),
                    target_ref: Some(a.id.to_string()),
                    metadata: json!({ "artifactType": a.artifact_type, "reason": "MANUAL" }),
                },
            )
            .await?;
            txn.commit().await?;
        }
        let _ = self.delete_now(artifact_id).await;
        let a = self.scoped(conference_id, artifact_id).await?;
        Ok(ArtifactView::of(&a))
    }

    /// Delete one due artefact (manual request or retention reached). DELETED is
    /// written only after the object's absence is confirmed.
    pub async fn delete_now(&self, artifact_id: Uuid) -> ApiResult<DeleteOutcome> {
        let holder = crate::services::outbox::holder_id("artifact-delete");
        let claimed = self
            .core
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE meeting_artifacts
                    SET locked_by = $2, locked_until = now() + interval '120 seconds'
                  WHERE id = $1 AND status <> 'DELETED'
                    AND status NOT IN ('REQUESTED','RECORDING','STOPPING')
                    AND (delete_requested_at IS NOT NULL OR retention_at <= now())
                    AND (locked_until IS NULL OR locked_until < now())
                  RETURNING storage_key, conference_id, attempts",
                [artifact_id.into(), holder.clone().into()],
            ))
            .await?;
        let Some(row) = claimed else {
            return Ok(DeleteOutcome::Skipped);
        };
        let key: Option<String> = row.try_get_by_index(0).ok().flatten();
        let conference_id: Uuid = row
            .try_get_by_index(1)
            .map_err(|e| ApiError::InternalError(e.to_string()))?;
        let attempts: i32 = row.try_get_by_index(2).unwrap_or(0);
        let storage = self.core.artifacts.storage.clone();
        let confirmed = match &key {
            None => Ok(()),
            Some(k) => match storage.delete(k).await {
                Ok(()) => match storage.head(k).await {
                    Ok(None) => Ok(()),
                    Ok(Some(_)) => Err("object still present after delete".to_string()),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            },
        };
        if let Err(reason) = confirmed {
            inc(&OM.artifact_delete_failures);
            let next = Utc::now() + backoff(attempts);
            self.core
                .db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE meeting_artifacts
                        SET attempts = attempts + 1, next_attempt_at = $3, locked_by = NULL,
                            locked_until = NULL, updated_at = now()
                      WHERE id = $1 AND locked_by = $2",
                    [artifact_id.into(), holder.into(), next.into()],
                ))
                .await?;
            if attempts + 1 >= 3 {
                let _ = incidents::record(
                    &self.core.db,
                    &Incident {
                        kind: kind::ARTIFACT_DELETE_FAILED,
                        category: "ARTIFACT",
                        conference_id: Some(conference_id),
                        room_name: None,
                        subject: Some(format!("artifact:{artifact_id}")),
                        action: IncidentAction::Failed,
                        critical: false,
                        detail: json!({ "artifactId": artifact_id, "reason": reason }),
                    },
                )
                .await;
            }
            return Ok(DeleteOutcome::Pending);
        }
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, conference_id).await?;
        let a = lock_artifact(&txn, conference_id, artifact_id).await?;
        if status_of(&a) == ArtifactStatus::Deleted {
            txn.commit().await?;
            return Ok(DeleteOutcome::Skipped);
        }
        let reason = a
            .delete_reason
            .clone()
            .unwrap_or_else(|| "RETENTION".into());
        let mut am: meeting_artifacts::ActiveModel = a.clone().into();
        am.status = Set(ArtifactStatus::Deleted.as_str().into());
        am.storage_state = Set(ArtifactStatus::Deleted.storage_state().into());
        am.deleted_at = Set(Some(Utc::now()));
        am.delete_reason = Set(Some(reason.clone()));
        am.locked_by = Set(None);
        am.locked_until = Set(None);
        am.version = Set(a.version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &AuditActor::system("artifact-retention"),
            AuditRecord {
                meeting_space_id: a.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::ARTIFACT_DELETED,
                target_type: Some("ARTIFACT"),
                target_ref: Some(a.id.to_string()),
                metadata: json!({ "artifactType": a.artifact_type, "reason": reason }),
            },
        )
        .await?;
        emit(&txn, artifact_event(&updated, event::ARTIFACT_DELETED)).await?;
        if type_of(&updated) == Some(ArtifactType::Recording) {
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "recording",
                    aggregate_id: updated.id,
                    event_type: event::RECORDING_DELETED,
                    meeting_space_id: Some(updated.meeting_space_id),
                    conference_id: Some(conference_id),
                    payload: super::recording::recording_payload(&updated),
                },
            )
            .await?;
        }
        txn.commit().await?;
        inc(&OM.artifact_deleted);
        Ok(DeleteOutcome::Deleted)
    }

    /// Verify a PROCESSING artefact's object and move it to READY (or FAILED).
    pub async fn finalize(&self, artifact_id: Uuid) -> ApiResult<FinalizeOutcome> {
        let holder = crate::services::outbox::holder_id("artifact-finalize");
        let claimed = meeting_artifacts::Entity::find_by_id(artifact_id)
            .one(&self.core.db)
            .await?;
        let Some(candidate) = claimed else {
            return Ok(FinalizeOutcome::Skipped);
        };
        if status_of(&candidate) != ArtifactStatus::Processing {
            return Ok(FinalizeOutcome::Skipped);
        }
        let lease = self
            .core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE meeting_artifacts SET locked_by = $2, locked_until = now() + interval '120 seconds'
                  WHERE id = $1 AND status = 'PROCESSING'
                    AND (locked_until IS NULL OR locked_until < now())",
                [artifact_id.into(), holder.clone().into()],
            ))
            .await?;
        if lease.rows_affected() == 0 {
            return Ok(FinalizeOutcome::Skipped);
        }
        let Some(key) = candidate.storage_key.clone() else {
            self.mark_failed(&candidate, "ARTIFACT_MISSING", "no storage reference")
                .await?;
            return Ok(FinalizeOutcome::Failed);
        };
        let storage = self.core.artifacts.storage.clone();
        let cfg = &self.core.config.artifacts;
        let meta = match storage.head(&key).await {
            Ok(Some(m)) => m,
            Ok(None) => {
                inc(&OM.artifact_missing);
                return self
                    .finalize_retry(&candidate, &holder, "object not found in storage")
                    .await;
            }
            Err(e) => return self.finalize_unavailable(artifact_id, &holder, e).await,
        };
        let provider_size = candidate
            .metadata
            .pointer("/providerOutput/sizeBytes")
            .and_then(Value::as_u64);
        if meta.size == 0 || provider_size.map(|s| s != meta.size).unwrap_or(false) {
            self.integrity_failure(&candidate, "size mismatch").await?;
            return Ok(FinalizeOutcome::Failed);
        }
        let digest = match storage.digest(&key, cfg.max_checksum_bytes).await {
            Ok(Some(d)) => d,
            Ok(None) => {
                return self
                    .finalize_retry(
                        &candidate,
                        &holder,
                        "object disappeared during verification",
                    )
                    .await
            }
            Err(StorageError::TooLarge) => {
                self.integrity_failure(&candidate, "object exceeds the verification limit")
                    .await?;
                return Ok(FinalizeOutcome::Failed);
            }
            Err(e) => return self.finalize_unavailable(artifact_id, &holder, e).await,
        };
        let md5_checked = meta.md5_etag().map(|etag| etag == digest.md5);
        if digest.size != meta.size || md5_checked == Some(false) {
            self.integrity_failure(&candidate, "checksum mismatch")
                .await?;
            return Ok(FinalizeOutcome::Failed);
        }
        self.mark_ready(
            &candidate,
            meta.size as i64,
            &digest.sha256,
            meta.content_type.clone(),
            md5_checked.unwrap_or(false),
        )
        .await?;
        Ok(FinalizeOutcome::Ready)
    }

    /// Object not (yet) in storage: retry with backoff, FAILED after the bound.
    async fn finalize_retry(
        &self,
        candidate: &meeting_artifacts::Model,
        holder: &str,
        reason: &str,
    ) -> ApiResult<FinalizeOutcome> {
        inc(&OM.artifact_finalize_retries);
        let attempts = candidate.attempts + 1;
        if attempts >= self.core.config.artifacts.finalize_max_attempts {
            self.mark_failed(candidate, "ARTIFACT_MISSING", reason)
                .await?;
            return Ok(FinalizeOutcome::Failed);
        }
        self.core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE meeting_artifacts
                    SET attempts = $3, next_attempt_at = $4, locked_by = NULL,
                        locked_until = NULL, updated_at = now()
                  WHERE id = $1 AND locked_by = $2",
                [
                    candidate.id.into(),
                    holder.into(),
                    attempts.into(),
                    (Utc::now() + backoff(attempts)).into(),
                ],
            ))
            .await?;
        Ok(FinalizeOutcome::Pending)
    }

    /// Storage outage: never fails the artefact, just retries soon.
    async fn finalize_unavailable(
        &self,
        artifact_id: Uuid,
        holder: &str,
        e: StorageError,
    ) -> ApiResult<FinalizeOutcome> {
        inc(&OM.artifact_finalize_retries);
        tracing::warn!(artifact_id = %artifact_id, "finalize: storage unavailable: {e}");
        self.core
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE meeting_artifacts
                    SET next_attempt_at = $3, locked_by = NULL, locked_until = NULL, updated_at = now()
                  WHERE id = $1 AND locked_by = $2",
                [
                    artifact_id.into(),
                    holder.into(),
                    (Utc::now() + Duration::seconds(15)).into(),
                ],
            ))
            .await?;
        Ok(FinalizeOutcome::Pending)
    }

    async fn mark_ready(
        &self,
        candidate: &meeting_artifacts::Model,
        size: i64,
        sha256: &str,
        content_type: Option<String>,
        md5_verified: bool,
    ) -> ApiResult<()> {
        let space =
            crate::services::meeting_core::find_space(&self.core.db, candidate.meeting_space_id)
                .await?;
        let conference_model = conference_sessions::Entity::find_by_id(candidate.conference_id)
            .one(&self.core.db)
            .await?;
        let profile =
            MeetingProfileResolver::load(&self.core.db, &space, conference_model.as_ref()).await?;
        let t = type_of(candidate).unwrap_or(ArtifactType::Recording);
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, candidate.conference_id).await?;
        let a = lock_artifact(&txn, candidate.conference_id, candidate.id).await?;
        if status_of(&a) != ArtifactStatus::Processing {
            txn.commit().await?;
            return Ok(());
        }
        let now = Utc::now();
        let mut meta = a.metadata.clone();
        meta["integrity"] = json!({ "sha256": true, "md5": md5_verified });
        let mut am: meeting_artifacts::ActiveModel = a.clone().into();
        am.status = Set(ArtifactStatus::Ready.as_str().into());
        am.storage_state = Set(ArtifactStatus::Ready.storage_state().into());
        am.size_bytes = Set(Some(size));
        am.checksum_sha256 = Set(Some(sha256.to_string()));
        if content_type.is_some() && a.content_type.is_none() {
            am.content_type = Set(content_type);
        }
        am.finalized_at = Set(Some(now));
        am.available_at = Set(Some(now));
        am.verified_at = Set(Some(now));
        am.retention_at = Set(Some(
            now + Duration::days(retention_days(&profile, t) as i64),
        ));
        am.metadata = Set(meta);
        am.locked_by = Set(None);
        am.locked_until = Set(None);
        am.next_attempt_at = Set(None);
        am.version = Set(a.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &AuditActor::system("artifact-finalizer"),
            AuditRecord {
                meeting_space_id: a.meeting_space_id,
                conference_id: Some(a.conference_id),
                action: action::RECORDING_FINALIZED,
                target_type: Some("ARTIFACT"),
                target_ref: Some(a.id.to_string()),
                metadata: json!({ "artifactType": a.artifact_type, "sizeBytes": size }),
            },
        )
        .await?;
        if t == ArtifactType::Recording {
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "recording",
                    aggregate_id: updated.id,
                    event_type: event::RECORDING_READY,
                    meeting_space_id: Some(updated.meeting_space_id),
                    conference_id: Some(updated.conference_id),
                    payload: super::recording::recording_payload(&updated),
                },
            )
            .await?;
        }
        emit(&txn, artifact_event(&updated, event::ARTIFACT_READY)).await?;
        txn.commit().await?;
        if t == ArtifactType::Recording {
            inc(&OM.recording_ready);
            let latency = updated
                .ended_at
                .map(|e| (now - e).num_milliseconds() as f64 / 1000.0);
            let _ = self
                .core
                .telemetry
                .record_recording(PipelineStage::Ready, latency);
        }
        Ok(())
    }

    async fn integrity_failure(&self, a: &meeting_artifacts::Model, reason: &str) -> ApiResult<()> {
        inc(&OM.artifact_integrity_failures);
        let _ = incidents::record(
            &self.core.db,
            &Incident {
                kind: kind::ARTIFACT_INTEGRITY_FAILED,
                category: "ARTIFACT",
                conference_id: Some(a.conference_id),
                room_name: None,
                subject: Some(format!("artifact:{}", a.id)),
                action: IncidentAction::Escalated,
                critical: true,
                detail: json!({ "artifactId": a.id, "reason": reason }),
            },
        )
        .await;
        self.mark_failed(a, "ARTIFACT_INTEGRITY_FAILED", reason)
            .await
    }

    async fn mark_failed(
        &self,
        a: &meeting_artifacts::Model,
        code: &str,
        reason: &str,
    ) -> ApiResult<()> {
        if code == "ARTIFACT_MISSING" {
            let _ = incidents::record(
                &self.core.db,
                &Incident {
                    kind: kind::ARTIFACT_MISSING,
                    category: "ARTIFACT",
                    conference_id: Some(a.conference_id),
                    room_name: None,
                    subject: Some(format!("artifact:{}", a.id)),
                    action: IncidentAction::Escalated,
                    critical: true,
                    detail: json!({ "artifactId": a.id, "reason": reason }),
                },
            )
            .await;
        }
        let txn = self.core.db.begin().await?;
        lock_conference(&txn, a.conference_id).await?;
        let current = lock_artifact(&txn, a.conference_id, a.id).await?;
        if matches!(
            status_of(&current),
            ArtifactStatus::Ready | ArtifactStatus::Failed | ArtifactStatus::Deleted
        ) {
            txn.commit().await?;
            return Ok(());
        }
        if type_of(&current) == Some(ArtifactType::Recording) {
            super::recording::fail_in_txn(
                &txn,
                &self.core,
                &current,
                code,
                Some(reason.to_string()),
                &AuditActor::system("artifact-finalizer"),
            )
            .await?;
        } else {
            let mut am: meeting_artifacts::ActiveModel = current.clone().into();
            am.status = Set(ArtifactStatus::Failed.as_str().into());
            am.storage_state = Set(ArtifactStatus::Failed.storage_state().into());
            am.failure_code = Set(Some(code.into()));
            am.failure_detail = Set(Some(reason.into()));
            am.locked_by = Set(None);
            am.locked_until = Set(None);
            am.version = Set(current.version + 1);
            am.updated_at = Set(Utc::now());
            let updated = am.update(&txn).await?;
            emit(&txn, artifact_event(&updated, event::ARTIFACT_FAILED)).await?;
        }
        txn.commit().await?;
        Ok(())
    }

    /// Store a server-generated artefact (report, transcript, captions, summary):
    /// put → verify → READY in one flow. Returns the READY artefact.
    #[allow(clippy::too_many_arguments)]
    pub async fn store_generated(
        &self,
        conference: &conference_sessions::Model,
        artifact_type: ArtifactType,
        bytes: Vec<u8>,
        content_type: &str,
        metadata: Value,
        source_artifact_id: Option<Uuid>,
        created_by: Option<String>,
        reuse_id: Option<Uuid>,
    ) -> ApiResult<meeting_artifacts::Model> {
        let storage = self.core.artifacts.storage.clone();
        if !storage.is_configured() {
            return Err(storage_unavailable());
        }
        let id = reuse_id.unwrap_or_else(Uuid::new_v4);
        let ext = match content_type {
            "text/vtt" => "vtt",
            "video/mp4" => "mp4",
            _ => "json",
        };
        let key = storage.object_key(&format!(
            "{}/{}/{}.{ext}",
            artifact_type.as_str().to_ascii_lowercase(),
            conference.id.simple(),
            id.simple()
        ));
        let md5 = hex::encode(Md5::digest(&bytes));
        let size = bytes.len() as i64;
        let now = Utc::now();
        if reuse_id.is_none() {
            meeting_artifacts::ActiveModel {
                id: Set(id),
                meeting_space_id: Set(conference.meeting_space_id),
                conference_id: Set(conference.id),
                artifact_type: Set(artifact_type.as_str().into()),
                provider: Set(Some("cfmeet".into())),
                provider_ref: Set(None),
                storage_state: Set(ArtifactStatus::Processing.storage_state().into()),
                storage_uri: Set(None),
                size_bytes: Set(None),
                checksum_sha256: Set(None),
                retention_policy_ref: Set(None),
                created_by: Set(created_by),
                created_at: Set(now),
                updated_at: Set(now),
                available_at: Set(None),
                deleted_at: Set(None),
                status: Set(ArtifactStatus::Processing.as_str().into()),
                storage_bucket: Set(Some(storage.bucket().to_string())),
                storage_key: Set(Some(key.clone())),
                content_type: Set(Some(content_type.into())),
                duration_ms: Set(None),
                layout: Set(None),
                metadata: Set(metadata),
                retention_at: Set(None),
                started_at: Set(None),
                ended_at: Set(Some(now)),
                finalized_at: Set(None),
                stop_requested_at: Set(None),
                delete_requested_at: Set(None),
                delete_reason: Set(None),
                failure_code: Set(None),
                failure_detail: Set(None),
                attempts: Set(0),
                next_attempt_at: Set(Some(now)),
                locked_by: Set(None),
                locked_until: Set(None),
                verified_at: Set(None),
                request_id: Set(current_request_id()),
                idempotency_key: Set(None),
                source_artifact_id: Set(source_artifact_id),
                version: Set(1),
            }
            .insert(&self.core.db)
            .await?;
        } else {
            self.core
                .db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE meeting_artifacts
                        SET status = 'PROCESSING', storage_state = 'PROCESSING', storage_bucket = $2,
                            storage_key = $3, content_type = $4, metadata = metadata || $5,
                            ended_at = COALESCE(ended_at, now()), next_attempt_at = now(), updated_at = now()
                      WHERE id = $1 AND status NOT IN ('READY','DELETED')",
                    [
                        id.into(),
                        storage.bucket().to_string().into(),
                        key.clone().into(),
                        content_type.into(),
                        metadata.into(),
                    ],
                ))
                .await?;
        }
        let stored = storage.put(&key, bytes, content_type).await.map_err(|e| {
            tracing::warn!(artifact_id = %id, "artifact upload failed: {e}");
            storage_unavailable()
        })?;
        if stored.size as i64 != size || stored.md5_etag().map(|e| e != md5).unwrap_or(false) {
            let a = self.scoped(conference.id, id).await?;
            self.integrity_failure(&a, "upload checksum mismatch")
                .await?;
            return Err(ApiError::domain(
                ErrorCode::ArtifactIntegrityFailed,
                "Artifact integrity validation failed",
            ));
        }
        match self.finalize(id).await? {
            FinalizeOutcome::Ready => {}
            FinalizeOutcome::Failed => {
                return Err(ApiError::domain(
                    ErrorCode::ArtifactIntegrityFailed,
                    "Artifact integrity validation failed",
                ))
            }
            _ => return Err(storage_unavailable()),
        }
        inc(&OM.artifact_generated);
        self.scoped(conference.id, id).await
    }

    /// Generate an attendance report, poll report or chat export (server-side data
    /// only; anonymous polls stay anonymous; private chat is never exported).
    pub async fn generate_report(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: &GenerateReportRequest,
    ) -> ApiResult<ArtifactView> {
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        c.require(
            Permission::ArtifactExport,
            "You are not allowed to export meeting data",
        )?;
        let exports = &c.profile.policy.exports;
        let allowed = match req.artifact_type {
            ArtifactType::AttendanceReport => exports.attendance_report_allowed,
            ArtifactType::PollReport => exports.poll_report_allowed,
            ArtifactType::ChatExport => exports.chat_export_allowed,
            _ => {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "artifactType must be ATTENDANCE_REPORT, POLL_REPORT or CHAT_EXPORT",
                ))
            }
        };
        if !allowed {
            inc(&OM.profile_policy_denials);
            return Err(ApiError::domain(
                ErrorCode::ProfilePolicyDenied,
                "This export is disabled by the meeting policy",
            ));
        }
        let (body, metadata) = match req.artifact_type {
            ArtifactType::AttendanceReport => self.attendance(&c.conference).await?,
            ArtifactType::PollReport => self.poll_report(&c.conference).await?,
            _ => self.chat_export(&c.conference).await?,
        };
        let bytes =
            serde_json::to_vec_pretty(&body).map_err(|e| ApiError::InternalError(e.to_string()))?;
        let a = self
            .store_generated(
                &c.conference,
                req.artifact_type,
                bytes,
                "application/json",
                metadata,
                None,
                c.audit.actor_ref.clone(),
                None,
            )
            .await?;
        audit(
            &self.core.db,
            &c.audit,
            AuditRecord {
                meeting_space_id: c.space.id,
                conference_id: Some(conference_id),
                action: action::ARTIFACT_GENERATE,
                target_type: Some("ARTIFACT"),
                target_ref: Some(a.id.to_string()),
                metadata: json!({ "artifactType": a.artifact_type }),
            },
        )
        .await?;
        Ok(ArtifactView::of(&a))
    }

    async fn attendance(
        &self,
        conference: &conference_sessions::Model,
    ) -> ApiResult<(Value, Value)> {
        let now = Utc::now();
        let seconds: std::collections::HashMap<Uuid, i64> =
            crate::services::admission::attendance_seconds(&self.core.db, conference.id, now)
                .await?
                .into_iter()
                .collect();
        let participants = conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference.id))
            .order_by_asc(conference_participants::Column::RequestedAt)
            .all(&self.core.db)
            .await?;
        let rows: Vec<Value> = participants
            .iter()
            .map(|p| {
                json!({
                    "participantId": p.id,
                    "displayName": p.display_name,
                    "role": p.role,
                    "identityType": p.identity_type,
                    "admissionState": p.admission_state,
                    "admittedAt": p.admitted_at,
                    "attendanceSeconds": seconds.get(&p.id).copied().unwrap_or(0),
                })
            })
            .collect();
        let sessions = {
            use sea_orm::PaginatorTrait;
            crate::models::participant_sessions::Entity::find()
                .filter(crate::models::participant_sessions::Column::ConferenceId.eq(conference.id))
                .count(&self.core.db)
                .await?
        };
        Ok((
            json!({
                "conferenceId": conference.id,
                "generatedAt": now,
                "participants": rows,
            }),
            json!({
                "generatedAt": now,
                "participantCount": participants.len(),
                "sessionCount": sessions,
            }),
        ))
    }

    async fn poll_report(
        &self,
        conference: &conference_sessions::Model,
    ) -> ApiResult<(Value, Value)> {
        let polls = meeting_polls::Entity::find()
            .filter(meeting_polls::Column::ConferenceId.eq(conference.id))
            .order_by_asc(meeting_polls::Column::CreatedAt)
            .all(&self.core.db)
            .await?;
        let service = crate::services::collaboration::polls::PollService::new(self.core.clone());
        let mut items = vec![];
        let mut ids = vec![];
        for poll in &polls {
            ids.push(poll.id);
            let results = service.report_results(&self.core.db, poll).await?;
            items.push(json!({
                "pollId": poll.id,
                "question": poll.question,
                "status": poll.status,
                "anonymous": poll.is_anonymous,
                "results": results,
            }));
        }
        Ok((
            json!({ "conferenceId": conference.id, "generatedAt": Utc::now(), "polls": items }),
            json!({ "pollIds": ids, "aggregation": "AGGREGATE_ONLY_ANONYMOUS_PRESERVED" }),
        ))
    }

    async fn chat_export(
        &self,
        conference: &conference_sessions::Model,
    ) -> ApiResult<(Value, Value)> {
        let rows = self
            .core
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT m.id, m.body, m.created_at, p.display_name
                   FROM conference_messages m
                   LEFT JOIN conference_participants p ON p.id = m.sender_participant_id
                  WHERE m.conference_id = $1 AND m.breakout_room_id IS NULL
                    AND m.recipient_participant_id IS NULL AND m.deleted_at IS NULL
                  ORDER BY m.created_at, m.id
                  LIMIT 50000",
                [conference.id.into()],
            ))
            .await?;
        let messages: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "messageId": r.try_get::<Uuid>("", "id").ok(),
                    "body": r.try_get::<String>("", "body").ok(),
                    "sentAt": r.try_get::<chrono::DateTime<Utc>>("", "created_at").ok(),
                    "sender": r.try_get::<Option<String>>("", "display_name").ok().flatten(),
                })
            })
            .collect();
        Ok((
            json!({ "conferenceId": conference.id, "generatedAt": Utc::now(), "messages": messages }),
            json!({ "conversationScope": "CONFERENCE", "exportPolicy": "MAIN_CHAT_ONLY_NO_PRIVATE", "messageCount": messages.len() }),
        ))
    }

    /// Due work for the artefact worker (bounded): PROCESSING finalization retries and
    /// deletions (manual or retention).
    pub async fn due_finalize(&self, batch: u64) -> ApiResult<Vec<Uuid>> {
        Ok(meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::Status.eq("PROCESSING"))
            .filter(
                sea_orm::Condition::any()
                    .add(meeting_artifacts::Column::NextAttemptAt.is_null())
                    .add(meeting_artifacts::Column::NextAttemptAt.lte(Utc::now())),
            )
            .order_by_asc(meeting_artifacts::Column::UpdatedAt)
            .limit(batch)
            .all(&self.core.db)
            .await?
            .into_iter()
            .map(|a| a.id)
            .collect())
    }

    pub async fn due_deletion(&self, batch: u64) -> ApiResult<Vec<Uuid>> {
        let rows = self
            .core
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM meeting_artifacts
                  WHERE status NOT IN ('DELETED','REQUESTED','RECORDING','STOPPING')
                    AND (delete_requested_at IS NOT NULL OR retention_at <= now())
                    AND (next_attempt_at IS NULL OR next_attempt_at <= now()
                         OR (delete_requested_at IS NULL AND status = 'READY'))
                    AND (locked_until IS NULL OR locked_until < now())
                  ORDER BY COALESCE(delete_requested_at, retention_at)
                  LIMIT $1",
                [(batch as i64).into()],
            ))
            .await?;
        Ok(rows
            .iter()
            .filter_map(|r| r.try_get_by_index::<Uuid>(0).ok())
            .collect())
    }

    /// Spot-check READY artefacts against storage (ARTIFACT_MISSING incidents, never
    /// destructive).
    pub async fn verify_ready(&self, batch: u64) -> ApiResult<u64> {
        let stale = meeting_artifacts::Entity::find()
            .filter(meeting_artifacts::Column::Status.eq("READY"))
            .filter(meeting_artifacts::Column::DeleteRequestedAt.is_null())
            .filter(
                sea_orm::Condition::any()
                    .add(meeting_artifacts::Column::VerifiedAt.is_null())
                    .add(
                        meeting_artifacts::Column::VerifiedAt.lt(Utc::now() - Duration::hours(24)),
                    ),
            )
            .limit(batch)
            .all(&self.core.db)
            .await?;
        let mut missing = 0;
        for a in stale {
            let Some(key) = a.storage_key.as_deref() else {
                continue;
            };
            match self.core.artifacts.storage.head(key).await {
                Ok(Some(_)) => {
                    self.core
                        .db
                        .execute(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "UPDATE meeting_artifacts SET verified_at = now() WHERE id = $1",
                            [a.id.into()],
                        ))
                        .await?;
                }
                Ok(None) => {
                    missing += 1;
                    inc(&OM.artifact_missing);
                    let _ = incidents::record(
                        &self.core.db,
                        &Incident {
                            kind: kind::ARTIFACT_MISSING,
                            category: "ARTIFACT",
                            conference_id: Some(a.conference_id),
                            room_name: None,
                            subject: Some(format!("artifact:{}", a.id)),
                            action: IncidentAction::Escalated,
                            critical: true,
                            detail: json!({ "artifactId": a.id, "reason": "READY object missing in storage" }),
                        },
                    )
                    .await;
                }
                Err(_) => {}
            }
        }
        Ok(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(backoff(0), Duration::seconds(5));
        assert!(backoff(50) <= Duration::seconds(3600));
    }
}

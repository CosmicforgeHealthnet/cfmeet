//! Orphaned protected objects (Part H §79.26).
//!
//! Capture delivery writes the voicemail / recording object BEFORE the state transaction
//! that makes it READY. When the stored object then fails integrity validation, when the
//! finalizing transaction fails, or when the row no longer awaits it, the object would be
//! left in the bucket with nothing referencing it. [`AppCallService::discard_written_object`]
//! deletes it at once (idempotent); if storage refuses, the key is queued in
//! `calling_orphan_objects` — the ONLY place it is kept — and the recording worker tick
//! ([`AppCallService::orphan_tick`]) retries with backoff until the delete succeeds.
//!
//! Safety: an orphan whose owning row now references the same key (a later delivery
//! succeeded) is dropped from the queue without touching the object, and an owner that
//! may still be finalizing (capture in progress) defers the delete. No object key ever
//! reaches a log, metric, incident evidence, audit row or event.

use chrono::Duration;
use sea_orm::FromQueryResult;
use serde_json::json;
use uuid::Uuid;

use super::{exec, stmt};
use crate::calls::{
    app::{
        incidents::{self, Incident, IncidentCategory, IncidentResource, IncidentSeverity},
        AppCallService,
    },
    command::CallScope,
    errors::CallResult,
};

/// How long a claimed orphan is leased to one worker before another may retry it.
const ORPHAN_LEASE_SECONDS: i64 = 300;
/// First retry delay of a queued orphan (then doubling, capped at one hour).
const ORPHAN_FIRST_RETRY_SECONDS: i64 = 30;

/// Which kind of protected artifact an object belonged to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactKind {
    Recording,
    Voicemail,
}

impl ArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Recording => "RECORDING",
            ArtifactKind::Voicemail => "VOICEMAIL",
        }
    }

    fn incident(self) -> (IncidentCategory, IncidentResource) {
        match self {
            ArtifactKind::Recording => (
                IncidentCategory::RecordingArtifactFailure,
                IncidentResource::CallRecording,
            ),
            ArtifactKind::Voicemail => (
                IncidentCategory::VoicemailFinalizationFailure,
                IncidentResource::VoicemailMessage,
            ),
        }
    }
}

fn orphan_metric(result: &'static str) {
    crate::metrics::inc("calling_orphan_objects_total", &[("result", result)]);
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds(
        ORPHAN_FIRST_RETRY_SECONDS
            .saturating_mul(2_i64.saturating_pow(attempts.clamp(0, 16) as u32))
            .min(3600),
    )
}

#[derive(Debug, FromQueryResult)]
struct ClaimedOrphan {
    id: Uuid,
    artifact_kind: String,
    artifact_id: Uuid,
    object_key: String,
    attempts: i32,
}

#[derive(Debug, FromQueryResult)]
struct Owner {
    state: String,
    object_key: Option<String>,
}

impl AppCallService {
    /// A just-written object will not be referenced (integrity mismatch, failed or
    /// superseded finalization): delete it now, or queue it for the worker. `reason` is a
    /// bounded code. Never fails the caller's operation.
    pub(crate) async fn discard_written_object(
        &self,
        scope: CallScope,
        kind: ArtifactKind,
        artifact_id: Uuid,
        call_id: Option<Uuid>,
        key: &str,
        reason: &'static str,
    ) {
        // Never remove an object its owner references (a concurrent delivery may have
        // finalized this very key). If the owner cannot be read, keep the object: a
        // stray object is recoverable, a READY artifact without its object is not.
        match self.owner(kind.as_str(), artifact_id).await {
            Ok(Some(owner)) if owner.object_key.as_deref() == Some(key) => return,
            Ok(_) => {}
            Err(_) => {
                orphan_metric("OWNER_UNREADABLE");
                tracing::error!(
                    artifact = kind.as_str(),
                    %artifact_id,
                    reason,
                    "unreferenced artifact object kept: its owner could not be read"
                );
                return;
            }
        }
        if self.artifacts.delete(key).await.is_ok() {
            orphan_metric("DELETED");
            tracing::info!(
                artifact = kind.as_str(),
                %artifact_id,
                reason,
                "unreferenced artifact object deleted"
            );
            return;
        }
        let now = self.calls.now();
        let queued = exec(
            self.db(),
            "INSERT INTO calling_orphan_objects (id, tenant_id, application_id, environment_id, \
               artifact_kind, artifact_id, object_key, reason, attempts, next_attempt_at, \
               created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, $9, $10, $10) \
             ON CONFLICT (object_key) DO UPDATE SET \
               next_attempt_at = LEAST(calling_orphan_objects.next_attempt_at, EXCLUDED.next_attempt_at), \
               reason = EXCLUDED.reason, \
               updated_at = GREATEST(calling_orphan_objects.updated_at, EXCLUDED.updated_at)",
            vec![
                Uuid::new_v4().into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                kind.as_str().into(),
                artifact_id.into(),
                key.into(),
                reason.into(),
                (now + backoff(0)).into(),
                now.into(),
            ],
        )
        .await;
        match queued {
            Ok(_) => {
                orphan_metric("QUEUED");
                tracing::warn!(
                    artifact = kind.as_str(),
                    %artifact_id,
                    reason,
                    "unreferenced artifact object could not be deleted; queued for retry"
                );
            }
            Err(error) => {
                orphan_metric("QUEUE_FAILED");
                tracing::error!(
                    artifact = kind.as_str(),
                    %artifact_id,
                    reason,
                    %error,
                    "unreferenced artifact object could not be deleted nor queued"
                );
            }
        }
        let (category, resource) = kind.incident();
        let mut incident = Incident::new(
            category,
            IncidentSeverity::Medium,
            scope,
            resource,
            artifact_id,
        )
        .evidence(json!({ "orphanObject": true, "reason": reason }));
        if let Some(call_id) = call_id {
            incident = incident.call(call_id);
        }
        incidents::raise(self.db(), &incident, now).await;
    }

    async fn owner(&self, kind: &str, id: Uuid) -> CallResult<Option<Owner>> {
        let sql = if kind == ArtifactKind::Recording.as_str() {
            "SELECT state, object_key FROM call_recordings WHERE id = $1"
        } else {
            "SELECT state, object_key FROM voicemail_messages WHERE id = $1"
        };
        Ok(Owner::find_by_statement(stmt(sql, vec![id.into()]))
            .one(self.db())
            .await?)
    }

    /// Worker pass: retries queued orphan deletes (lease claim, `SKIP LOCKED`, bounded).
    /// Returns the number of objects deleted.
    pub(crate) async fn orphan_tick(&self, limit: u64) -> CallResult<usize> {
        let now = self.calls.now();
        let claimed = ClaimedOrphan::find_by_statement(stmt(
            "UPDATE calling_orphan_objects o SET next_attempt_at = $2, attempts = o.attempts + 1, \
               updated_at = GREATEST(o.updated_at, $1) \
             WHERE o.id IN (SELECT id FROM calling_orphan_objects WHERE next_attempt_at <= $1 \
                             ORDER BY next_attempt_at LIMIT $3 FOR UPDATE SKIP LOCKED) \
             RETURNING o.id, o.artifact_kind, o.artifact_id, o.object_key, o.attempts",
            vec![
                now.into(),
                (now + Duration::seconds(ORPHAN_LEASE_SECONDS)).into(),
                (limit as i64).into(),
            ],
        ))
        .all(self.db())
        .await?;
        let mut deleted = 0;
        for orphan in claimed {
            let owner = self
                .owner(&orphan.artifact_kind, orphan.artifact_id)
                .await?;
            if let Some(owner) = &owner {
                if owner.object_key.as_deref() == Some(orphan.object_key.as_str()) {
                    // A later delivery made this exact object the artifact: keep it.
                    exec(
                        self.db(),
                        "DELETE FROM calling_orphan_objects WHERE id = $1",
                        vec![orphan.id.into()],
                    )
                    .await?;
                    orphan_metric("REFERENCED");
                    continue;
                }
                if matches!(
                    owner.state.as_str(),
                    "STARTING" | "RECORDING" | "STOPPING" | "PROCESSING"
                ) {
                    // A delivery may still be finalizing under this key: later.
                    exec(
                        self.db(),
                        "UPDATE calling_orphan_objects SET next_attempt_at = $2 WHERE id = $1",
                        vec![orphan.id.into(), (now + backoff(orphan.attempts)).into()],
                    )
                    .await?;
                    orphan_metric("DEFERRED");
                    continue;
                }
            }
            match self.artifacts.delete(&orphan.object_key).await {
                Ok(()) => {
                    exec(
                        self.db(),
                        "DELETE FROM calling_orphan_objects WHERE id = $1",
                        vec![orphan.id.into()],
                    )
                    .await?;
                    orphan_metric("DELETED");
                    deleted += 1;
                }
                Err(_) => {
                    exec(
                        self.db(),
                        "UPDATE calling_orphan_objects SET next_attempt_at = $2 WHERE id = $1",
                        vec![orphan.id.into(), (now + backoff(orphan.attempts)).into()],
                    )
                    .await?;
                    orphan_metric("RETRY");
                    tracing::warn!(
                        orphan_id = %orphan.id,
                        attempts = orphan.attempts,
                        "orphaned artifact object deletion pending; retried"
                    );
                }
            }
        }
        Ok(deleted)
    }
}

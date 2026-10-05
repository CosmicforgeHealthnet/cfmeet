//! ArtifactRetentionWorker (Part G §28) — bounded, restart-safe, multi-instance safe.
//!
//! Each tick:
//! 1. finalizes due PROCESSING artefacts (object verification retries with backoff);
//! 2. deletes artefacts whose retention date passed or whose deletion was requested:
//!    delete object → confirm absence → DELETED (+ audit + event); transient failures
//!    retry with backoff, repeated failures escalate as incidents;
//! 3. spot-checks READY artefacts against storage (ARTIFACT_MISSING incidents).
//!
//! Concurrency: every artefact is claimed with a row lease (`locked_by` /
//! `locked_until`, conditional UPDATE), so two workers — or a worker and a manual
//! delete — never process the same artefact; a crashed worker's lease simply expires.

use std::time::Duration;

use tokio::sync::watch;

use super::service::{ArtifactService, DeleteOutcome, FinalizeOutcome};
use crate::{
    error::ApiResult,
    services::{
        meeting_core::Core,
        ops_metrics::{inc, OPS_METRICS as OM},
    },
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub finalized: u64,
    pub finalize_pending: u64,
    pub finalize_failed: u64,
    pub deleted: u64,
    pub delete_pending: u64,
    pub missing: u64,
}

pub struct ArtifactRetentionWorker {
    core: Core,
    service: ArtifactService,
}

impl ArtifactRetentionWorker {
    pub fn new(core: Core) -> Self {
        Self {
            service: ArtifactService::new(core.clone()),
            core,
        }
    }

    pub async fn tick(&self) -> ApiResult<RetentionReport> {
        inc(&OM.retention_runs);
        let batch = self.core.config.artifacts.retention_batch;
        let mut report = RetentionReport::default();
        for id in self.service.due_finalize(batch).await? {
            match self.service.finalize(id).await {
                Ok(FinalizeOutcome::Ready) => report.finalized += 1,
                Ok(FinalizeOutcome::Pending) => report.finalize_pending += 1,
                Ok(FinalizeOutcome::Failed) => report.finalize_failed += 1,
                Ok(FinalizeOutcome::Skipped) => {}
                Err(e) => tracing::warn!(artifact_id = %id, "finalize failed: {e}"),
            }
        }
        for id in self.service.due_deletion(batch).await? {
            match self.service.delete_now(id).await {
                Ok(DeleteOutcome::Deleted) => report.deleted += 1,
                Ok(DeleteOutcome::Pending) => report.delete_pending += 1,
                Ok(DeleteOutcome::Skipped) => {}
                Err(e) => tracing::warn!(artifact_id = %id, "artifact deletion failed: {e}"),
            }
        }
        report.missing = self.service.verify_ready(batch.min(20)).await.unwrap_or(0);
        Ok(report)
    }
}

pub fn spawn_artifact_worker(
    worker: ArtifactRetentionWorker,
    poll_secs: u64,
) -> watch::Sender<bool> {
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(poll_secs.max(1)));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = worker.tick().await {
                        tracing::warn!("artifact worker tick failed: {e}");
                    }
                }
                _ = rx.changed() => break,
            }
        }
    });
    tx
}

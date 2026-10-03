//! ConferenceScheduler — idempotent, multi-instance-safe v2 scheduling worker
//! (spec §C16, §C18, §3.6). Replaces the legacy auto-start/auto-end workers for v2
//! conferences (the legacy workers keep serving legacy `meetings` rows).
//!
//! Each tick:
//! 1. materializes near-future recurring occurrences (rules claimed with
//!    `FOR UPDATE SKIP LOCKED`, inserts `ON CONFLICT DO NOTHING`);
//! 2. requests the end (LIVE → ENDING, reason SCHEDULED_END) of conferences past their
//!    scheduled end + auto-end grace. `end` locks the row and is idempotent, so
//!    duplicate execution by several instances or after a crash is harmless.
//!
//! No auto-start: starting is an authorized host decision. Notifications/reminders are
//! never authorization dependencies.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use tokio::sync::watch;

use crate::{
    domain::{ConferenceStatus, RecordOrigin, TerminationReason},
    error::ApiResult,
    models::{conference_sessions, meeting_spaces},
    services::{
        actor::ActorContext, meeting_domain::MeetingDomain, meeting_space::schedule_policy,
    },
};

const BATCH: u64 = 50;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerTick {
    pub materialized: u64,
    pub end_requested: u64,
}

pub struct ConferenceScheduler {
    meeting: Arc<MeetingDomain>,
}

impl ConferenceScheduler {
    pub fn new(meeting: Arc<MeetingDomain>) -> Self {
        Self { meeting }
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> ApiResult<SchedulerTick> {
        let materialized = self.meeting.recurrence.materialize_due(now, BATCH).await?;
        let end_requested = self.auto_end(now).await?;
        Ok(SchedulerTick {
            materialized,
            end_requested,
        })
    }

    async fn auto_end(&self, now: DateTime<Utc>) -> ApiResult<u64> {
        let db = &self.meeting.core.db;
        let candidates = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::Status.eq(ConferenceStatus::Live))
            .filter(conference_sessions::Column::Origin.eq(RecordOrigin::V2))
            .filter(conference_sessions::Column::ScheduledEndAt.lt(now))
            .limit(BATCH)
            .all(db)
            .await?;
        let system = ActorContext::system("conference-scheduler");
        let mut ended = 0;
        for c in candidates {
            let Some(space) = meeting_spaces::Entity::find_by_id(c.meeting_space_id)
                .one(db)
                .await?
            else {
                continue;
            };
            let policy = schedule_policy(&space);
            let due = c
                .scheduled_end_at
                .map(|end| end + Duration::minutes(policy.auto_end_grace_minutes as i64) <= now)
                .unwrap_or(false);
            if !policy.auto_end || !due {
                continue;
            }
            match self
                .meeting
                .conferences
                .end(&system, c.id, TerminationReason::ScheduledEnd)
                .await
            {
                Ok(outcome) if outcome.changed => ended += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(conference_id = %c.id, "scheduled end failed: {e}"),
            }
        }
        Ok(ended)
    }

    pub async fn run(&self, poll_secs: u64, mut shutdown: watch::Receiver<bool>) {
        tracing::info!("Conference scheduler started (polling every {poll_secs}s)");
        loop {
            if *shutdown.borrow() {
                break;
            }
            match self.tick(Utc::now()).await {
                Ok(t) if t != SchedulerTick::default() => {
                    tracing::info!(?t, "conference scheduler tick")
                }
                Ok(_) => {}
                // PostgreSQL unavailable: do nothing (never act on stale state), retry next tick.
                Err(e) => tracing::error!("conference scheduler tick failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
                _ = shutdown.changed() => {}
            }
        }
        tracing::info!("Conference scheduler stopped");
    }
}

pub struct ConferenceSchedulerHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl ConferenceSchedulerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

pub fn spawn_conference_scheduler(meeting: Arc<MeetingDomain>) -> ConferenceSchedulerHandle {
    let poll = meeting.core.config.scheduler_poll_secs.max(5);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let scheduler = ConferenceScheduler::new(meeting);
    tokio::spawn(async move { scheduler.run(poll, shutdown_rx).await });
    ConferenceSchedulerHandle { shutdown_tx }
}

//! Meeting Auto-End Worker
//!
//! Background worker that automatically ends meetings that have passed their scheduled end time.
//! Runs periodically to check for ongoing meetings that should be ended.
//!
//! ## Configuration
//!
//! - `POLL_INTERVAL_SECS` - How often to check for meetings to end (default: 60 seconds)
//! - `BATCH_SIZE` - Max meetings to process per poll (default: 20)
//!
//! ## Behavior
//!
//! - Only affects meetings with status = 'ongoing' and a scheduled end_time
//! - Audit M2: ends a meeting only once its scheduled end plus the v2 default auto-end
//!   grace ([`LEGACY_AUTO_END_GRACE_MINUTES`], 15 minutes) has passed, and warns everyone
//!   in its v2 conference [`AUTO_END_WARNING_MINUTES`] before that (`conference.ending_soon`);
//!   a host extends it with `POST /api/v2/conferences/{id}/extend` (MF1)
//! - Transitions meeting status to 'ended'
//! - Marks all active participants as left
//! - Logs meeting end event
//! - Does NOT affect meetings without an end_time (they can run indefinitely)

use sea_orm::{
    sea_query::{Alias, Expr},
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use tokio::sync::watch;
use tokio::time::{sleep, Duration};

use crate::models::{
    chat_messages::{self, Entity as ChatMessages},
    meetings::{self, Entity as Meetings, MeetingStatus},
    participants::{self, Entity as Participants},
    session_logs::{self, EventType},
};
use crate::services::conference::{AUTO_END_WARNING_MINUTES, LEGACY_AUTO_END_GRACE_MINUTES};
use crate::utils::now_utc;

// ============================================================================
// CONFIGURATION
// ============================================================================

/// How often to poll for meetings to end (seconds)
const POLL_INTERVAL_SECS: u64 = 60;

/// Maximum meetings to process per batch
const BATCH_SIZE: u64 = 20;

// ============================================================================
// MEETING AUTO-END WORKER
// ============================================================================

/// Background worker for auto-ending meetings
pub struct MeetingAutoEndWorker {
    db: DatabaseConnection,
    /// Gap audit M7: the provider whose legacy room is terminated when a meeting ends.
    rtc: Option<std::sync::Arc<dyn crate::services::rtc_provider::RtcProvider>>,
    /// Gap audit M9: keeps the v2 view of each changed meeting current.
    mirror: Option<std::sync::Arc<crate::services::compat::CompatService>>,
}

impl MeetingAutoEndWorker {
    /// Create a new MeetingAutoEndWorker
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            db,
            rtc: None,
            mirror: None,
        }
    }

    /// Gap audit M9: mirror each meeting this worker changes into its v2 view.
    pub fn with_mirror(
        mut self,
        compat: std::sync::Arc<crate::services::compat::CompatService>,
    ) -> Self {
        self.mirror = Some(compat);
        self
    }

    /// Also terminate each ended meeting's media room through `rtc`.
    pub fn with_rtc(
        mut self,
        rtc: std::sync::Arc<dyn crate::services::rtc_provider::RtcProvider>,
    ) -> Self {
        self.rtc = Some(rtc);
        self
    }

    /// Start the worker loop
    ///
    /// Runs until shutdown signal is received.
    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) {
        tracing::info!(
            "Meeting auto-end worker started (polling every {}s)",
            POLL_INTERVAL_SECS
        );

        loop {
            // Check for shutdown
            if *shutdown_rx.borrow() {
                tracing::info!("Meeting auto-end worker shutting down");
                break;
            }

            // Warn first, then end what is past its grace period.
            if let Err(e) = self.warn_batch().await {
                tracing::error!("Error warning legacy meetings about their end: {}", e);
            }
            if let Err(e) = self.process_batch().await {
                tracing::error!("Error processing meeting auto-end batch: {}", e);
            }

            // Wait before next poll
            tokio::select! {
                _ = sleep(Duration::from_secs(POLL_INTERVAL_SECS)) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Meeting auto-end worker shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Process a batch of meetings that should be auto-ended.
    /// Multi-instance safe: returns how many meetings THIS call ended.
    pub async fn process_batch(&self) -> Result<u64, String> {
        // Audit M2: the scheduled end plus the grace period has passed.
        let cutoff = now_utc() - chrono::Duration::minutes(LEGACY_AUTO_END_GRACE_MINUTES);

        // Find ongoing meetings that have passed their scheduled end time + grace
        let meetings = Meetings::find()
            .filter(meetings::Column::Status.eq(MeetingStatus::Ongoing))
            .filter(meetings::Column::EndTime.is_not_null())
            .filter(meetings::Column::EndTime.lte(cutoff))
            .order_by_asc(meetings::Column::EndTime)
            .limit(BATCH_SIZE)
            .all(&self.db)
            .await
            .map_err(|e| format!("Failed to fetch meetings: {}", e))?;

        if meetings.is_empty() {
            tracing::debug!("No meetings to auto-end");
            return Ok(0);
        }

        let mut ended = 0;
        for meeting in meetings {
            let meeting_id = meeting.id;
            match self.end_meeting(meeting).await {
                Ok(true) => ended += 1,
                Ok(false) => {}
                Err(e) => tracing::error!("Failed to auto-end meeting {}: {}", meeting_id, e),
            }
        }
        if ended > 0 {
            tracing::info!("Auto-ended {} meeting(s)", ended);
        }
        Ok(ended)
    }

    /// Audit M2 / MF1: `conference.ending_soon` on the v2 conference of each ongoing legacy
    /// meeting whose automatic end (scheduled end + grace) is at most
    /// [`AUTO_END_WARNING_MINUTES`] away. Once per automatic-end instant; returns how many
    /// warnings THIS call emitted.
    pub async fn warn_batch(&self) -> Result<u64, String> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let now = chrono::Utc::now();
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT c.id, (m.end_time AT TIME ZONE 'UTC') + make_interval(mins => $2) AS auto_end_at \
                   FROM meetings m JOIN conference_sessions c ON c.legacy_meeting_id = m.id \
                  WHERE m.status = 'ongoing' AND m.end_time IS NOT NULL AND c.status = 'LIVE' \
                    AND (m.end_time AT TIME ZONE 'UTC') + make_interval(mins => $2) > $1 \
                    AND (m.end_time AT TIME ZONE 'UTC') + make_interval(mins => $2) \
                        <= $1 + make_interval(mins => $3) \
                  ORDER BY m.end_time LIMIT $4",
                [
                    now.into(),
                    (LEGACY_AUTO_END_GRACE_MINUTES as i32).into(),
                    (AUTO_END_WARNING_MINUTES as i32).into(),
                    (BATCH_SIZE as i64).into(),
                ],
            ))
            .await
            .map_err(|e| format!("Failed to fetch meetings to warn: {e}"))?;
        let mut warned = 0;
        for r in rows {
            let (Ok(id), Ok(at)) = (
                r.try_get::<uuid::Uuid>("", "id"),
                r.try_get::<chrono::DateTime<chrono::Utc>>("", "auto_end_at"),
            ) else {
                continue;
            };
            match crate::workers::conference_scheduler::warn_ending_soon(&self.db, id, at, now)
                .await
            {
                Ok(true) => warned += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(conference_id = %id, "ending-soon warning failed: {e}"),
            }
        }
        Ok(warned)
    }

    /// End a single meeting. The `status = 'ongoing'` conditional update and every
    /// follow-up write share one transaction, so concurrent worker instances end a
    /// meeting — and log it — exactly once.
    async fn end_meeting(&self, meeting: meetings::Model) -> Result<bool, String> {
        let meeting_id = meeting.id;
        let meeting_identifier = meeting.meeting_identifier.clone();
        let now = now_utc();
        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        // Keep the original scheduled end_time, don't override with current time
        let claimed = Meetings::update_many()
            .col_expr(
                meetings::Column::Status,
                Expr::val("ended").as_enum(Alias::new("meeting_status")),
            )
            .col_expr(meetings::Column::UpdatedAt, Expr::value(now))
            .filter(meetings::Column::Id.eq(meeting_id))
            .filter(meetings::Column::Status.eq(MeetingStatus::Ongoing))
            .exec(&txn)
            .await
            .map_err(|e| format!("Failed to update meeting status: {}", e))?
            .rows_affected;
        if claimed == 0 {
            // Another instance (or the host) already ended it.
            return Ok(false);
        }

        // Update all active participants to mark leave time
        let active_participants = Participants::find()
            .filter(participants::Column::MeetingId.eq(meeting_id))
            .filter(participants::Column::LeaveTime.is_null())
            .all(&txn)
            .await
            .map_err(|e| format!("Failed to fetch participants: {}", e))?;

        for p in active_participants {
            let mut participant: participants::ActiveModel = p.into();
            participant.leave_time = Set(Some(now));
            participant
                .update(&txn)
                .await
                .map_err(|e| format!("Failed to update participant: {}", e))?;
        }

        // Clear chat messages (volatile per session)
        ChatMessages::delete_many()
            .filter(chat_messages::Column::MeetingId.eq(meeting_id))
            .exec(&txn)
            .await
            .map_err(|e| format!("Failed to clear chat messages: {}", e))?;

        // Log the auto-end event
        let session_log = session_logs::ActiveModel {
            id: Set(uuid::Uuid::new_v4()),
            meeting_id: Set(meeting_id),
            participant_id: Set(None),
            event_type: Set(EventType::MeetingEnd),
            event_time: Set(now),
            metadata: Set(Some(serde_json::json!({
                "auto_ended": true,
                "reason": "scheduled_end_time_reached",
                "grace_minutes": LEGACY_AUTO_END_GRACE_MINUTES,
                "ended_at": now.to_string()
            }))),
        };

        session_log
            .insert(&txn)
            .await
            .map_err(|e| format!("Failed to log auto-end event: {}", e))?;
        txn.commit()
            .await
            .map_err(|e| format!("Failed to commit auto-end: {}", e))?;
        if let Some(compat) = &self.mirror {
            compat.mirror_legacy_write(meeting_id).await;
        }
        if let Some(rtc) = &self.rtc {
            crate::services::legacy_media::terminate(
                rtc.as_ref(),
                meeting_id,
                &meeting_identifier,
                "LEGACY_MEETING_AUTO_ENDED",
            )
            .await;
        }

        tracing::info!(
            "Auto-ended meeting {} ({}) at scheduled end time",
            meeting_id,
            meeting_identifier
        );

        Ok(true)
    }
}

// ============================================================================
// WORKER HANDLE
// ============================================================================

/// Handle for controlling the meeting auto-end worker
pub struct MeetingAutoEndWorkerHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl MeetingAutoEndWorkerHandle {
    /// Signal the worker to shut down
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Spawn the meeting auto-end worker as a background task
///
/// Returns a handle for shutdown.
pub fn spawn_meeting_auto_end_worker(
    db: DatabaseConnection,
    rtc: Option<std::sync::Arc<dyn crate::services::rtc_provider::RtcProvider>>,
    mirror: Option<std::sync::Arc<crate::services::compat::CompatService>>,
) -> MeetingAutoEndWorkerHandle {
    let mut worker = match rtc {
        Some(rtc) => MeetingAutoEndWorker::new(db).with_rtc(rtc),
        None => MeetingAutoEndWorker::new(db),
    };
    if let Some(m) = mirror {
        worker = worker.with_mirror(m);
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        worker.run(shutdown_rx).await;
    });

    tracing::info!("Meeting auto-end worker spawned");

    MeetingAutoEndWorkerHandle { shutdown_tx }
}

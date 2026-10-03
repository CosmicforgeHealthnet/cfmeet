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
//! - Transitions meeting status to 'ended'
//! - Marks all active participants as left
//! - Logs meeting end event
//! - Does NOT affect meetings without an end_time (they can run indefinitely)

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use tokio::sync::watch;
use tokio::time::{sleep, Duration};

use crate::utils::now_utc;
use crate::models::{
    chat_messages::{self, Entity as ChatMessages},
    meetings::{self, Entity as Meetings, MeetingStatus},
    participants::{self, Entity as Participants},
    session_logs::{self, EventType},
};

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
}

impl MeetingAutoEndWorker {
    /// Create a new MeetingAutoEndWorker
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
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

            // Process meetings that should be ended
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

    /// Process a batch of meetings that should be auto-ended
    async fn process_batch(&self) -> Result<(), String> {
        let now = now_utc();

        // Find ongoing meetings that have passed their scheduled end time
        let meetings = Meetings::find()
            .filter(meetings::Column::Status.eq(MeetingStatus::Ongoing))
            .filter(meetings::Column::EndTime.is_not_null())
            .filter(meetings::Column::EndTime.lte(now))
            .order_by_asc(meetings::Column::EndTime)
            .limit(BATCH_SIZE)
            .all(&self.db)
            .await
            .map_err(|e| format!("Failed to fetch meetings: {}", e))?;

        if meetings.is_empty() {
            tracing::debug!("No meetings to auto-end");
            return Ok(());
        }

        tracing::info!("Auto-ending {} meeting(s)", meetings.len());

        for meeting in meetings {
            let meeting_id = meeting.id;
            if let Err(e) = self.end_meeting(meeting).await {
                tracing::error!("Failed to auto-end meeting {}: {}", meeting_id, e);
            }
        }

        Ok(())
    }

    /// End a single meeting
    async fn end_meeting(&self, meeting: meetings::Model) -> Result<(), String> {
        let meeting_id = meeting.id;
        let meeting_identifier = meeting.meeting_identifier.clone();
        let now = now_utc();

        // Update meeting status to ended
        let mut meeting_update: meetings::ActiveModel = meeting.into();
        meeting_update.status = Set(MeetingStatus::Ended);
        meeting_update.updated_at = Set(now);
        // Keep the original scheduled end_time, don't override with current time

        meeting_update
            .update(&self.db)
            .await
            .map_err(|e| format!("Failed to update meeting status: {}", e))?;

        // Update all active participants to mark leave time
        let active_participants = Participants::find()
            .filter(participants::Column::MeetingId.eq(meeting_id))
            .filter(participants::Column::LeaveTime.is_null())
            .all(&self.db)
            .await
            .map_err(|e| format!("Failed to fetch participants: {}", e))?;

        for p in active_participants {
            let mut participant: participants::ActiveModel = p.into();
            participant.leave_time = Set(Some(now));
            participant
                .update(&self.db)
                .await
                .map_err(|e| format!("Failed to update participant: {}", e))?;
        }

        // Clear chat messages (volatile per session)
        ChatMessages::delete_many()
            .filter(chat_messages::Column::MeetingId.eq(meeting_id))
            .exec(&self.db)
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
                "ended_at": now.to_string()
            }))),
        };

        session_log
            .insert(&self.db)
            .await
            .map_err(|e| format!("Failed to log auto-end event: {}", e))?;

        tracing::info!(
            "Auto-ended meeting {} ({}) at scheduled end time",
            meeting_id,
            meeting_identifier
        );

        Ok(())
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
pub fn spawn_meeting_auto_end_worker(db: DatabaseConnection) -> MeetingAutoEndWorkerHandle {
    let worker = MeetingAutoEndWorker::new(db);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        worker.run(shutdown_rx).await;
    });

    tracing::info!("Meeting auto-end worker spawned");

    MeetingAutoEndWorkerHandle { shutdown_tx }
}

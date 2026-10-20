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

    /// Process a batch of meetings that should be auto-ended.
    /// Multi-instance safe: returns how many meetings THIS call ended.
    pub async fn process_batch(&self) -> Result<u64, String> {
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
pub fn spawn_meeting_auto_end_worker(db: DatabaseConnection) -> MeetingAutoEndWorkerHandle {
    let worker = MeetingAutoEndWorker::new(db);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        worker.run(shutdown_rx).await;
    });

    tracing::info!("Meeting auto-end worker spawned");

    MeetingAutoEndWorkerHandle { shutdown_tx }
}

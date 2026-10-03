//! Meeting Auto-Start Worker
//!
//! Background worker that automatically transitions scheduled meetings to live (ongoing)
//! when their start time is reached.
//!
//! ## Configuration
//!
//! - `POLL_INTERVAL_SECS` - How often to check for meetings to start (default: 30 seconds)
//! - `BATCH_SIZE` - Max meetings to process per poll (default: 20)
//!
//! ## Behavior
//!
//! - Only affects meetings with status = 'scheduled' whose start_time <= now (UTC)
//! - Transitions meeting status from 'scheduled' to 'ongoing'
//! - Logs a MeetingStart event in session_logs

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use tokio::sync::watch;
use tokio::time::{sleep, Duration};

use crate::models::{
    meetings::{self, Entity as Meetings, MeetingStatus},
    session_logs::{self, EventType},
};
use crate::utils::now_utc;

// Configuration
const POLL_INTERVAL_SECS: u64 = 30;
const BATCH_SIZE: u64 = 20;

pub struct MeetingAutoStartWorker {
    db: DatabaseConnection,
}

impl MeetingAutoStartWorker {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// Start the worker loop — runs until shutdown signal
    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) {
        tracing::info!(
            "Meeting auto-start worker started (polling every {}s)",
            POLL_INTERVAL_SECS
        );

        loop {
            if *shutdown_rx.borrow() {
                tracing::info!("Meeting auto-start worker shutting down");
                break;
            }

            if let Err(e) = self.process_batch().await {
                tracing::error!("Error processing meeting auto-start batch: {}", e);
            }

            tokio::select! {
                _ = sleep(Duration::from_secs(POLL_INTERVAL_SECS)) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Meeting auto-start worker shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Process a batch of scheduled meetings whose start time has been reached
    async fn process_batch(&self) -> Result<(), String> {
        let now = now_utc();

        let meetings = Meetings::find()
            .filter(meetings::Column::Status.eq(MeetingStatus::Scheduled))
            .filter(meetings::Column::StartTime.lte(now))
            .order_by_asc(meetings::Column::StartTime)
            .limit(BATCH_SIZE)
            .all(&self.db)
            .await
            .map_err(|e| format!("Failed to fetch scheduled meetings: {}", e))?;

        if meetings.is_empty() {
            tracing::debug!("No meetings to auto-start");
            return Ok(());
        }

        tracing::info!("Auto-starting {} meeting(s)", meetings.len());

        for meeting in meetings {
            let meeting_id = meeting.id;
            if let Err(e) = self.start_meeting(meeting).await {
                tracing::error!("Failed to auto-start meeting {}: {}", meeting_id, e);
            }
        }

        Ok(())
    }

    /// Transition a single meeting from Scheduled to Ongoing
    async fn start_meeting(&self, meeting: meetings::Model) -> Result<(), String> {
        let meeting_id = meeting.id;
        let meeting_identifier = meeting.meeting_identifier.clone();
        let now = now_utc();

        // Update meeting status to ongoing
        let mut meeting_update: meetings::ActiveModel = meeting.into();
        meeting_update.status = Set(MeetingStatus::Ongoing);
        meeting_update.updated_at = Set(now);

        meeting_update
            .update(&self.db)
            .await
            .map_err(|e| format!("Failed to update meeting status: {}", e))?;

        // Log the meeting start event
        let session_log = session_logs::ActiveModel {
            id: Set(uuid::Uuid::new_v4()),
            meeting_id: Set(meeting_id),
            participant_id: Set(None),
            event_type: Set(EventType::MeetingStart),
            event_time: Set(now),
            metadata: Set(Some(serde_json::json!({
                "auto_started": true,
                "reason": "scheduled_start_time_reached",
                "started_at": now.to_string()
            }))),
        };

        session_log
            .insert(&self.db)
            .await
            .map_err(|e| format!("Failed to log auto-start event: {}", e))?;

        tracing::info!(
            "Auto-started meeting {} ({}) at scheduled start time",
            meeting_id,
            meeting_identifier
        );

        Ok(())
    }
}

/// Handle for graceful shutdown
pub struct MeetingAutoStartWorkerHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl MeetingAutoStartWorkerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Spawn the meeting auto-start worker as a background task
pub fn spawn_meeting_auto_start_worker(db: DatabaseConnection) -> MeetingAutoStartWorkerHandle {
    let worker = MeetingAutoStartWorker::new(db);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        worker.run(shutdown_rx).await;
    });

    tracing::info!("Meeting auto-start worker spawned");

    MeetingAutoStartWorkerHandle { shutdown_tx }
}

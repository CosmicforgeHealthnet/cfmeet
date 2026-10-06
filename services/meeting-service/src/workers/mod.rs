//! Background Workers
//!
//! This module contains all background workers that process async jobs.
//! Workers run in separate Tokio tasks and poll the database for work.
//!
//! ## Available Workers
//!
//! - `email` - Processes queued email jobs
//! - `meeting_auto_end` - Auto-ends meetings at scheduled end time
//! - `meeting_auto_start` - Auto-starts meetings at scheduled start time
//! - `conference_scheduler` - Phase 2: recurrence materialization + scheduled end (v2)
//! - `outbox_relay` - Phase 2: transactional outbox delivery (realtime fanout + provider commands)
//! - `conference_reconciler` - Phase 2 Part E: PostgreSQL ↔ RTC provider drift repair
//!
//! ## Adding a New Worker
//!
//! 1. Create a new file in this folder (e.g., `notification.rs`)
//! 2. Implement the worker with a `run()` method
//! 3. Export it from this mod.rs
//! 4. Start it in main.rs
//!
//! See `/docs/EMAIL_QUEUE.md` for the email worker example.

pub mod breakout_closer;
pub mod conference_reconciler;
pub mod conference_scheduler;
pub mod email;
pub mod meeting_auto_end;
pub mod meeting_auto_start;
pub mod outbox_relay;

pub use breakout_closer::{spawn_breakout_closer, BreakoutCloser};
pub use conference_reconciler::{
    spawn_conference_reconciler, ConferenceReconciler, ReconcileReport,
};
pub use conference_scheduler::{
    spawn_conference_scheduler, ConferenceScheduler, ConferenceSchedulerHandle,
};
pub use email::{spawn_email_worker, EmailWorker, EmailWorkerHandle};
pub use meeting_auto_end::{
    spawn_meeting_auto_end_worker, MeetingAutoEndWorker, MeetingAutoEndWorkerHandle,
};
pub use meeting_auto_start::{
    spawn_meeting_auto_start_worker, MeetingAutoStartWorker, MeetingAutoStartWorkerHandle,
};

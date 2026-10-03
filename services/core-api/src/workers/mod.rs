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
//!
//! ## Adding a New Worker
//!
//! 1. Create a new file in this folder (e.g., `notification.rs`)
//! 2. Implement the worker with a `run()` method
//! 3. Export it from this mod.rs
//! 4. Start it in main.rs
//!
//! See `/docs/EMAIL_QUEUE.md` for the email worker example.

pub mod email;
pub mod meeting_auto_end;
pub mod meeting_auto_start;

pub use email::{spawn_email_worker, EmailWorker, EmailWorkerHandle};
pub use meeting_auto_end::{
    spawn_meeting_auto_end_worker, MeetingAutoEndWorker, MeetingAutoEndWorkerHandle,
};
pub use meeting_auto_start::{
    spawn_meeting_auto_start_worker, MeetingAutoStartWorker, MeetingAutoStartWorkerHandle,
};

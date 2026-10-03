// Re-export all entity models
pub mod api_keys;
pub mod audio_video_devices;
pub mod chat_messages;
pub mod meetings;
pub mod participants;
pub mod session_logs;
pub mod users;
pub mod webhooks;

// Phase 2 meeting domain (spec v1.1 Part B)
pub mod breakout_assignments;
pub mod breakout_rooms;
pub mod conference_audit_events;
pub mod conference_bans;
pub mod conference_participants;
pub mod conference_sessions;
pub mod meeting_artifacts;
pub mod meeting_consents;
pub mod meeting_invitations;
pub mod meeting_members;
pub mod meeting_poll_responses;
pub mod meeting_polls;
pub mod meeting_question_votes;
pub mod meeting_questions;
pub mod meeting_recurrence_exceptions;
pub mod meeting_recurrence_rules;
pub mod meeting_spaces;
pub mod organization_memberships;
pub mod organizations;
pub mod outbox_events;
pub mod participant_sessions;

// Note: email_jobs and oauth_states have been migrated to Redis
// See: services/otp.rs for OTP tokens
// See: queues/redis_email.rs for email jobs
// See: handlers/auth.rs for OAuth state handling

pub use api_keys::Entity as ApiKeys;
pub use audio_video_devices::Entity as AudioVideoDevices;
pub use chat_messages::Entity as ChatMessages;
pub use meetings::Entity as Meetings;
pub use participants::Entity as Participants;
pub use session_logs::Entity as SessionLogs;
pub use users::Entity as Users;
pub use webhooks::Entity as Webhooks;

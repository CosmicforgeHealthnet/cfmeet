pub use sea_orm_migration::prelude::*;

// Import all migration modules
mod m20260116_000001_create_users;
mod m20260116_000002_create_meetings;
mod m20260116_000003_create_participants;
mod m20260116_000004_create_audio_video_devices;
mod m20260116_000005_create_chat_messages;
mod m20260116_000006_create_session_logs;
mod m20260116_000007_create_webhooks;
mod m20260116_000008_create_api_keys;
mod m20260117_000001_create_email_jobs;
mod m20260117_000002_add_reset_token_to_users;
mod m20260127_000001_create_oauth_states;
mod m20260205_110126_m20260205_000001_add_guest_role;
mod m20260206_000001_add_media_toggle_event_type;
mod m20260323_000001_migrate_to_redis;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260116_000001_create_users::Migration),
            Box::new(m20260116_000002_create_meetings::Migration),
            Box::new(m20260116_000003_create_participants::Migration),
            Box::new(m20260116_000004_create_audio_video_devices::Migration),
            Box::new(m20260116_000005_create_chat_messages::Migration),
            Box::new(m20260116_000006_create_session_logs::Migration),
            Box::new(m20260116_000007_create_webhooks::Migration),
            Box::new(m20260116_000008_create_api_keys::Migration),
            Box::new(m20260117_000001_create_email_jobs::Migration),
            Box::new(m20260117_000002_add_reset_token_to_users::Migration),
            Box::new(m20260127_000001_create_oauth_states::Migration),
            Box::new(m20260205_110126_m20260205_000001_add_guest_role::Migration),
            Box::new(m20260206_000001_add_media_toggle_event_type::Migration),
            Box::new(m20260323_000001_migrate_to_redis::Migration),
        ]
    }
}

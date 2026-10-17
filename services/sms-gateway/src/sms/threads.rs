//! Transport threads: the messages between ONE Number Resource and ONE remote party (by
//! keyed fingerprint) in one environment. A transport index for listing and replies —
//! not a conversation model (no participants, read state or membership).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use crate::db;

use super::events::EventScope;

#[derive(Debug, Clone, FromQueryResult)]
pub struct ThreadRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub number_resource_id: Uuid,
    pub remote_fingerprint: String,
    pub remote_masked: String,
    pub message_count: i64,
    pub last_message_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

pub const THREAD_COLUMNS: &str =
    "id, tenant_id, application_id, environment_id, number_resource_id, \
    remote_fingerprint, remote_masked, message_count, last_message_at, created_at";

/// The public view of a thread (no fingerprint, no number).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadView {
    pub thread_id: Uuid,
    pub number_resource_id: Uuid,
    pub remote: RemoteView,
    pub message_count: i64,
    pub last_message_at: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteView {
    pub masked: String,
}

pub fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl From<&ThreadRow> for ThreadView {
    fn from(t: &ThreadRow) -> Self {
        ThreadView {
            thread_id: t.id,
            number_resource_id: t.number_resource_id,
            remote: RemoteView {
                masked: t.remote_masked.clone(),
            },
            message_count: t.message_count,
            last_message_at: ts(t.last_message_at),
            created_at: ts(t.created_at),
        }
    }
}

/// The thread of (number, remote party), created on first use, counting this message.
pub async fn touch<C: ConnectionTrait>(
    txn: &C,
    scope: EventScope,
    number_resource_id: Uuid,
    remote_fingerprint: &str,
    remote_masked: &str,
    now: DateTime<Utc>,
) -> Result<ThreadRow, DbErr> {
    db::one::<ThreadRow, _>(
        txn,
        &format!(
            "INSERT INTO sms_transport_threads (id, tenant_id, application_id, environment_id, number_resource_id, \
               remote_fingerprint, remote_masked, message_count, last_message_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $8) \
             ON CONFLICT (environment_id, number_resource_id, remote_fingerprint) DO UPDATE \
               SET message_count = sms_transport_threads.message_count + 1, \
                   last_message_at = GREATEST(sms_transport_threads.last_message_at, EXCLUDED.last_message_at) \
             RETURNING {THREAD_COLUMNS}"
        ),
        vec![
            Uuid::new_v4().into(),
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            number_resource_id.into(),
            remote_fingerprint.into(),
            remote_masked.into(),
            now.into(),
        ],
    )
    .await?
    .ok_or_else(|| DbErr::RecordNotFound("thread".into()))
}

/// A thread of `environment` (another environment's thread is NOT_FOUND).
pub async fn load<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    thread_id: Uuid,
) -> Result<Option<ThreadRow>, DbErr> {
    db::one::<ThreadRow, _>(
        db,
        &format!("SELECT {THREAD_COLUMNS} FROM sms_transport_threads WHERE id = $1 AND environment_id = $2"),
        vec![thread_id.into(), environment_id.into()],
    )
    .await
}

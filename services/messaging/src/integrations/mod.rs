//! Peer-service integrations (Part V H §§97–112, I §116): authenticated internal
//! operations of the services that own an external authority Messaging must follow.
//!
//! | Peer | Permission | Module |
//! |---|---|---|
//! | Meeting Service (first-party backend) | `messaging-application.delegate` | `/internal/v1/delegated/**` (the customer surface, [`crate::http::middleware::require_delegated_application`]) |
//! | Meeting Service | `messaging-meeting.sync` | [`meeting`] |
//! | Privacy Relay | `messaging-context-authority.apply` | [`context_authority`] |
//!
//! Every operation requires the peer's signed DELEGATED scope, re-validated against
//! Platform Access ([`crate::http::middleware::delegated_context`]); the scope is never
//! taken from a request body. Peer authority changes are VERSION-FENCED
//! ([`advance_fence`]): an update at or below the last applied source version is stale
//! and is never re-applied, so a delayed or replayed update cannot restore access.

pub mod context_authority;
pub mod http;
pub mod meeting;

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use crate::{db, error::Result};

/// Advances the fence `key` of `conversation` to `version` from `source`. `Ok(true)`:
/// the update is newer and must be applied (in the same transaction); `Ok(false)`: it is
/// stale or a replay and must be ignored.
pub async fn advance_fence<C: ConnectionTrait>(
    db: &C,
    conversation: Uuid,
    key: &str,
    source: &str,
    version: i64,
    now: DateTime<Utc>,
) -> Result<bool> {
    let n = db::exec(
        db,
        "INSERT INTO messaging_sync_fences (conversation_id, fence_key, source, source_version, applied_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (conversation_id, fence_key) DO UPDATE \
           SET source_version = EXCLUDED.source_version, applied_at = EXCLUDED.applied_at \
         WHERE messaging_sync_fences.source_version < EXCLUDED.source_version \
           AND messaging_sync_fences.source = EXCLUDED.source",
        vec![
            conversation.into(),
            key.into(),
            source.into(),
            version.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(n == 1)
}

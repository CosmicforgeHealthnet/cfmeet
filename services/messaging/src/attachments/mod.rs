//! Attachments (Part V Part F). Interim: until the attachment store exists, messages
//! cannot reference attachments (refused, never silently dropped).

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use crate::{
    conversations::{policy::EffectivePolicy, service::ConversationRow},
    error::{Error, ErrorCode, Result},
    state::AppState,
};

pub async fn message_attachment_ids<C: ConnectionTrait>(
    _db: &C,
    _message: Uuid,
) -> Result<Vec<Uuid>> {
    Ok(Vec::new())
}

pub async fn check_message_attachments(
    _state: &AppState,
    _conversation: &ConversationRow,
    _policy: &EffectivePolicy,
    _sender: Uuid,
    _ids: &[Uuid],
) -> Result<()> {
    Err(Error::reason(
        ErrorCode::FeatureDisabled,
        "ATTACHMENTS_NOT_AVAILABLE",
    ))
}

pub async fn link_to_message<C: ConnectionTrait>(
    _db: &C,
    _conversation: &ConversationRow,
    _message: Uuid,
    _sender: Uuid,
    ids: &[Uuid],
) -> Result<()> {
    if ids.is_empty() {
        Ok(())
    } else {
        Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "ATTACHMENTS_NOT_AVAILABLE",
        ))
    }
}

pub async fn follow_message<C: ConnectionTrait>(
    _db: &C,
    _message: Uuid,
    _lifecycle: &str,
    _now: DateTime<Utc>,
) -> Result<()> {
    Ok(())
}

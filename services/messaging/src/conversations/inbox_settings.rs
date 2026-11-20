//! F1: a member's own archive and pin of a conversation in its chat list.
//!
//! `PUT /v1/conversations/{id}/inbox-settings` (`messages.read`, acting principal, ACTIVE
//! member) sets `archived` and / or `pinned`. Per principal and never visible to anyone
//! else. Like WhatsApp: at most [`MAX_PINNED`] pinned conversations per principal (in the
//! environment; conversations the principal is no longer an ACTIVE member of do not
//! count), the most recently pinned first; pinning un-archives and archiving un-pins; an
//! archived chat stays archived when new messages arrive. `GET /v1/conversations`
//! returns both in each conversation's `inbox` summary.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::put,
    Json, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::service::rfc3339;
use crate::{
    db,
    error::{Error, ErrorCode, Result},
    http::{
        error::ApiError,
        middleware::{Acting, CustomerCaller},
    },
    identity::TrustedContext,
    messages::http::acting_with,
    state::AppState,
};

/// Most conversations one principal may pin (WhatsApp's limit).
pub const MAX_PINNED: i64 = 3;

pub fn routes() -> Router<AppState> {
    Router::new().route("/:conversation_id/inbox-settings", put(put_settings))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxSettingsRequest {
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

async fn put_settings(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<InboxSettingsRequest>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, super::http::MESSAGES_READ).await?;
    Ok(Json(
        set(&state, caller.context(), &acting, id, body).await?,
    ))
}

/// The stored settings of one member (absent row: neither archived nor pinned).
#[derive(Debug, Clone, Default, FromQueryResult)]
pub struct InboxSettings {
    pub archived_at: Option<DateTime<Utc>>,
    pub pinned_at: Option<DateTime<Utc>>,
}

impl InboxSettings {
    /// The fields the chat list (`inbox`) carries.
    pub fn summary_fields(&self) -> [(&'static str, Value); 3] {
        [
            ("archived", json!(self.archived_at.is_some())),
            ("pinned", json!(self.pinned_at.is_some())),
            ("pinnedAt", json!(self.pinned_at.map(rfc3339))),
        ]
    }
}

pub async fn load<C: ConnectionTrait>(
    db: &C,
    conversation: Uuid,
    principal: Uuid,
) -> Result<InboxSettings> {
    Ok(db::one::<InboxSettings, _>(
        db,
        "SELECT archived_at, pinned_at FROM messaging_conversation_inbox_preferences \
          WHERE conversation_id = $1 AND principal_id = $2",
        vec![conversation.into(), principal.into()],
    )
    .await?
    .unwrap_or_default())
}

pub async fn set(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    request: InboxSettingsRequest,
) -> Result<Value> {
    if request.archived.is_none() && request.pinned.is_none() {
        return Err(Error::validation(
            "nothing to change: archived or pinned is required",
        ));
    }
    if request.archived == Some(true) && request.pinned == Some(true) {
        return Err(Error::validation(
            "an archived conversation cannot be pinned",
        ));
    }
    let now = state.now();
    let ctx = ctx.clone();
    let principal = acting.principal_id;
    let txn = state.db.begin().await.map_err(Error::from)?;
    super::access::as_member(&txn, &ctx, id, principal).await?;
    // Serializes one principal's pin changes (the cap is checked, then written).
    db::exec(
        &txn,
        "SELECT pg_advisory_xact_lock(hashtextextended('messaging-inbox-pins:' || $1::text || ':' || $2::text, 0))",
        vec![ctx.scope.environment_id.into(), principal.into()],
    )
    .await?;
    let current = load(&txn, id, principal).await?;
    let mut archived_at = current.archived_at;
    let mut pinned_at = current.pinned_at;
    match request.pinned {
        Some(true) if pinned_at.is_none() => {
            let pinned = db::scalar_i64(
                &txn,
                "SELECT count(*) FROM messaging_conversation_inbox_preferences p \
                   JOIN messaging_conversation_members m ON m.conversation_id = p.conversation_id \
                    AND m.principal_id = p.principal_id AND m.state = 'ACTIVE' \
                  WHERE p.environment_id = $1 AND p.principal_id = $2 AND p.pinned_at IS NOT NULL \
                    AND p.conversation_id <> $3",
                vec![ctx.scope.environment_id.into(), principal.into(), id.into()],
            )
            .await?;
            if pinned >= MAX_PINNED {
                return Err(Error::reason(ErrorCode::Conflict, "PIN_LIMIT_REACHED"));
            }
            pinned_at = Some(now);
            archived_at = None;
        }
        Some(false) => pinned_at = None,
        _ => {}
    }
    match request.archived {
        Some(true) if archived_at.is_none() => {
            archived_at = Some(now);
            pinned_at = None;
        }
        Some(false) => archived_at = None,
        _ => {}
    }
    db::exec(
        &txn,
        "INSERT INTO messaging_conversation_inbox_preferences \
           (conversation_id, principal_id, environment_id, archived_at, pinned_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (conversation_id, principal_id) DO UPDATE SET \
           archived_at = EXCLUDED.archived_at, pinned_at = EXCLUDED.pinned_at, updated_at = EXCLUDED.updated_at",
        vec![
            id.into(),
            principal.into(),
            ctx.scope.environment_id.into(),
            archived_at.into(),
            pinned_at.into(),
            now.into(),
        ],
    )
    .await?;
    txn.commit().await.map_err(Error::from)?;
    crate::metrics::inc("messaging_inbox_settings_changes_total", &[]);
    Ok(json!({
        "conversationId": id,
        "archived": archived_at.is_some(),
        "pinned": pinned_at.is_some(),
        "pinnedAt": pinned_at.map(rfc3339),
    }))
}

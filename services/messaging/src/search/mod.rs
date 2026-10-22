//! Server-side search of MANAGED conversations (Part V §82–§83).
//!
//! * Only MANAGED text is indexed (`messaging_messages.managed_search`, PostgreSQL full
//!   text, the `simple` configuration — no language stemming assumptions). PRIVATE_E2EE
//!   conversations are searched on the user's devices only
//!   (`cf_messaging_crypto::local_search`); the server has no index of them and refuses
//!   with `LOCAL_SEARCH_ONLY` — it never accepts search terms for them.
//! * Every query is membership-aware at query time: the acting principal's ACTIVE
//!   memberships, each member's history window, ACTIVE messages only (tombstoned and
//!   purged messages are removed from the index at once), "delete for me" honoured.
//! * Bounded: query length, result count, per-principal rate; query text is never
//!   logged or stored.

pub mod http;

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::access,
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    messages::{self, MessageRow, MESSAGE_COLUMNS},
    state::AppState,
};

pub const MAX_QUERY_CHARS: usize = 200;
pub const MAX_RESULTS: i64 = 50;
pub const SEARCHES_PER_MINUTE: i64 = 60;
/// Conversations one cross-conversation search spans (most recent memberships).
pub const MAX_SEARCH_CONVERSATIONS: i64 = 1000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub limit: Option<i64>,
    /// Results strictly older than this sequence (conversation search paging).
    #[serde(default)]
    pub before_sequence: Option<i64>,
}

fn validate(q: &SearchQuery) -> Result<i64> {
    let text = q.q.trim();
    if text.is_empty() || text.chars().count() > MAX_QUERY_CHARS {
        return Err(Error::validation(format!(
            "q must be 1-{MAX_QUERY_CHARS} characters"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(Error::validation("q must not contain control characters"));
    }
    let limit = q.limit.unwrap_or(20);
    if !(1..=MAX_RESULTS).contains(&limit) {
        return Err(Error::validation(format!(
            "limit must be between 1 and {MAX_RESULTS}"
        )));
    }
    Ok(limit)
}

async fn rate(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<()> {
    let key = format!(
        "search:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(
        &state.db,
        &key,
        Window::Minute,
        SEARCHES_PER_MINUTE,
        state.now(),
    )
    .await?
    {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    Ok(())
}

async fn rendered(state: &AppState, acting: &Acting, rows: Vec<MessageRow>) -> Result<Vec<Value>> {
    let hidden = messages::hidden_for(
        &state.db,
        acting.principal_id,
        &rows.iter().map(|r| r.id).collect::<Vec<_>>(),
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows.iter().filter(|r| !hidden.contains(&r.id)) {
        out.push(messages::render(state, r, false).await);
    }
    Ok(out)
}

/// `GET /v1/conversations/{id}/search` — one conversation.
pub async fn conversation(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    q: SearchQuery,
) -> Result<Value> {
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::Search,
        ctx.scope.environment_id,
    )
    .await?;
    let limit = validate(&q)?;
    let (conversation, member) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    if conversation.profile() == SecurityProfile::PrivateE2ee {
        // Nothing about the query reaches the server's logs or storage.
        return Err(Error::code(ErrorCode::LocalSearchOnly));
    }
    rate(state, ctx, acting).await?;
    let rows: Vec<MessageRow> = db::all(
        &state.db,
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM messaging_messages m WHERE m.conversation_id = $1 AND m.lifecycle = 'ACTIVE' \
               AND m.managed_search @@ plainto_tsquery('simple', $2) AND m.sequence >= $3 AND m.sequence < $4 \
             ORDER BY m.sequence DESC LIMIT $5"
        ),
        vec![
            conversation_id.into(),
            q.q.trim().to_string().into(),
            member.visible_from_sequence.into(),
            q.before_sequence.unwrap_or(i64::MAX).into(),
            limit.into(),
        ],
    )
    .await?;
    let next = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| r.sequence))
        .flatten();
    crate::metrics::inc("messaging_searches_total", &[("scope", "conversation")]);
    crate::usage::record_best_effort(
        &state.db,
        ctx.into(),
        crate::usage::SEARCH_QUERY,
        1,
        conversation.id,
        Some("MANAGED"),
        state.now(),
    )
    .await;
    Ok(
        json!({ "results": rendered(state, acting, rows).await?, "nextBeforeSequence": next, "searchedProfile": "MANAGED" }),
    )
}

/// `GET /v1/messaging/search` — every MANAGED conversation the principal is an ACTIVE
/// member of (E2EE conversations are skipped and counted, never searched).
pub async fn across(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    q: SearchQuery,
) -> Result<Value> {
    crate::operations::require_open(
        &state.db,
        crate::operations::KillSwitch::Search,
        ctx.scope.environment_id,
    )
    .await?;
    let limit = validate(&q)?;
    if q.before_sequence.is_some() {
        return Err(Error::validation(
            "beforeSequence applies to a single conversation",
        ));
    }
    rate(state, ctx, acting).await?;
    let rows: Vec<MessageRow> = db::all(
        &state.db,
        &format!(
            "WITH mine AS (SELECT mb.conversation_id, mb.visible_from_sequence FROM messaging_conversation_members mb \
                 JOIN messaging_conversations c ON c.id = mb.conversation_id \
                WHERE mb.environment_id = $1 AND mb.principal_id = $2 AND mb.state = 'ACTIVE' AND c.security_profile = 'MANAGED' \
                  AND c.tenant_id = $5 AND c.application_id = $6 \
                ORDER BY mb.joined_at DESC LIMIT $7) \
             SELECT {MESSAGE_COLUMNS} FROM messaging_messages m JOIN mine ON mine.conversation_id = m.conversation_id \
              WHERE m.lifecycle = 'ACTIVE' AND m.sequence >= mine.visible_from_sequence \
                AND m.managed_search @@ plainto_tsquery('simple', $3) \
              ORDER BY m.created_at DESC, m.id LIMIT $4"
        ),
        vec![
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
            q.q.trim().to_string().into(),
            limit.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            MAX_SEARCH_CONVERSATIONS.into(),
        ],
    )
    .await?;
    let e2ee = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_conversation_members mb JOIN messaging_conversations c ON c.id = mb.conversation_id \
          WHERE mb.environment_id = $1 AND mb.principal_id = $2 AND mb.state = 'ACTIVE' AND c.security_profile = 'PRIVATE_E2EE'",
        vec![ctx.scope.environment_id.into(), acting.principal_id.into()],
    )
    .await?;
    crate::metrics::inc("messaging_searches_total", &[("scope", "principal")]);
    crate::usage::record_best_effort(
        &state.db,
        ctx.into(),
        crate::usage::SEARCH_QUERY,
        1,
        acting.principal_id,
        Some("MANAGED"),
        state.now(),
    )
    .await;
    Ok(json!({
        "results": rendered(state, acting, rows).await?,
        "searchedProfile": "MANAGED",
        "localSearchOnlyConversations": e2ee,
    }))
}

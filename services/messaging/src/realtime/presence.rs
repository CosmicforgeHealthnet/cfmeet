//! Privacy-respecting presence (§63).
//!
//! * States: `AVAILABLE`, `OFFLINE`, `POLICY_RESTRICTED` (the environment's policy
//!   disables presence). A HIDDEN principal is reported `OFFLINE` everywhere — on every
//!   API, snapshot and change notice — indistinguishable from being offline.
//! * Being connected never discloses presence by itself: it is shared only when the
//!   policy enables presence AND the principal's visibility is `CONVERSATION_MEMBERS`.
//! * Presence is visible only to co-members of a conversation the viewer is SUBSCRIBED to
//!   (rate-limited queries, at most [`MAX_SNAPSHOT_MEMBERS`] members): no lookup by
//!   principal id, so presence cannot be enumerated across a tenant.
//! * Last seen is shown only when the policy allows it AND the principal shares it.
//! * Live state is ephemeral (TTL; multiple devices; disconnect cleanup); only the
//!   preference and the shared last-seen time are durable.
//! * Responses never contain device ids, connection ids or network information.

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{broker::BrokerMessage, ephemeral::principal_key, sessions::SessionRow};
use crate::{
    conversations::{access, policy::EffectivePolicy, service::rfc3339},
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

pub const MAX_SNAPSHOT_MEMBERS: usize = 256;
/// Conversations a presence change is announced to (most recent memberships).
pub const MAX_ANNOUNCE_CONVERSATIONS: i64 = 500;

#[derive(Debug, Clone, FromQueryResult)]
struct Settings {
    visibility: String,
    share_last_seen: bool,
}

async fn settings(state: &AppState, env: Uuid, principal: Uuid) -> Settings {
    db::one::<Settings, _>(
        &state.db,
        "SELECT visibility, share_last_seen FROM messaging_presence_settings WHERE environment_id = $1 AND principal_id = $2",
        vec![env.into(), principal.into()],
    )
    .await
    .ok()
    .flatten()
    .unwrap_or(Settings {
        visibility: "CONVERSATION_MEMBERS".into(),
        share_last_seen: false,
    })
}

fn ttl(state: &AppState) -> std::time::Duration {
    state.realtime.settings.idle_timeout * 2
}

async fn member_conversations(state: &AppState, env: Uuid, principal: Uuid) -> Vec<Uuid> {
    #[derive(FromQueryResult)]
    struct C {
        conversation_id: Uuid,
    }
    db::all::<C, _>(
        &state.db,
        "SELECT conversation_id FROM messaging_conversation_members WHERE environment_id = $1 AND principal_id = $2 \
           AND state = 'ACTIVE' ORDER BY joined_at DESC LIMIT $3",
        vec![env.into(), principal.into(), MAX_ANNOUNCE_CONVERSATIONS.into()],
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|c| c.conversation_id)
    .collect()
}

async fn announce(
    state: &AppState,
    session: &SessionRow,
    presence_state: &str,
    last_seen: Option<DateTime<Utc>>,
) {
    let conversations =
        member_conversations(state, session.environment_id, session.principal_id).await;
    if conversations.is_empty() {
        return;
    }
    let _ = state
        .realtime
        .broker
        .publish(&BrokerMessage::Presence {
            principal_id: session.principal_id,
            conversation_ids: conversations,
            state: presence_state.to_string(),
            last_seen_at: last_seen.map(rfc3339),
        })
        .await;
}

/// A device connected: mark it live; announce AVAILABLE when the principal came online
/// and its presence is shared. Returns whether presence was announced.
pub async fn connected(state: &AppState, session: &SessionRow, policy: &EffectivePolicy) -> bool {
    let key = principal_key(session.environment_id, session.principal_id);
    let came_online = state
        .realtime
        .presence
        .touch(&key, session.device_id, ttl(state))
        .await
        .unwrap_or(false);
    if !came_online || !policy.privacy.presence {
        return false;
    }
    if settings(state, session.environment_id, session.principal_id)
        .await
        .visibility
        == "HIDDEN"
    {
        return false;
    }
    announce(state, session, "AVAILABLE", None).await;
    true
}

pub async fn touch(state: &AppState, session: &SessionRow) {
    let key = principal_key(session.environment_id, session.principal_id);
    let _ = state
        .realtime
        .presence
        .touch(&key, session.device_id, ttl(state))
        .await;
}

/// A device disconnected: when it was the principal's last, announce OFFLINE (and the
/// last-seen time only if both the policy and the principal allow it).
pub async fn disconnected(state: &AppState, session: &SessionRow, policy: &EffectivePolicy) {
    let key = principal_key(session.environment_id, session.principal_id);
    let offline = state
        .realtime
        .presence
        .remove(&key, session.device_id)
        .await
        .unwrap_or(false);
    if !offline || !policy.privacy.presence {
        return;
    }
    let s = settings(state, session.environment_id, session.principal_id).await;
    if s.visibility == "HIDDEN" {
        return;
    }
    let now = state.now();
    let share = policy.privacy.last_seen && s.share_last_seen;
    if share {
        let _ = db::exec(
            &state.db,
            "INSERT INTO messaging_presence_last_seen (environment_id, principal_id, last_seen_at) VALUES ($1, $2, $3) \
             ON CONFLICT (environment_id, principal_id) DO UPDATE SET last_seen_at = EXCLUDED.last_seen_at",
            vec![session.environment_id.into(), session.principal_id.into(), now.into()],
        )
        .await;
    }
    announce(state, session, "OFFLINE", share.then_some(now)).await;
}

/// Changes the principal's visibility; switching to HIDDEN announces OFFLINE at once.
pub async fn set_visibility(
    state: &AppState,
    session: &SessionRow,
    policy: &EffectivePolicy,
    visibility: &str,
    share_last_seen: Option<bool>,
) -> Result<Value> {
    let v = store(
        state,
        session.environment_id,
        session.principal_id,
        visibility,
        share_last_seen,
    )
    .await?;
    if policy.privacy.presence {
        let online = state
            .realtime
            .presence
            .online(&[principal_key(session.environment_id, session.principal_id)])
            .await
            .unwrap_or_default()
            .first()
            .copied()
            .unwrap_or(false);
        if visibility == "HIDDEN" {
            announce(state, session, "OFFLINE", None).await;
        } else if online {
            announce(state, session, "AVAILABLE", None).await;
        }
    }
    Ok(v)
}

async fn store(
    state: &AppState,
    env: Uuid,
    principal: Uuid,
    visibility: &str,
    share: Option<bool>,
) -> Result<Value> {
    #[derive(FromQueryResult)]
    struct S {
        visibility: String,
        share_last_seen: bool,
    }
    let s: S = db::one(
        &state.db,
        "INSERT INTO messaging_presence_settings (environment_id, principal_id, visibility, share_last_seen, updated_at) \
         VALUES ($1, $2, $3, COALESCE($4, FALSE), $5) ON CONFLICT (environment_id, principal_id) DO UPDATE \
           SET visibility = EXCLUDED.visibility, \
               share_last_seen = COALESCE($4, messaging_presence_settings.share_last_seen), updated_at = EXCLUDED.updated_at \
         RETURNING visibility, share_last_seen",
        vec![env.into(), principal.into(), visibility.into(), share.into(), state.now().into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    if s.visibility == "HIDDEN" || !s.share_last_seen {
        // A principal that stops sharing leaves no last-seen trace behind.
        db::exec(
            &state.db,
            "DELETE FROM messaging_presence_last_seen WHERE environment_id = $1 AND principal_id = $2",
            vec![env.into(), principal.into()],
        )
        .await?;
    }
    Ok(json!({ "visibility": s.visibility, "shareLastSeen": s.share_last_seen }))
}

/// The presence of a conversation's members as the (subscribed, member) viewer may see it.
pub async fn snapshot(
    state: &AppState,
    session: &SessionRow,
    policy: &EffectivePolicy,
    conversation: Uuid,
) -> Result<Vec<Value>> {
    let ctx = session
        .trusted_context()
        .ok_or(Error::code(ErrorCode::InternalError))?;
    access::as_member(&state.db, &ctx, conversation, session.principal_id).await?;
    let members = access::active_members(&state.db, conversation).await?;
    let members: Vec<_> = members
        .into_iter()
        .filter(|m| m.principal_id != session.principal_id)
        .take(MAX_SNAPSHOT_MEMBERS)
        .collect();
    if !policy.privacy.presence {
        return Ok(members
            .iter()
            .map(|m| json!({ "principalId": m.principal_id, "state": "POLICY_RESTRICTED" }))
            .collect());
    }
    #[derive(FromQueryResult)]
    struct P {
        principal_id: Uuid,
        visibility: String,
        share_last_seen: bool,
        last_seen_at: Option<DateTime<Utc>>,
    }
    let ids: Vec<Uuid> = members.iter().map(|m| m.principal_id).collect();
    let prefs: Vec<P> = db::all(
        &state.db,
        "SELECT s.principal_id, s.visibility, s.share_last_seen, l.last_seen_at FROM messaging_presence_settings s \
           LEFT JOIN messaging_presence_last_seen l ON l.environment_id = s.environment_id AND l.principal_id = s.principal_id \
          WHERE s.environment_id = $1 AND s.principal_id = ANY($2)",
        vec![session.environment_id.into(), ids.clone().into()],
    )
    .await?;
    let keys: Vec<String> = ids
        .iter()
        .map(|p| principal_key(session.environment_id, *p))
        .collect();
    let online = state
        .realtime
        .presence
        .online(&keys)
        .await
        .map_err(|_| Error::code(ErrorCode::ServiceUnavailable))?;
    Ok(ids
        .iter()
        .zip(online)
        .map(|(p, on)| {
            let pref = prefs.iter().find(|x| x.principal_id == *p);
            let hidden = pref.is_some_and(|x| x.visibility == "HIDDEN");
            let last = pref
                .filter(|x| policy.privacy.last_seen && x.share_last_seen && !hidden)
                .and_then(|x| x.last_seen_at);
            json!({
                "principalId": p,
                "state": if on && !hidden { "AVAILABLE" } else { "OFFLINE" },
                "lastSeenAt": if on && !hidden { None } else { last.map(rfc3339) },
            })
        })
        .collect())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsRequest {
    pub visibility: String,
    #[serde(default)]
    pub share_last_seen: Option<bool>,
}

/// `PUT /v1/messaging/presence/settings` (application backend for its principal).
pub async fn put_settings(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: SettingsRequest,
) -> Result<Value> {
    if !matches!(
        request.visibility.as_str(),
        "CONVERSATION_MEMBERS" | "HIDDEN"
    ) {
        return Err(Error::validation(
            "visibility must be CONVERSATION_MEMBERS or HIDDEN",
        ));
    }
    let v = store(
        state,
        ctx.scope.environment_id,
        acting.principal_id,
        &request.visibility,
        request.share_last_seen,
    )
    .await?;
    if request.visibility == "HIDDEN" {
        let conversations =
            member_conversations(state, ctx.scope.environment_id, acting.principal_id).await;
        if !conversations.is_empty() {
            let _ = state
                .realtime
                .broker
                .publish(&BrokerMessage::Presence {
                    principal_id: acting.principal_id,
                    conversation_ids: conversations,
                    state: "OFFLINE".into(),
                    last_seen_at: None,
                })
                .await;
        }
    }
    Ok(v)
}

pub async fn get_settings(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
) -> Result<Value> {
    let s = settings(state, ctx.scope.environment_id, acting.principal_id).await;
    Ok(json!({ "visibility": s.visibility, "shareLastSeen": s.share_last_seen }))
}

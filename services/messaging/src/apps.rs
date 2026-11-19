//! CF Meet Apps ("Integrate", ADR 0064; `docs/platform/integrate-internal-contracts.md`
//! §0, §2 Authentication and §7): outside apps acting in CF Meet's own Messaging.
//!
//! * **OAuth credentials.** Platform Access authenticates OAuth access (`cfoa_`) and bot
//!   (`cfob_`) tokens like any credential and reports `credentialKind`, `appId` and
//!   `grantId` ([`crate::identity::OAuthCredential`]); every such credential carries a
//!   principal restriction to exactly one principal (the granting user, or the app's
//!   organization bot). With [`AppsSettings::enabled`] such a request:
//!   * acts ONLY as that principal (`CF-Acting-Principal` is set to it; a different one is
//!     `403 PRINCIPAL_NOT_AUTHORIZED` / `ACTING_PRINCIPAL_MISMATCH`);
//!   * reaches only the closed allowlist [`OAUTH_ROUTES`] (conversation / member reads,
//!     messages, reactions, read cursor, unread, events, attachments, search) — anything
//!     else is `403 INSUFFICIENT_SCOPE` / `OAUTH_ROUTE_NOT_ALLOWED`, which keeps client
//!     tokens, realtime, devices, MLS, backups and administration out of reach;
//!   * sees MANAGED conversations only: a PRIVATE_E2EE conversation is `404 NOT_FOUND` on
//!     every conversation route, absent from lists, and not counted by search;
//!   * records the app on the messages it sends (`messaging_messages.via_application_id`,
//!     rendered as `viaApplicationId`) so clients can label "sent by App X".
//!
//!   With the flag off an OAuth credential is refused (`403 INSUFFICIENT_SCOPE` /
//!   `OAUTH_NOT_ENABLED`): it never acts as an ordinary unrestricted-kind credential.
//! * **Bots** (`BOT_OR_SERVICE` principals whose external reference is `app:<appId>`):
//!   may be members only of MANAGED conversations (adding one to, or creating a DIRECT
//!   conversation with one in, a PRIVATE_E2EE conversation is
//!   `403 PRINCIPAL_NOT_AUTHORIZED` / `APP_BOT_MANAGED_ONLY`). A MANAGED message that
//!   mentions a bot (`mentionedPrincipalIds`) records `messaging.bot.mentioned`; a MANAGED
//!   message in a DIRECT conversation with a bot records `messaging.bot.message-received`.
//!   Both are committed with the message (outbox, ids only: conversation, message, sender
//!   and the bot as subject; never text) in the conversation's own (organization) scope
//!   and relayed ONLY to the Developer Platform's central event ingestion
//!   (`developer-event.publish`, the existing webhook relay), which projects them to the
//!   public `bot.mentioned` / `bot.message_received` and routes them to the installation.
//!   They are recorded only while that receiver is configured
//!   ([`AppsSettings::bot_events`]).

use std::collections::{HashMap, HashSet};

use axum::http::Method;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::json;
use uuid::Uuid;

use crate::{
    conversations::service::{scope_of, ConversationRow},
    db,
    domain::{RequestContext, SecurityProfile},
    error::{Error, ErrorCode, Result},
    events::outbox::{self, NewEvent},
    identity::TrustedContext,
    messages::MessageRow,
    state::AppState,
};

/// `MESSAGING_APPS_ENABLED` (default off).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppsSettings {
    /// OAuth credentials are accepted (as above) and app bots are MANAGED-only.
    pub enabled: bool,
    /// Bot events are recorded (enabled AND the Developer Platform is a public-event
    /// receiver, `MESSAGING_PUBLIC_EVENTS_DESTINATION` `developer-platform` / `both`).
    pub bot_events: bool,
}

impl AppsSettings {
    pub const VARIABLE: &'static str = "MESSAGING_APPS_ENABLED";

    /// Parses the flag; `developer_platform_receiver`: whether relayed events reach the
    /// Developer Platform.
    pub fn from_env(
        get: impl Fn(&str) -> Option<String>,
        developer_platform_receiver: bool,
    ) -> std::result::Result<Self, String> {
        let enabled = match get(Self::VARIABLE).as_deref().map(str::trim) {
            None | Some("") | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(format!("{} must be `true` or `false`", Self::VARIABLE)),
        };
        Ok(Self {
            enabled,
            bot_events: enabled && developer_platform_receiver,
        })
    }

    /// Everything on (tests).
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            bot_events: true,
        }
    }
}

/// The closed allowlist of an OAuth credential (method, path pattern; `*` is one
/// non-empty segment): the partner session device member operations
/// ([`crate::http::middleware::PARTNER_SESSION_DEVICE_ROUTES`]) plus search across the
/// principal's MANAGED conversations. Scopes still apply on top (an app holds
/// `messages.read` / `messages.send` only).
pub const OAUTH_ROUTES: &[(&str, &str)] = &[("GET", "/v1/messaging/search")];

/// Whether an OAuth credential may call `method path`.
pub fn oauth_route(method: &Method, path: &str) -> bool {
    if crate::http::middleware::partner_session_device_route(method, path) {
        return true;
    }
    let path = path.trim_end_matches('/');
    OAUTH_ROUTES
        .iter()
        .any(|(m, p)| *m == method.as_str() && *p == path)
}

fn unavailable(e: crate::identity::AuthorityUnavailable) -> Error {
    tracing::warn!(error = %e, "principal directory unavailable");
    Error::code(ErrorCode::ServiceUnavailable)
}

/// The app bots among `principals` (principal → app id); empty with the flag off.
pub async fn application_bots(
    state: &AppState,
    ctx: &TrustedContext,
    principals: &[Uuid],
) -> Result<HashMap<Uuid, Uuid>> {
    if !state.apps.enabled || principals.is_empty() {
        return Ok(HashMap::new());
    }
    state
        .principals
        .application_bots(&ctx.scope, principals)
        .await
        .map_err(unavailable)
}

/// The refusal of an app bot in a PRIVATE_E2EE conversation.
pub fn bot_managed_only() -> Error {
    crate::metrics::inc("messaging_app_bot_e2ee_refused_total", &[]);
    Error::reason(ErrorCode::PrincipalNotAuthorized, "APP_BOT_MANAGED_ONLY")
}

/// Membership: an app bot may join only a MANAGED conversation. `kind` is the
/// principal's already-resolved kind (only `BOT_OR_SERVICE` is looked up).
pub async fn require_member_allowed(
    state: &AppState,
    ctx: &TrustedContext,
    conversation_id: Uuid,
    principal: Uuid,
    kind: &str,
) -> Result<()> {
    if !state.apps.enabled || kind != "BOT_OR_SERVICE" {
        return Ok(());
    }
    // The profile is immutable, so checking it before the membership transaction is
    // race-free. An unknown conversation is left to the membership path (NOT_FOUND).
    let e2ee = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_conversations WHERE id = $1 AND environment_id = $2 \
           AND security_profile <> 'MANAGED'",
        vec![conversation_id.into(), ctx.scope.environment_id.into()],
    )
    .await?;
    if e2ee > 0
        && !application_bots(state, ctx, &[principal])
            .await?
            .is_empty()
    {
        return Err(bot_managed_only());
    }
    Ok(())
}

/// Conversation creation: whether an app bot takes part (the creator or the DIRECT
/// counterpart); the creation then refuses a PRIVATE_E2EE profile.
pub async fn involves_bot(
    state: &AppState,
    ctx: &TrustedContext,
    parties: &[(Uuid, Option<&str>)],
) -> Result<bool> {
    if !state.apps.enabled {
        return Ok(false);
    }
    // Only BOT_OR_SERVICE principals (or ones whose kind is not known here) are looked up.
    let candidates: Vec<Uuid> = parties
        .iter()
        .filter(|(_, kind)| kind.is_none_or(|k| k == "BOT_OR_SERVICE"))
        .map(|(p, _)| *p)
        .collect();
    Ok(!application_bots(state, ctx, &candidates).await?.is_empty())
}

/// The bots a new MANAGED message concerns (computed before its transaction).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotTargets {
    /// Mentioned app bots (`messaging.bot.mentioned`).
    pub mentioned: Vec<Uuid>,
    /// The app bot counterpart of a DIRECT conversation (`messaging.bot.message-received`).
    pub direct: Option<Uuid>,
}

impl BotTargets {
    pub fn is_empty(&self) -> bool {
        self.mentioned.is_empty() && self.direct.is_none()
    }
}

/// The app bots a MANAGED message of `sender` with `mentions` concerns (none with bot
/// events off, for PRIVATE_E2EE, or when no candidate is an app bot).
pub async fn bot_targets(
    state: &AppState,
    ctx: &TrustedContext,
    conversation: &ConversationRow,
    sender: Uuid,
    mentions: &[Uuid],
) -> Result<BotTargets> {
    if !state.apps.bot_events || conversation.profile() != SecurityProfile::Managed {
        return Ok(BotTargets::default());
    }
    let counterpart = if conversation.context_class == "DIRECT" {
        #[derive(FromQueryResult)]
        struct Other {
            principal_id: Uuid,
        }
        db::one::<Other, _>(
            &state.db,
            "SELECT principal_id FROM messaging_conversation_members WHERE conversation_id = $1 \
               AND principal_id <> $2 AND state = 'ACTIVE'",
            vec![conversation.id.into(), sender.into()],
        )
        .await?
        .map(|o| o.principal_id)
    } else {
        None
    };
    let mut candidates: Vec<Uuid> = mentions.to_vec();
    candidates.extend(counterpart);
    candidates.sort();
    candidates.dedup();
    let bots: HashSet<Uuid> = application_bots(state, ctx, &candidates)
        .await?
        .into_keys()
        .collect();
    Ok(BotTargets {
        mentioned: mentions
            .iter()
            .copied()
            .filter(|p| bots.contains(p))
            .collect(),
        direct: counterpart.filter(|p| bots.contains(p)),
    })
}

/// Bot events are relayed to the Developer Platform only (never to Calling).
pub fn developer_platform_only(event_type: &str) -> bool {
    event_type.starts_with("messaging.bot.")
}

pub const BOT_MENTIONED: &str = "messaging.bot.mentioned";
pub const BOT_MESSAGE_RECEIVED: &str = "messaging.bot.message-received";

/// Records the bot events of a new message (same transaction): ids only, in the
/// conversation's scope, queued for the Developer Platform relay.
pub async fn record_bot_events<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    targets: &BotTargets,
    row: &MessageRow,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<()> {
    let events = targets
        .mentioned
        .iter()
        .map(|b| (BOT_MENTIONED, *b))
        .chain(targets.direct.map(|b| (BOT_MESSAGE_RECEIVED, b)));
    for (event_type, bot) in events {
        let id = outbox::enqueue(
            db,
            NewEvent {
                event_type,
                aggregate_type: "BOT",
                aggregate_id: bot,
                scope: scope_of(ctx),
                payload: json!({
                    "conversation_id": row.conversation_id,
                    "message_id": row.id,
                    "sender_principal_id": row.sender_principal_id,
                    "subject_principal_id": bot,
                }),
            },
            request,
            now,
        )
        .await?;
        db::exec(
            db,
            "INSERT INTO messaging_webhook_relay (event_id, environment_id, next_attempt_at, created_at) \
             VALUES ($1, $2, $3, $3)",
            vec![id.into(), ctx.scope.environment_id.into(), now.into()],
        )
        .await?;
        crate::metrics::inc("messaging_app_bot_events_total", &[("type", event_type)]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_is_strict_and_bot_events_need_the_developer_platform() {
        let env = |v: Option<&'static str>| move |_: &str| v.map(str::to_string);
        assert_eq!(
            AppsSettings::from_env(env(None), true).unwrap(),
            AppsSettings::default()
        );
        assert_eq!(
            AppsSettings::from_env(env(Some("true")), true).unwrap(),
            AppsSettings::enabled()
        );
        let no_receiver = AppsSettings::from_env(env(Some("true")), false).unwrap();
        assert!(no_receiver.enabled && !no_receiver.bot_events);
        assert!(AppsSettings::from_env(env(Some("yes")), true).is_err());
    }

    #[test]
    fn the_oauth_allowlist_is_closed() {
        let ok = |m: Method, p: &str| oauth_route(&m, p);
        let c = Uuid::new_v4();
        assert!(ok(Method::GET, "/v1/conversations"));
        assert!(ok(Method::POST, &format!("/v1/conversations/{c}/messages")));
        assert!(ok(Method::GET, "/v1/messaging/search"));
        assert!(ok(Method::GET, &format!("/v1/conversations/{c}/search")));
        assert!(!ok(Method::POST, "/v1/conversations"));
        assert!(!ok(Method::POST, &format!("/v1/conversations/{c}/members")));
        assert!(!ok(Method::POST, "/v1/messaging/client-tokens"));
        assert!(!ok(Method::POST, "/v1/realtime/tickets"));
        assert!(!ok(Method::POST, &format!("/v1/conversations/{c}/mls/commits")));
        assert!(!ok(Method::GET, "/v1/messaging/policy"));
    }

    #[test]
    fn only_bot_events_skip_calling() {
        assert!(developer_platform_only(BOT_MENTIONED));
        assert!(developer_platform_only(BOT_MESSAGE_RECEIVED));
        assert!(!developer_platform_only("messaging.message.created"));
    }
}

//! Group invite links (S12).
//!
//! An OWNER / ADMIN (or the application backend) of a GROUP / CHANNEL conversation creates
//! an expiring, optionally use-capped join link. The link TOKEN (`mil_` + 32 random bytes,
//! base64url) is returned ONCE at creation; only its SHA-256 is stored, so a database copy
//! cannot be turned back into a working link. Links are listed (without tokens) and revoked
//! by administrators; a revocation is final.
//!
//! Joining (`POST /v1/messaging/invite-links/join` `{ token }`) needs an authenticated
//! acting principal of the same application environment. It is refused for an unknown,
//! expired, revoked or used-up link (all answered `NOT_FOUND` / `INVITE_LINK_UNAVAILABLE`
//! alike, so a link cannot be probed), for a banned principal, when the conversation does
//! not accept members, when the Communications personal policy refuses a group invitation
//! from the link's creator, and above the member limit — exactly the rules of an ordinary
//! admission ([`super::activate`], `authority_source = 'INVITE_LINK'`). An existing member
//! joining again changes nothing and does not consume a use.

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{activate, require_admin, Administrator};
use crate::{
    conversations::{
        access,
        service::{load, request_context, rfc3339},
    },
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

pub const TOKEN_PREFIX: &str = "mil_";
pub const MIN_TTL_SECONDS: i64 = 60;
pub const MAX_TTL_SECONDS: i64 = 30 * 24 * 3600;
pub const DEFAULT_TTL_SECONDS: i64 = 7 * 24 * 3600;
pub const MAX_USES: i32 = 10_000;
/// Open links per conversation.
pub const MAX_ACTIVE_LINKS: i64 = 100;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateLinkRequest {
    #[serde(default)]
    pub expires_in_seconds: Option<i64>,
    #[serde(default)]
    pub max_uses: Option<i32>,
    /// `MEMBER` (default) or `READ_ONLY`.
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JoinRequest {
    pub token: String,
}

#[derive(Debug, Clone, FromQueryResult)]
struct LinkRow {
    id: Uuid,
    conversation_id: Uuid,
    role: String,
    max_uses: Option<i32>,
    use_count: i32,
    expires_at: DateTime<Utc>,
    created_by_principal_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str =
    "id, conversation_id, role, max_uses, use_count, expires_at, created_by_principal_id, \
     created_at, revoked_at";

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{TOKEN_PREFIX}{}", crate::messages::b64(&b))
}

fn status(l: &LinkRow, now: DateTime<Utc>) -> &'static str {
    if l.revoked_at.is_some() {
        "REVOKED"
    } else if l.expires_at <= now {
        "EXPIRED"
    } else if l.max_uses.is_some_and(|m| l.use_count >= m) {
        "EXHAUSTED"
    } else {
        "ACTIVE"
    }
}

fn view(l: &LinkRow, now: DateTime<Utc>) -> Value {
    json!({
        "inviteLinkId": l.id,
        "conversationId": l.conversation_id,
        "role": l.role,
        "maxUses": l.max_uses,
        "useCount": l.use_count,
        "expiresAt": rfc3339(l.expires_at),
        "createdByPrincipalId": l.created_by_principal_id,
        "createdAt": rfc3339(l.created_at),
        "revokedAt": l.revoked_at.map(rfc3339),
        "status": status(l, now),
    })
}

fn require_group(context_class: &str) -> Result<()> {
    if matches!(context_class, "GROUP" | "CHANNEL") {
        Ok(())
    } else {
        Err(Error::reason(
            ErrorCode::InvalidTransition,
            "INVITE_LINKS_GROUP_ONLY",
        ))
    }
}

/// `POST /v1/conversations/{id}/invite-links` — the token is in this response only.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
    request: CreateLinkRequest,
) -> Result<Value> {
    let ttl = request.expires_in_seconds.unwrap_or(DEFAULT_TTL_SECONDS);
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl) {
        return Err(Error::validation(format!(
            "expiresInSeconds must be {MIN_TTL_SECONDS}-{MAX_TTL_SECONDS}"
        )));
    }
    if request
        .max_uses
        .is_some_and(|m| !(1..=MAX_USES).contains(&m))
    {
        return Err(Error::validation(format!("maxUses must be 1-{MAX_USES}")));
    }
    let role = match request.role.as_deref() {
        None | Some("MEMBER") => "MEMBER",
        Some("READ_ONLY") => "READ_ONLY",
        Some(_) => return Err(Error::validation("role must be MEMBER or READ_ONLY")),
    };
    let now = state.now();
    let conversation = load(&state.db, ctx, conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    require_admin(&state.db, &conversation, admin).await?;
    require_group(&conversation.context_class)?;
    access::require_accepting(&conversation, now)?;
    let open = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_invite_links WHERE conversation_id = $1 AND revoked_at IS NULL \
           AND expires_at > $2",
        vec![conversation.id.into(), now.into()],
    )
    .await?;
    if open >= MAX_ACTIVE_LINKS {
        return Err(Error::reason(ErrorCode::Conflict, "TOO_MANY_INVITE_LINKS"));
    }
    let token = new_token();
    let id = Uuid::new_v4();
    db::exec(
        &state.db,
        "INSERT INTO messaging_invite_links (id, tenant_id, application_id, environment_id, conversation_id, \
           token_sha256, role, max_uses, expires_at, created_by_principal_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            conversation.id.into(),
            digest(&token).into(),
            role.into(),
            request.max_uses.into(),
            (now + Duration::seconds(ttl)).into(),
            admin.principal_id().into(),
            now.into(),
        ],
    )
    .await?;
    audit(
        state,
        ctx,
        admin.principal_id(),
        "INVITE_LINK_CREATED",
        conversation.id,
        id,
        now,
    )
    .await?;
    let row = one(&state.db, conversation.id, id).await?;
    let mut v = view(&row, now);
    v["token"] = json!(token);
    Ok(v)
}

async fn one<C: sea_orm::ConnectionTrait>(db: &C, conversation: Uuid, id: Uuid) -> Result<LinkRow> {
    db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_invite_links WHERE conversation_id = $1 AND id = $2"
        ),
        vec![conversation.into(), id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))
}

async fn audit(
    state: &AppState,
    ctx: &TrustedContext,
    principal: Option<Uuid>,
    action: &'static str,
    conversation: Uuid,
    link: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    crate::audit::record(
        &state.db,
        crate::audit::AuditEntry {
            ctx,
            principal_id: principal,
            action,
            target_kind: "CONVERSATION",
            target_id: conversation,
            detail: json!({ "invite_link_id": link }),
        },
        &request_context(),
        now,
    )
    .await
}

/// `GET /v1/conversations/{id}/invite-links` — newest first, never a token.
pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
) -> Result<Value> {
    let conversation = load(&state.db, ctx, conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    require_admin(&state.db, &conversation, admin).await?;
    let rows: Vec<LinkRow> = db::all(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_invite_links WHERE conversation_id = $1 \
             ORDER BY created_at DESC, id LIMIT 200"
        ),
        vec![conversation.id.into()],
    )
    .await?;
    let now = state.now();
    Ok(json!({ "inviteLinks": rows.iter().map(|r| view(r, now)).collect::<Vec<_>>() }))
}

/// `POST /v1/conversations/{id}/invite-links/{linkId}/revoke` — final; idempotent.
pub async fn revoke(
    state: &AppState,
    ctx: &TrustedContext,
    admin: &Administrator,
    conversation_id: Uuid,
    link_id: Uuid,
) -> Result<Value> {
    let conversation = load(&state.db, ctx, conversation_id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    require_admin(&state.db, &conversation, admin).await?;
    let now = state.now();
    let n = db::exec(
        &state.db,
        "UPDATE messaging_invite_links SET revoked_at = $3, revoked_by_principal_id = $4 \
         WHERE conversation_id = $1 AND id = $2 AND revoked_at IS NULL",
        vec![
            conversation.id.into(),
            link_id.into(),
            now.into(),
            admin.principal_id().into(),
        ],
    )
    .await?;
    let row = one(&state.db, conversation.id, link_id).await?;
    if n == 1 {
        audit(
            state,
            ctx,
            admin.principal_id(),
            "INVITE_LINK_REVOKED",
            conversation.id,
            link_id,
            now,
        )
        .await?;
    }
    Ok(view(&row, now))
}

fn unavailable() -> Error {
    Error::reason(ErrorCode::NotFound, "INVITE_LINK_UNAVAILABLE")
}

/// `POST /v1/messaging/invite-links/join` — the acting principal joins with a link token.
pub async fn join(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: JoinRequest,
) -> Result<(u16, Value)> {
    if !request.token.starts_with(TOKEN_PREFIX) || request.token.len() > 80 {
        return Err(unavailable());
    }
    let hash = digest(&request.token);
    // The joiner must be an ACTIVE messaging principal of this environment.
    let kind = super::principal_kind(state, ctx, acting.principal_id).await?;
    if kind == "BOT_OR_SERVICE" {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "BOT_CANNOT_JOIN_BY_LINK",
        ));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, (u16, Value), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let link: LinkRow = db::one(
                    txn,
                    &format!(
                        "SELECT {COLUMNS} FROM messaging_invite_links WHERE token_sha256 = $1 \
                           AND environment_id = $2 FOR UPDATE"
                    ),
                    vec![hash.into(), ctx.scope.environment_id.into()],
                )
                .await?
                .ok_or_else(unavailable)?;
                if status(&link, now) != "ACTIVE" {
                    return Err(unavailable());
                }
                let conversation = load(txn, &ctx, link.conversation_id, true)
                    .await?
                    .ok_or_else(unavailable)?;
                if access::membership(txn, conversation.id, acting.principal_id, true)
                    .await?
                    .is_some_and(|m| m.is_active())
                {
                    let (_, v) = activate(
                        txn,
                        &st,
                        &ctx,
                        &Administrator::Principal(acting.clone()),
                        &conversation,
                        acting.principal_id,
                        &link.role,
                        "INVITE_LINK",
                        &request_ctx,
                        now,
                    )
                    .await?;
                    return Ok((200, v));
                }
                let creator = link
                    .created_by_principal_id
                    .unwrap_or(conversation.owner_principal_id);
                if creator != acting.principal_id {
                    crate::communications::require(
                        &st,
                        &ctx,
                        cf_communications_policy::Action::GroupInvitation,
                        creator,
                        acting.principal_id,
                    )
                    .await?;
                }
                let (status_code, v) = activate(
                    txn,
                    &st,
                    &ctx,
                    &Administrator::Principal(acting.clone()),
                    &conversation,
                    acting.principal_id,
                    &link.role,
                    "INVITE_LINK",
                    &request_ctx,
                    now,
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_invite_links SET use_count = use_count + 1 WHERE id = $1",
                    vec![link.id.into()],
                )
                .await?;
                crate::metrics::inc("messaging_invite_link_joins_total", &[]);
                Ok((status_code, v))
            })
        })
        .await
        .map_err(Error::from)
}

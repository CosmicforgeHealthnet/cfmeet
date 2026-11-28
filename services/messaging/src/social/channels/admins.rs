//! Channel admins, ownership transfer, invite links and the channel's audit log (ADR 0072).
//!
//! * The owner holds every permission. An admin holds any of `canPost` (post, schedule,
//!   pin, edit / delete their own posts), `canEdit` (channel details, invite links, every
//!   post) and `canManageAdmins` (add, change and remove admins who cannot manage admins,
//!   granting only permissions they hold themselves). Only the owner deletes the channel,
//!   changes its username or visibility, applies for verification and transfers ownership
//!   (to an existing admin, who keeps being followed; the old owner becomes a full admin).
//! * A new admin must be a mutual, unblocked contact of the person adding them
//!   (Communications Hub): nobody is made an admin of a stranger's channel, and no follower
//!   list is ever needed or shown.
//! * Invite links (the group invite-link model): the token (`mci_` + 32 random bytes,
//!   base64url) is returned once; only its SHA-256 is stored. Preview and join take the
//!   token in the body (never in a URL). Revocation is final.

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{add_follower, audit, forbidden, limit, load, one, ChannelRow, Role, CHANNEL_SELECT};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::Window,
    social::{eligible_contacts, media, not_found},
    state::AppState,
};

pub const MAX_ADMINS: i64 = 50;
pub const TOKEN_PREFIX: &str = "mci_";
pub const DEFAULT_INVITE_TTL_SECONDS: i64 = 30 * 24 * 3600;
pub const MAX_INVITE_TTL_SECONDS: i64 = 365 * 24 * 3600;
pub const MIN_INVITE_TTL_SECONDS: i64 = 3600;
pub const MAX_ACTIVE_INVITES: i64 = 50;

fn staff_only(row: &ChannelRow, me: Uuid) -> Result<()> {
    if row.staff(me) {
        Ok(())
    } else {
        Err(forbidden(
            "NOT_CHANNEL_ADMIN",
            "Only the channel's owner and admins can do this",
        ))
    }
}

#[derive(FromQueryResult)]
struct AdminRow {
    principal_id: Uuid,
    can_post: bool,
    can_edit: bool,
    can_manage_admins: bool,
    added_at: DateTime<Utc>,
}

/// The owner and the admins (owner and admins only).
pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    staff_only(&row, me)?;
    let admins: Vec<AdminRow> = db::all(
        &state.db,
        "SELECT principal_id, can_post, can_edit, can_manage_admins, added_at FROM messaging_channel_admins \
          WHERE channel_id = $1 ORDER BY added_at",
        vec![id.into()],
    )
    .await?;
    Ok(json!({
        "channelId": id,
        "ownerPrincipalId": row.owner_principal_id,
        "admins": admins.iter().map(|a| json!({
            "principalId": a.principal_id,
            "canPost": a.can_post,
            "canEdit": a.can_edit,
            "canManageAdmins": a.can_manage_admins,
            "addedAt": rfc3339(a.added_at),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminRequest {
    pub can_post: bool,
    pub can_edit: bool,
    pub can_manage_admins: bool,
}

async fn admin_of<C: ConnectionTrait>(
    db: &C,
    channel: Uuid,
    who: Uuid,
) -> Result<Option<AdminRow>> {
    Ok(db::one(
        db,
        "SELECT principal_id, can_post, can_edit, can_manage_admins, added_at FROM messaging_channel_admins \
          WHERE channel_id = $1 AND principal_id = $2",
        vec![channel.into(), who.into()],
    )
    .await?)
}

/// Add an admin or change their permissions.
pub async fn put(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    who: Uuid,
    req: AdminRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    let mine = row.perms(me);
    if !mine.manage_admins {
        return Err(forbidden(
            "NOT_ADMIN_MANAGER",
            "Only the owner or an admin who can manage admins can do this",
        ));
    }
    row.require_open()?;
    if who == row.owner_principal_id || who == me {
        return Err(Error::Code {
            code: ErrorCode::Conflict,
            reason: Some("NOT_AN_ADMIN_CHANGE"),
            message: Some("The owner's and your own permissions can't be changed here".into()),
        });
    }
    if !(req.can_post || req.can_edit || req.can_manage_admins) {
        return Err(Error::validation(
            "Give the admin at least one permission (or remove them)",
        ));
    }
    let owner = row.role(me) == Role::Owner;
    if !owner
        && ((req.can_post && !mine.post) || (req.can_edit && !mine.edit) || req.can_manage_admins)
    {
        return Err(forbidden(
            "PERMISSION_NOT_HELD",
            "You can grant only permissions you hold, and only the owner adds admin managers",
        ));
    }
    limit(state, ctx, "admins", me, Window::Hour, 60).await?;
    let existing = admin_of(&state.db, id, who).await?;
    if let Some(a) = &existing {
        if a.can_manage_admins && !owner {
            return Err(forbidden(
                "PERMISSION_NOT_HELD",
                "Only the owner changes an admin who manages admins",
            ));
        }
    } else {
        // A new admin is a mutual, unblocked contact of the person adding them.
        let contacts = eligible_contacts(state, ctx, me, Some(&[who])).await?;
        if !contacts.contains_key(&who) {
            return Err(forbidden(
                "NOT_A_CONTACT",
                "You can make only your contacts admins",
            ));
        }
        let n = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_channel_admins WHERE channel_id = $1",
            vec![id.into()],
        )
        .await?;
        if n >= MAX_ADMINS {
            return Err(Error::reason(ErrorCode::QuotaExceeded, "TOO_MANY_ADMINS"));
        }
    }
    let now = state.now();
    let adding = existing.is_none();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                db::exec(
                    txn,
                    "INSERT INTO messaging_channel_admins (channel_id, principal_id, can_post, can_edit, can_manage_admins, \
                       added_by_principal_id, added_at) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (channel_id, principal_id) DO UPDATE SET can_post = EXCLUDED.can_post, \
                       can_edit = EXCLUDED.can_edit, can_manage_admins = EXCLUDED.can_manage_admins",
                    vec![
                        id.into(),
                        who.into(),
                        req.can_post.into(),
                        req.can_edit.into(),
                        req.can_manage_admins.into(),
                        me.into(),
                        now.into(),
                    ],
                )
                .await?;
                add_follower(txn, id, who, now).await?;
                audit(
                    txn,
                    id,
                    me,
                    if adding { "ADMIN_ADDED" } else { "ADMIN_CHANGED" },
                    Some(who),
                    None,
                    now,
                )
                .await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    list(state, ctx, acting, id).await
}

/// Remove an admin (an admin may also step down themselves).
pub async fn remove(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    who: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    let existing = admin_of(&state.db, id, who).await?.ok_or_else(not_found)?;
    let owner = row.role(me) == Role::Owner;
    let allowed =
        who == me || owner || (row.perms(me).manage_admins && !existing.can_manage_admins);
    if !allowed {
        return Err(forbidden(
            "NOT_ADMIN_MANAGER",
            "Only the owner or an admin who can manage admins can do this",
        ));
    }
    let now = state.now();
    db::exec(
        &state.db,
        "DELETE FROM messaging_channel_admins WHERE channel_id = $1 AND principal_id = $2",
        vec![id.into(), who.into()],
    )
    .await?;
    audit(&state.db, id, me, "ADMIN_REMOVED", Some(who), None, now).await?;
    if who == me {
        return one(state, ctx, me, id).await;
    }
    list(state, ctx, acting, id).await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferRequest {
    /// An existing admin of the channel.
    pub principal_id: Uuid,
}

/// Hand the channel to an admin; the old owner stays on as a full admin.
pub async fn transfer(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: TransferRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if row.role(me) != Role::Owner {
        return Err(forbidden(
            "NOT_CHANNEL_OWNER",
            "Only the channel's owner can do this",
        ));
    }
    row.require_open()?;
    let to = req.principal_id;
    if admin_of(&state.db, id, to).await?.is_none() {
        return Err(Error::Code {
            code: ErrorCode::Conflict,
            reason: Some("NOT_AN_ADMIN"),
            message: Some("Make them an admin first".into()),
        });
    }
    let owned = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_channels WHERE environment_id = $1 AND owner_principal_id = $2",
        vec![ctx.scope.environment_id.into(), to.into()],
    )
    .await?;
    if owned >= super::MAX_CHANNELS_PER_OWNER {
        return Err(Error::reason(ErrorCode::QuotaExceeded, "TOO_MANY_CHANNELS"));
    }
    let now = state.now();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let n = db::exec(
                    txn,
                    "UPDATE messaging_channels SET owner_principal_id = $3, version = version + 1, updated_at = $4 \
                      WHERE id = $1 AND owner_principal_id = $2",
                    vec![id.into(), me.into(), to.into(), now.into()],
                )
                .await?;
                if n != 1 {
                    return Err(Error::code(ErrorCode::VersionConflict));
                }
                db::exec(
                    txn,
                    "DELETE FROM messaging_channel_admins WHERE channel_id = $1 AND principal_id = $2",
                    vec![id.into(), to.into()],
                )
                .await?;
                db::exec(
                    txn,
                    "INSERT INTO messaging_channel_admins (channel_id, principal_id, can_post, can_edit, can_manage_admins, \
                       added_by_principal_id, added_at) VALUES ($1, $2, true, true, true, $3, $4) ON CONFLICT DO NOTHING",
                    vec![id.into(), me.into(), to.into(), now.into()],
                )
                .await?;
                add_follower(txn, id, to, now).await?;
                // A pending verification belongs to the person who applied.
                db::exec(
                    txn,
                    "UPDATE messaging_channel_verifications SET state = 'REJECTED', decision_reason = 'OWNERSHIP_TRANSFERRED', \
                       decided_at = $2, version = version + 1 WHERE channel_id = $1 AND state = 'PENDING'",
                    vec![id.into(), now.into()],
                )
                .await?;
                audit(txn, id, me, "OWNERSHIP_TRANSFERRED", Some(to), None, now).await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    one(state, ctx, me, id).await
}

// ----------------------------------------------------------------------------- invites

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!(
        "{TOKEN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[derive(FromQueryResult)]
struct InviteRow {
    id: Uuid,
    channel_id: Uuid,
    created_by_principal_id: Uuid,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    max_uses: Option<i32>,
    use_count: i32,
    revoked_at: Option<DateTime<Utc>>,
}

const INVITE_COLUMNS: &str =
    "id, channel_id, created_by_principal_id, created_at, expires_at, max_uses, use_count, revoked_at";

fn invite_view(r: &InviteRow, now: DateTime<Utc>) -> Value {
    let used_up = r.max_uses.is_some_and(|m| r.use_count >= m);
    json!({
        "inviteId": r.id,
        "channelId": r.channel_id,
        "createdByPrincipalId": r.created_by_principal_id,
        "createdAt": rfc3339(r.created_at),
        "expiresAt": rfc3339(r.expires_at),
        "maxUses": r.max_uses,
        "useCount": r.use_count,
        "revoked": r.revoked_at.is_some(),
        "active": r.revoked_at.is_none() && r.expires_at > now && !used_up,
    })
}

fn require_inviter(row: &ChannelRow, me: Uuid) -> Result<()> {
    if row.perms(me).edit {
        Ok(())
    } else {
        Err(forbidden(
            "NOT_CHANNEL_EDITOR",
            "Only the owner or an admin who can edit can manage invite links",
        ))
    }
}

pub async fn invites(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    require_inviter(&row, me)?;
    let rows: Vec<InviteRow> = db::all(
        &state.db,
        &format!(
            "SELECT {INVITE_COLUMNS} FROM messaging_channel_invites WHERE channel_id = $1 ORDER BY created_at DESC LIMIT 100"
        ),
        vec![id.into()],
    )
    .await?;
    let now = state.now();
    Ok(json!({ "invites": rows.iter().map(|r| invite_view(r, now)).collect::<Vec<_>>() }))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InviteRequest {
    #[serde(default)]
    pub expires_in_seconds: Option<i64>,
    #[serde(default)]
    pub max_uses: Option<i32>,
}

/// A new invite link; its `token` is in this answer only.
pub async fn create_invite(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: InviteRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    require_inviter(&row, me)?;
    row.require_open()?;
    let ttl = req.expires_in_seconds.unwrap_or(DEFAULT_INVITE_TTL_SECONDS);
    if !(MIN_INVITE_TTL_SECONDS..=MAX_INVITE_TTL_SECONDS).contains(&ttl) {
        return Err(Error::validation(
            "expiresInSeconds must be between 3600 and 31536000",
        ));
    }
    if req.max_uses.is_some_and(|m| !(1..=100_000).contains(&m)) {
        return Err(Error::validation("maxUses must be between 1 and 100000"));
    }
    limit(state, ctx, "invite", me, Window::Hour, 30).await?;
    let now = state.now();
    let open = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_channel_invites WHERE channel_id = $1 AND revoked_at IS NULL AND expires_at > $2",
        vec![id.into(), now.into()],
    )
    .await?;
    if open >= MAX_ACTIVE_INVITES {
        return Err(Error::reason(ErrorCode::Conflict, "TOO_MANY_INVITE_LINKS"));
    }
    let token = new_token();
    let invite = Uuid::new_v4();
    db::exec(
        &state.db,
        "INSERT INTO messaging_channel_invites (id, channel_id, token_sha256, created_by_principal_id, created_at, expires_at, max_uses) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        vec![
            invite.into(),
            id.into(),
            digest(&token).into(),
            me.into(),
            now.into(),
            (now + Duration::seconds(ttl)).into(),
            req.max_uses.into(),
        ],
    )
    .await?;
    audit(&state.db, id, me, "INVITE_CREATED", None, Some(invite), now).await?;
    let r: InviteRow = db::one(
        &state.db,
        &format!("SELECT {INVITE_COLUMNS} FROM messaging_channel_invites WHERE id = $1"),
        vec![invite.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    let mut v = invite_view(&r, now);
    v["token"] = json!(token);
    Ok(v)
}

pub async fn revoke_invite(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    invite: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    require_inviter(&row, me)?;
    let now = state.now();
    let n = db::exec(
        &state.db,
        "UPDATE messaging_channel_invites SET revoked_at = coalesce(revoked_at, $3) WHERE id = $1 AND channel_id = $2",
        vec![invite.into(), id.into(), now.into()],
    )
    .await?;
    if n == 0 {
        return Err(not_found());
    }
    audit(&state.db, id, me, "INVITE_REVOKED", None, Some(invite), now).await?;
    let r: InviteRow = db::one(
        &state.db,
        &format!("SELECT {INVITE_COLUMNS} FROM messaging_channel_invites WHERE id = $1"),
        vec![invite.into()],
    )
    .await?
    .ok_or_else(not_found)?;
    Ok(invite_view(&r, now))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenRequest {
    pub token: String,
}

/// The usable invite of `token` in this environment (anything else is the same 404).
async fn usable_invite<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    token: &str,
    now: DateTime<Utc>,
    lock: bool,
) -> Result<InviteRow> {
    let token = token.trim();
    if !token.starts_with(TOKEN_PREFIX) || token.len() > 100 {
        return Err(Error::reason(
            ErrorCode::NotFound,
            "INVITE_LINK_UNAVAILABLE",
        ));
    }
    let row: Option<InviteRow> = db::one(
        db,
        &format!(
            "SELECT i.id, i.channel_id, i.created_by_principal_id, i.created_at, i.expires_at, i.max_uses, i.use_count, \
               i.revoked_at FROM messaging_channel_invites i JOIN messaging_channels c ON c.id = i.channel_id \
              WHERE i.token_sha256 = $1 AND c.environment_id = $2{}",
            if lock { " FOR UPDATE OF i" } else { "" }
        ),
        vec![digest(token).into(), ctx.scope.environment_id.into()],
    )
    .await?;
    row.filter(|r| {
        r.revoked_at.is_none() && r.expires_at > now && r.max_uses.is_none_or(|m| r.use_count < m)
    })
    .ok_or(Error::reason(
        ErrorCode::NotFound,
        "INVITE_LINK_UNAVAILABLE",
    ))
}

/// What an invite leads to, before joining: the channel's public card (no posts).
pub async fn preview(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: TokenRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    limit(state, ctx, "invite-use", me, Window::Minute, 30).await?;
    let invite = usable_invite(&state.db, ctx, &req.token, state.now(), false).await?;
    let c: ChannelRow = db::one(
        &state.db,
        &format!("{CHANNEL_SELECT} WHERE c.id = $2"),
        vec![me.into(), invite.channel_id.into()],
    )
    .await?
    .ok_or(Error::reason(
        ErrorCode::NotFound,
        "INVITE_LINK_UNAVAILABLE",
    ))?;
    if !c.open() {
        return Err(Error::reason(
            ErrorCode::NotFound,
            "INVITE_LINK_UNAVAILABLE",
        ));
    }
    let avatars = super::media_of(&state.db, c.avatar_media_id.into_iter().collect()).await?;
    Ok(json!({
        "channelId": c.id,
        "name": c.name,
        "description": c.description,
        "handle": c.handle,
        "category": c.category,
        "visibility": c.visibility,
        "verified": c.verified_at.is_some(),
        "followerCount": c.followers,
        "following": c.following || c.staff(me),
        "avatar": c.avatar_media_id.and_then(|m| avatars.get(&m)).map(|m| media::view(m, None)),
    }))
}

/// Follow the channel of an invite (private channels are joined only this way).
pub async fn join(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: TokenRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    limit(state, ctx, "invite-use", me, Window::Minute, 30).await?;
    let now = state.now();
    let ctx2 = ctx.clone();
    let token = req.token.clone();
    let channel = state
        .db
        .transaction::<_, Uuid, Error>(move |txn| {
            Box::pin(async move {
                let invite = usable_invite(txn, &ctx2, &token, now, true).await?;
                let open: Option<ChannelRow> = db::one(
                    txn,
                    &format!("{CHANNEL_SELECT} WHERE c.id = $2"),
                    vec![me.into(), invite.channel_id.into()],
                )
                .await?;
                let c = open
                    .filter(|c| c.moderation_state == "ACTIVE")
                    .ok_or(Error::reason(ErrorCode::NotFound, "INVITE_LINK_UNAVAILABLE"))?;
                if !c.following && add_follower(txn, c.id, me, now).await? {
                    db::exec(
                        txn,
                        "UPDATE messaging_channel_invites SET use_count = use_count + 1 WHERE id = $1",
                        vec![invite.id.into()],
                    )
                    .await?;
                }
                Ok(c.id)
            })
        })
        .await
        .map_err(Error::from)?;
    one(state, ctx, me, channel).await
}

// ------------------------------------------------------------------------- audit log

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(FromQueryResult)]
struct AuditRow {
    id: Uuid,
    actor_principal_id: Uuid,
    action: String,
    target_principal_id: Option<Uuid>,
    target_id: Option<Uuid>,
    occurred_at: DateTime<Utc>,
}

/// What the owner and admins did (owner and admins only): who acted on what, when.
pub async fn audit_log(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    q: AuditQuery,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    staff_only(&row, me)?;
    let page = match q.limit {
        None => 50,
        Some(n) if (1..=100).contains(&n) => n,
        Some(_) => return Err(Error::validation("limit must be between 1 and 100")),
    };
    let (at, after) = match q.cursor.as_deref() {
        None | Some("") => (None, None),
        Some(raw) => {
            let (a, i) = crate::operator_reads::decode_cursor(raw)
                .ok_or_else(|| Error::validation("cursor is invalid"))?;
            (Some(a), Some(i))
        }
    };
    let rows: Vec<AuditRow> = db::all(
        &state.db,
        "SELECT id, actor_principal_id, action, target_principal_id, target_id, occurred_at FROM messaging_channel_audit \
          WHERE channel_id = $1 AND ($2::timestamptz IS NULL OR (occurred_at, id) < ($2, $3)) \
          ORDER BY occurred_at DESC, id DESC LIMIT $4",
        vec![id.into(), at.into(), after.into(), (page + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > page;
    let shown: Vec<&AuditRow> = rows.iter().take(page as usize).collect();
    let next = if more {
        shown
            .last()
            .map(|r| crate::operator_reads::encode_cursor(r.occurred_at, r.id))
    } else {
        None
    };
    Ok(json!({
        "items": shown.iter().map(|r| json!({
            "entryId": r.id,
            "action": r.action,
            "actorPrincipalId": r.actor_principal_id,
            "targetPrincipalId": r.target_principal_id,
            "targetId": r.target_id,
            "occurredAt": rfc3339(r.occurred_at),
        })).collect::<Vec<_>>(),
        "nextCursor": next,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_only_their_digest_is_kept() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert!(a.starts_with(TOKEN_PREFIX) && a.len() == TOKEN_PREFIX.len() + 43);
        assert_eq!(digest(&a).len(), 64);
        assert_ne!(digest(&a), digest(&b));
    }
}

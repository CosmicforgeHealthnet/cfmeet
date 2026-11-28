//! Channels (ADR 0069, completed by ADR 0072): one-to-many broadcasts in Messaging.
//!
//! * [`mod@self`]: the channel itself (unique handle, category, language, country, PUBLIC /
//!   PRIVATE, profile picture), discovery (search, suggestions, popular / trending / new /
//!   verified, filters, keyset cursors), following, muting, unread counts and the one
//!   access rule every read and write goes through ([`ChannelRow`]).
//! * [`posts`]: text, photos, videos, voice notes, audio, documents, GIFs, stickers, link
//!   previews (client-supplied, like chat link cards), polls, scheduled posts, pins, edits,
//!   reactions, deduplicated views, stars and forwards into chats.
//! * [`admins`]: admins with permissions, ownership transfer, invite links, the audit log.
//! * [`safety`]: reports, verification applications (decided only by HQ through the admin
//!   bridge, `crate::operator_commands::channels`) and the moderation history.
//! * [`analytics`]: counts of recorded events for owners and admins (never a person).
//!
//! Access: a PRIVATE channel (and any SUSPENDED / BANNED one) is visible only to its
//! members (owner, admins, followers); anyone else gets `404`. Posts of a SUSPENDED channel
//! are readable by its owner and admins only, of a BANNED one by its owner only. Followers
//! are never listed to anyone, and no answer carries a follower's identity.

pub mod admins;
pub mod analytics;
pub mod posts;
pub mod safety;

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    clean_text,
    media::{self, MediaKind, MediaRow, Purpose},
    not_found,
};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    state::AppState,
};

pub use posts::{create_post, delete_post, posts, react, PostRequest};

pub const MAX_CHANNELS_PER_OWNER: i64 = 10;
pub const MAX_NAME: usize = 80;
pub const MAX_DESCRIPTION: usize = 500;
pub const MAX_POST: usize = 4000;
pub const PAGE: i64 = 30;
/// Discovery pages: default and largest.
pub const DISCOVER_DEFAULT: i64 = 50;
pub const DISCOVER_MAX: i64 = 50;
/// Trending looks at this many days of recorded follows, reactions and views.
pub const TRENDING_DAYS: i64 = 7;
/// Trending is ranked, not keyset-paged: at most this many entries.
pub const TRENDING_MAX: i64 = 200;
pub const SUGGESTIONS: i64 = 8;

/// Categories a channel may choose.
pub const CATEGORIES: &[&str] = &[
    "NEWS",
    "ENTERTAINMENT",
    "SPORTS",
    "BUSINESS",
    "TECHNOLOGY",
    "EDUCATION",
    "HEALTH",
    "LIFESTYLE",
    "MUSIC",
    "GAMING",
    "SCIENCE",
    "POLITICS",
    "RELIGION",
    "TRAVEL",
    "FOOD",
    "FASHION",
    "ART",
    "COMMUNITY",
    "GOVERNMENT",
    "ORGANIZATION",
    "OTHER",
];

/// Handles nobody may take (and words no handle may contain): impersonation of the
/// platform, its staff or a verification state.
pub const RESERVED_HANDLES: &[&str] = &[
    "admin",
    "administrator",
    "support",
    "help",
    "helpdesk",
    "system",
    "root",
    "moderator",
    "moderation",
    "staff",
    "security",
    "settings",
    "account",
    "accounts",
    "billing",
    "api",
    "www",
    "about",
    "privacy",
    "terms",
    "legal",
    "channel",
    "channels",
    "status",
    "updates",
    "meet",
    "meeting",
    "meetings",
    "null",
    "undefined",
    "everyone",
    "here",
    "owner",
    "team",
    "trust",
    "safety",
    "abuse",
    "report",
    "reports",
    "notifications",
];
const RESERVED_WORDS: &[&str] = &["cfmeet", "cosmicforge", "official", "verified"];

// ------------------------------------------------------------------------------ rows

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct ChannelRow {
    pub id: Uuid,
    pub owner_principal_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub handle: Option<String>,
    pub category: Option<String>,
    pub language: Option<String>,
    pub country: Option<String>,
    pub visibility: String,
    pub avatar_media_id: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    pub moderation_state: String,
    pub followers: i64,
    pub stats_since: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub following: bool,
    pub muted: bool,
    pub can_post: Option<bool>,
    pub can_edit: Option<bool>,
    pub can_manage_admins: Option<bool>,
    pub unread: i64,
}

/// `$1` = caller.
pub(crate) const CHANNEL_SELECT: &str = "SELECT c.id, c.owner_principal_id, c.name, c.description, c.handle, c.category, c.language, c.country, c.visibility, \
        c.avatar_media_id, c.verified_at, c.moderation_state, c.follower_count AS followers, c.stats_since, \
        c.created_at, \
        (mf.principal_id IS NOT NULL) AS following, coalesce(mf.muted, false) AS muted, \
        a.can_post, a.can_edit, a.can_manage_admins, \
        coalesce((SELECT count(*) FROM messaging_channel_posts p WHERE p.channel_id = c.id AND mf.principal_id IS NOT NULL \
            AND p.created_at > mf.last_read_at AND p.author_principal_id <> $1), 0) AS unread \
   FROM messaging_channels c \
   LEFT JOIN messaging_channel_followers mf ON mf.channel_id = c.id AND mf.principal_id = $1 \
   LEFT JOIN messaging_channel_admins a ON a.channel_id = c.id AND a.principal_id = $1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Owner,
    Admin,
    Follower,
    Visitor,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "OWNER",
            Role::Admin => "ADMIN",
            Role::Follower => "FOLLOWER",
            Role::Visitor => "NONE",
        }
    }
}

/// What the caller may do as an owner or admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Perms {
    pub post: bool,
    pub edit: bool,
    pub manage_admins: bool,
}

impl ChannelRow {
    pub fn role(&self, me: Uuid) -> Role {
        if self.owner_principal_id == me {
            Role::Owner
        } else if self.can_post.is_some() {
            Role::Admin
        } else if self.following {
            Role::Follower
        } else {
            Role::Visitor
        }
    }

    pub fn perms(&self, me: Uuid) -> Perms {
        match self.role(me) {
            Role::Owner => Perms {
                post: true,
                edit: true,
                manage_admins: true,
            },
            Role::Admin => Perms {
                post: self.can_post.unwrap_or(false),
                edit: self.can_edit.unwrap_or(false),
                manage_admins: self.can_manage_admins.unwrap_or(false),
            },
            _ => Perms::default(),
        }
    }

    pub fn staff(&self, me: Uuid) -> bool {
        matches!(self.role(me), Role::Owner | Role::Admin)
    }

    pub fn private(&self) -> bool {
        self.visibility == "PRIVATE"
    }

    /// Open for posting, following, invites and edits (ACTIVE or RESTRICTED).
    pub fn open(&self) -> bool {
        matches!(self.moderation_state.as_str(), "ACTIVE" | "RESTRICTED")
    }

    /// The channel's details (name, picture, state) are visible to the caller.
    pub fn detail_visible(&self, me: Uuid) -> bool {
        if self.private() || !self.open() {
            self.role(me) != Role::Visitor
        } else {
            true
        }
    }

    /// The caller may read its posts and their media.
    pub fn posts_readable(&self, me: Uuid) -> bool {
        self.detail_visible(me)
            && match self.moderation_state.as_str() {
                "ACTIVE" | "RESTRICTED" => true,
                "SUSPENDED" => self.staff(me),
                _ => self.role(me) == Role::Owner,
            }
    }

    /// Anyone may start following (PUBLIC and ACTIVE; private channels only by invite).
    pub fn followable(&self) -> bool {
        !self.private() && self.moderation_state == "ACTIVE"
    }

    /// 409 unless the channel is open (ACTIVE / RESTRICTED).
    pub fn require_open(&self) -> Result<()> {
        if self.open() {
            return Ok(());
        }
        Err(Error::Code {
            code: ErrorCode::Conflict,
            reason: Some(if self.moderation_state == "BANNED" {
                "CHANNEL_BANNED"
            } else {
                "CHANNEL_SUSPENDED"
            }),
            message: Some("This channel was suspended by CF Meet".into()),
        })
    }
}

/// 403 with a reason.
pub(crate) fn forbidden(reason: &'static str, message: &str) -> Error {
    Error::Code {
        code: ErrorCode::PrincipalNotAuthorized,
        reason: Some(reason),
        message: Some(message.into()),
    }
}

/// The channel as the caller sees it, or 404 (also when it is hidden from the caller).
pub(crate) async fn load<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    me: Uuid,
    id: Uuid,
) -> Result<ChannelRow> {
    let row: Option<ChannelRow> = db::one(
        db,
        &format!("{CHANNEL_SELECT} WHERE c.environment_id = $2 AND c.id = $3"),
        vec![me.into(), ctx.scope.environment_id.into(), id.into()],
    )
    .await?;
    row.filter(|r| r.detail_visible(me)).ok_or_else(not_found)
}

/// Channels by id in the given order (hidden ones dropped).
async fn rows_for<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    me: Uuid,
    ids: &[Uuid],
) -> Result<Vec<ChannelRow>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<ChannelRow> = db::all(
        db,
        &format!("{CHANNEL_SELECT} WHERE c.environment_id = $2 AND c.id = ANY($3)"),
        vec![
            me.into(),
            ctx.scope.environment_id.into(),
            ids.to_vec().into(),
        ],
    )
    .await?;
    let mut by: HashMap<Uuid, ChannelRow> = rows.into_iter().map(|r| (r.id, r)).collect();
    Ok(ids
        .iter()
        .filter_map(|id| by.remove(id))
        .filter(|r| r.detail_visible(me))
        .collect())
}

pub(crate) async fn media_of<C: ConnectionTrait>(
    db: &C,
    ids: Vec<Uuid>,
) -> Result<HashMap<Uuid, MediaRow>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(db::all::<MediaRow, _>(
        db,
        "SELECT id, tenant_id, application_id, environment_id, owner_principal_id, purpose, kind, media_type, \
           size_bytes, part_bytes, part_count, duration_ms, state, created_at, completed_at, file_name \
           FROM messaging_social_media WHERE id = ANY($1)",
        vec![ids.into()],
    )
    .await?
    .into_iter()
    .map(|m| (m.id, m))
    .collect())
}

/// The caller's views of `rows` (each with its latest readable post).
pub(crate) async fn views<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    rows: Vec<ChannelRow>,
) -> Result<Vec<Value>> {
    let readable: Vec<Uuid> = rows
        .iter()
        .filter(|r| r.posts_readable(me))
        .map(|r| r.id)
        .collect();
    let last = posts::latest(db, me, &readable).await?;
    let avatars = media_of(db, rows.iter().filter_map(|r| r.avatar_media_id).collect()).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let perms = r.perms(me);
            let role = r.role(me);
            json!({
                "channelId": r.id,
                "name": r.name,
                "description": r.description,
                "handle": r.handle,
                "category": r.category,
                "language": r.language,
                "country": r.country,
                "visibility": r.visibility,
                "avatar": r.avatar_media_id.and_then(|m| avatars.get(&m)).map(|m| media::view(m, None)),
                "verified": r.verified_at.is_some(),
                "verifiedAt": r.verified_at.map(rfc3339),
                "moderationState": r.moderation_state,
                "ownerPrincipalId": r.owner_principal_id,
                "followerCount": r.followers,
                "following": r.following,
                "muted": r.muted,
                "isOwner": role == Role::Owner,
                "myRole": role.as_str(),
                "permissions": {
                    "post": perms.post,
                    "edit": perms.edit,
                    "manageAdmins": perms.manage_admins,
                },
                "unreadCount": r.unread,
                "lastPost": last.get(&r.id).cloned().unwrap_or(Value::Null),
                "createdAt": rfc3339(r.created_at),
            })
        })
        .collect())
}

async fn one(state: &AppState, ctx: &TrustedContext, me: Uuid, id: Uuid) -> Result<Value> {
    let row = load(&state.db, ctx, me, id).await?;
    let readable = row.posts_readable(me);
    let staff = row.staff(me);
    let mut v = views(&state.db, me, vec![row])
        .await?
        .into_iter()
        .next()
        .ok_or_else(not_found)?;
    v["pinnedPosts"] = if readable {
        json!(posts::pinned(&state.db, me, id, staff, state.now()).await?)
    } else {
        json!([])
    };
    Ok(v)
}

// ------------------------------------------------------------------------- helpers

/// A rate limit per acting principal (`429` with Retry-After when exceeded).
pub(crate) async fn limit(
    state: &AppState,
    ctx: &TrustedContext,
    what: &str,
    actor: Uuid,
    window: Window,
    max: i64,
) -> Result<()> {
    let key = format!(
        "channel-{what}:{}:{}",
        ctx.scope.environment_id.simple(),
        actor.simple()
    );
    if let Some(retry) = limits::hit(&state.db, &key, window, max, state.now()).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    Ok(())
}

/// One entry of the channel's own audit log (owner / admin actions).
pub(crate) async fn audit<C: ConnectionTrait>(
    db: &C,
    channel: Uuid,
    actor: Uuid,
    action: &str,
    target_principal: Option<Uuid>,
    target: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        db,
        "INSERT INTO messaging_channel_audit (id, channel_id, actor_principal_id, action, target_principal_id, target_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        vec![
            Uuid::new_v4().into(),
            channel.into(),
            actor.into(),
            action.into(),
            target_principal.into(),
            target.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Follows (or keeps following) `who` in `txn`: counts and records a FOLLOW once.
pub(crate) async fn add_follower<C: ConnectionTrait>(
    txn: &C,
    channel: Uuid,
    who: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    let n = db::exec(
        txn,
        "INSERT INTO messaging_channel_followers (channel_id, principal_id, followed_at, last_read_at) VALUES ($1, $2, $3, $3) \
         ON CONFLICT DO NOTHING",
        vec![channel.into(), who.into(), now.into()],
    )
    .await?;
    if n == 1 {
        db::exec(
            txn,
            "UPDATE messaging_channels SET follower_count = follower_count + 1 WHERE id = $1",
            vec![channel.into()],
        )
        .await?;
        follow_event(txn, channel, "FOLLOW", now).await?;
    }
    Ok(n == 1)
}

async fn follow_event<C: ConnectionTrait>(
    txn: &C,
    channel: Uuid,
    kind: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        txn,
        "INSERT INTO messaging_channel_follow_events (id, channel_id, kind, occurred_at) VALUES ($1, $2, $3, $4)",
        vec![Uuid::new_v4().into(), channel.into(), kind.into(), now.into()],
    )
    .await?;
    Ok(())
}

/// `None` when absent, `Some(None)` for an explicit `null`.
fn double<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

// ----------------------------------------------------------------------- validation

/// Why a handle cannot be used (`None`: it can, if nobody holds it).
pub fn handle_problem(handle: &str) -> Option<&'static str> {
    let valid = handle.len() >= 5
        && handle.len() <= 32
        && handle.as_bytes()[0].is_ascii_lowercase()
        && handle
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !handle.ends_with('_')
        && !handle.contains("__");
    if !valid {
        return Some("INVALID");
    }
    if RESERVED_HANDLES.contains(&handle) || RESERVED_WORDS.iter().any(|w| handle.contains(w)) {
        return Some("RESERVED");
    }
    None
}

/// The canonical (lower-case, without `@`) spelling of a handle the caller typed.
pub fn normalize_handle(raw: &str) -> String {
    raw.trim().trim_start_matches('@').to_ascii_lowercase()
}

fn valid_language(v: &str) -> bool {
    let (lang, region) = match v.split_once('-') {
        Some((l, r)) => (l, Some(r)),
        None => (v, None),
    };
    (2..=3).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_lowercase())
        && region.is_none_or(|r| r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase()))
}

fn valid_country(v: &str) -> bool {
    v.len() == 2 && v.bytes().all(|b| b.is_ascii_uppercase())
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// `""` clears it.
    #[serde(default)]
    pub description: Option<String>,
    /// Unique per environment: 5-32 of `a-z 0-9 _`, starting with a letter; `""` clears it
    /// (owner only).
    #[serde(default)]
    pub handle: Option<String>,
    /// One of [`CATEGORIES`]; `""` clears it.
    #[serde(default)]
    pub category: Option<String>,
    /// BCP 47 language (`en`, `pt-BR`); `""` clears it.
    #[serde(default)]
    pub language: Option<String>,
    /// ISO 3166-1 alpha-2 country (`NG`); `""` clears it.
    #[serde(default)]
    pub country: Option<String>,
    /// `PUBLIC` (default) or `PRIVATE` (owner only).
    #[serde(default)]
    pub visibility: Option<String>,
    /// A COMPLETE channel image upload of the caller; `null` removes the picture.
    #[serde(default, deserialize_with = "double")]
    pub avatar_media_id: Option<Option<Uuid>>,
}

/// A field: absent (`None`), cleared (`Some(None)`) or set.
type Change<T> = Option<Option<T>>;

struct Fields {
    name: Option<String>,
    description: Change<String>,
    handle: Change<String>,
    category: Change<String>,
    language: Change<String>,
    country: Change<String>,
    visibility: Option<&'static str>,
}

fn optional(
    v: &Option<String>,
    check: impl Fn(&str) -> bool,
    what: &str,
) -> Result<Change<String>> {
    match v.as_deref().map(str::trim) {
        None => Ok(None),
        Some("") => Ok(Some(None)),
        Some(s) if check(s) => Ok(Some(Some(s.to_string()))),
        Some(_) => Err(Error::validation(format!("{what} is not valid"))),
    }
}

fn fields(req: &ChannelRequest, creating: bool) -> Result<Fields> {
    let name = match &req.name {
        None if creating => return Err(Error::validation("Give the channel a name")),
        None => None,
        Some(_) => Some(
            clean_text(&req.name, MAX_NAME, "A channel name")?
                .ok_or_else(|| Error::validation("Give the channel a name"))?,
        ),
    };
    let description = match &req.description {
        None => None,
        Some(_) => Some(clean_text(
            &req.description,
            MAX_DESCRIPTION,
            "A description",
        )?),
    };
    let handle = match req.handle.as_deref().map(normalize_handle) {
        None => None,
        Some(h) if h.is_empty() => Some(None),
        Some(h) => match handle_problem(&h) {
            Some("RESERVED") => {
                return Err(Error::Code {
                    code: ErrorCode::ValidationError,
                    reason: Some("HANDLE_RESERVED"),
                    message: Some("This username is reserved".into()),
                })
            }
            Some(_) => return Err(Error::Code {
                code: ErrorCode::ValidationError,
                reason: Some("HANDLE_INVALID"),
                message: Some(
                    "A username is 5-32 lower-case letters, digits or _, starting with a letter"
                        .into(),
                ),
            }),
            None => Some(Some(h)),
        },
    };
    let category = optional(&req.category, |c| CATEGORIES.contains(&c), "The category")?;
    let language = optional(&req.language, valid_language, "The language")?;
    let country = optional(&req.country, valid_country, "The country")?;
    let visibility = match req.visibility.as_deref() {
        None => None,
        Some("PUBLIC") => Some("PUBLIC"),
        Some("PRIVATE") => Some("PRIVATE"),
        Some(_) => return Err(Error::validation("visibility must be PUBLIC or PRIVATE")),
    };
    Ok(Fields {
        name,
        description,
        handle,
        category,
        language,
        country,
        visibility,
    })
}

/// A unique-index violation on the handle is `409 HANDLE_TAKEN`.
fn handle_taken(e: sea_orm::DbErr) -> Error {
    if e.to_string().contains("ux_messaging_channels_handle") {
        Error::Code {
            code: ErrorCode::Conflict,
            reason: Some("HANDLE_TAKEN"),
            message: Some("This username is taken".into()),
        }
    } else {
        e.into()
    }
}

async fn require_free_handle<C: ConnectionTrait>(
    db: &C,
    env: Uuid,
    handle: &str,
    except: Option<Uuid>,
) -> Result<()> {
    let n = db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_channels WHERE environment_id = $1 AND handle = $2 AND ($3::uuid IS NULL OR id <> $3)",
        vec![env.into(), handle.into(), except.into()],
    )
    .await?;
    if n > 0 {
        return Err(Error::Code {
            code: ErrorCode::Conflict,
            reason: Some("HANDLE_TAKEN"),
            message: Some("This username is taken".into()),
        });
    }
    Ok(())
}

async fn avatar(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    change: Option<Option<Uuid>>,
) -> Result<Option<Option<Uuid>>> {
    match change {
        Some(Some(m)) => {
            let row = media::usable(&state.db, ctx, me, Purpose::Channel, m).await?;
            if row.media_kind() != Some(MediaKind::Image) {
                return Err(Error::validation("A channel picture must be a photo"));
            }
            Ok(Some(Some(m)))
        }
        other => Ok(other),
    }
}

// ------------------------------------------------------------------------ channel CRUD

pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: ChannelRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let env = ctx.scope.environment_id;
    let f = fields(&req, true)?;
    let picture = avatar(state, ctx, me, req.avatar_media_id).await?.flatten();
    limit(state, ctx, "create", me, Window::Hour, 10).await?;
    let owned = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_channels WHERE environment_id = $1 AND owner_principal_id = $2",
        vec![env.into(), me.into()],
    )
    .await?;
    if owned >= MAX_CHANNELS_PER_OWNER {
        return Err(Error::reason(ErrorCode::QuotaExceeded, "TOO_MANY_CHANNELS"));
    }
    let handle = f.handle.flatten();
    if let Some(h) = &handle {
        require_free_handle(&state.db, env, h, None).await?;
    }
    let id = Uuid::new_v4();
    let now = state.now();
    let ctx2 = ctx.clone();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                db::exec(
                    txn,
                    "INSERT INTO messaging_channels (id, tenant_id, application_id, environment_id, owner_principal_id, name, \
                       description, handle, category, language, country, visibility, avatar_media_id, created_at, updated_at, \
                       stats_since) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14, $14)",
                    vec![
                        id.into(),
                        ctx2.scope.tenant_id.into(),
                        ctx2.scope.application_id.into(),
                        env.into(),
                        me.into(),
                        f.name.into(),
                        f.description.flatten().into(),
                        handle.into(),
                        f.category.flatten().into(),
                        f.language.flatten().into(),
                        f.country.flatten().into(),
                        f.visibility.unwrap_or("PUBLIC").into(),
                        picture.into(),
                        now.into(),
                    ],
                )
                .await
                .map_err(handle_taken)?;
                add_follower(txn, id, me, now).await?;
                audit(txn, id, me, "CHANNEL_CREATED", None, None, now).await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    one(state, ctx, me, id).await
}

pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    one(state, ctx, acting.principal_id, id).await
}

pub async fn update(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ChannelRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if !row.perms(me).edit {
        return Err(forbidden(
            "NOT_CHANNEL_EDITOR",
            "Only the channel's owner or an admin who can edit can do this",
        ));
    }
    row.require_open()?;
    let f = fields(&req, false)?;
    if (f.handle.is_some() || f.visibility.is_some()) && row.role(me) != Role::Owner {
        return Err(forbidden(
            "NOT_CHANNEL_OWNER",
            "Only the channel's owner can change its username or visibility",
        ));
    }
    if row.verified_at.is_some() && f.handle.is_some() && f.handle.clone().flatten() != row.handle {
        return Err(Error::Code {
            code: ErrorCode::Conflict,
            reason: Some("VERIFIED_HANDLE_LOCKED"),
            message: Some("A verified channel keeps its username".into()),
        });
    }
    if let Some(Some(h)) = &f.handle {
        require_free_handle(&state.db, ctx.scope.environment_id, h, Some(id)).await?;
    }
    let picture = avatar(state, ctx, me, req.avatar_media_id).await?;
    let now = state.now();
    db::exec(
        &state.db,
        "UPDATE messaging_channels SET name = coalesce($2, name), \
           description = CASE WHEN $3 THEN $4 ELSE description END, \
           handle = CASE WHEN $5 THEN $6 ELSE handle END, \
           category = CASE WHEN $7 THEN $8 ELSE category END, \
           language = CASE WHEN $9 THEN $10 ELSE language END, \
           country = CASE WHEN $11 THEN $12 ELSE country END, \
           visibility = coalesce($13, visibility), \
           avatar_media_id = CASE WHEN $14 THEN $15 ELSE avatar_media_id END, \
           updated_at = $16 WHERE id = $1",
        vec![
            id.into(),
            f.name.into(),
            f.description.is_some().into(),
            f.description.flatten().into(),
            f.handle.is_some().into(),
            f.handle.flatten().into(),
            f.category.is_some().into(),
            f.category.flatten().into(),
            f.language.is_some().into(),
            f.language.flatten().into(),
            f.country.is_some().into(),
            f.country.flatten().into(),
            f.visibility.map(str::to_string).into(),
            picture.is_some().into(),
            picture.flatten().into(),
            now.into(),
        ],
    )
    .await
    .map_err(handle_taken)?;
    audit(&state.db, id, me, "CHANNEL_UPDATED", None, None, now).await?;
    one(state, ctx, me, id).await
}

pub async fn delete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if row.role(me) != Role::Owner {
        return Err(forbidden(
            "NOT_CHANNEL_OWNER",
            "Only the channel's owner can do this",
        ));
    }
    db::exec(
        &state.db,
        "DELETE FROM messaging_channels WHERE id = $1",
        vec![id.into()],
    )
    .await?;
    Ok(json!({ "channelId": id, "deleted": true }))
}

pub async fn set_following(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    follow: bool,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    let now = state.now();
    if follow {
        if row.following {
            return one(state, ctx, me, id).await;
        }
        if row.private() {
            // Visible only to members; a member who unfollowed rejoins by invite.
            return Err(forbidden(
                "INVITE_REQUIRED",
                "Join this private channel with an invite link",
            ));
        }
        if !row.followable() {
            return Err(Error::Code {
                code: ErrorCode::Conflict,
                reason: Some("CHANNEL_RESTRICTED"),
                message: Some("This channel isn't taking new followers".into()),
            });
        }
        limit(state, ctx, "follow", me, Window::Hour, 200).await?;
        state
            .db
            .transaction::<_, (), Error>(move |txn| {
                Box::pin(async move {
                    add_follower(txn, id, me, now).await?;
                    Ok(())
                })
            })
            .await
            .map_err(Error::from)?;
    } else {
        if row.role(me) == Role::Owner {
            return Err(Error::Code {
                code: ErrorCode::Conflict,
                reason: Some("OWNER_FOLLOWS"),
                message: Some("The owner always follows their channel".into()),
            });
        }
        state
            .db
            .transaction::<_, (), Error>(move |txn| {
                Box::pin(async move {
                    let n = db::exec(
                        txn,
                        "DELETE FROM messaging_channel_followers WHERE channel_id = $1 AND principal_id = $2",
                        vec![id.into(), me.into()],
                    )
                    .await?;
                    if n == 1 {
                        db::exec(
                            txn,
                            "UPDATE messaging_channels SET follower_count = greatest(follower_count - 1, 0) WHERE id = $1",
                            vec![id.into()],
                        )
                        .await?;
                        follow_event(txn, id, "UNFOLLOW", now).await?;
                    }
                    Ok(())
                })
            })
            .await
            .map_err(Error::from)?;
        // A private channel is hidden once left.
        if row.private() && row.role(me) == Role::Follower {
            return Ok(json!({ "channelId": id, "following": false }));
        }
    }
    one(state, ctx, me, id).await
}

/// Mute or unmute a followed channel (its unread count stays; clients stop alerting).
pub async fn set_muted(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    muted: bool,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if !row.following {
        return Err(Error::Code {
            code: ErrorCode::Conflict,
            reason: Some("NOT_FOLLOWING"),
            message: Some("Follow the channel first".into()),
        });
    }
    db::exec(
        &state.db,
        "UPDATE messaging_channel_followers SET muted = $3 WHERE channel_id = $1 AND principal_id = $2",
        vec![id.into(), me.into(), muted.into()],
    )
    .await?;
    one(state, ctx, me, id).await
}

/// Channels the caller follows, owns or administers; latest activity first, muted last.
pub async fn following(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let me = acting.principal_id;
    let rows: Vec<ChannelRow> = db::all(
        &state.db,
        &format!(
            "{CHANNEL_SELECT} WHERE c.environment_id = $2 AND (mf.principal_id IS NOT NULL OR c.owner_principal_id = $1 \
               OR a.principal_id IS NOT NULL) \
             ORDER BY coalesce(mf.muted, false), \
               coalesce((SELECT max(p.created_at) FROM messaging_channel_posts p WHERE p.channel_id = c.id), c.created_at) DESC \
             LIMIT 200"
        ),
        vec![me.into(), ctx.scope.environment_id.into()],
    )
    .await?;
    let rows: Vec<ChannelRow> = rows.into_iter().filter(|r| r.detail_visible(me)).collect();
    Ok(json!({ "channels": views(&state.db, me, rows).await? }))
}

// ------------------------------------------------------------------------- discovery

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoverQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    /// `POPULAR` (default), `TRENDING`, `NEW` or `VERIFIED`.
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Popular,
    Trending,
    New,
    Verified,
}

/// A discovery cursor: `p.<followers>.<id>`, `n.<micros>.<id>` or `t.<offset>`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cursor {
    Count(i64, Uuid),
    Time(DateTime<Utc>, Uuid),
    Offset(i64),
}

fn parse_cursor(raw: &str, sort: Sort) -> Option<Cursor> {
    let mut parts = raw.splitn(3, '.');
    let tag = parts.next()?;
    match (tag, sort) {
        ("p", Sort::Popular | Sort::Verified) => {
            let n: i64 = parts.next()?.parse().ok()?;
            let id = Uuid::parse_str(parts.next()?).ok()?;
            Some(Cursor::Count(n, id))
        }
        ("n", Sort::New) => {
            let micros: i64 = parts.next()?.parse().ok()?;
            let at = Utc.timestamp_micros(micros).single()?;
            let id = Uuid::parse_str(parts.next()?).ok()?;
            Some(Cursor::Time(at, id))
        }
        ("t", Sort::Trending) => {
            let off: i64 = parts.next()?.parse().ok()?;
            (0..TRENDING_MAX)
                .contains(&off)
                .then_some(Cursor::Offset(off))
        }
        _ => None,
    }
}

fn like(s: &str) -> String {
    s.chars()
        .take(64)
        .flat_map(|c| match c {
            '%' | '_' | '\\' => vec!['\\', c],
            c => vec![c],
        })
        .collect()
}

#[derive(FromQueryResult)]
struct Ranked {
    id: Uuid,
    followers: i64,
    created_at: DateTime<Utc>,
}

/// The environment's PUBLIC, ACTIVE channels: search (name, handle, description
/// keywords), category / country / language filters, popular / trending / new / verified,
/// keyset pages (trending: ranked offsets, at most 200).
pub async fn directory(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    q: DiscoverQuery,
) -> Result<Value> {
    let me = acting.principal_id;
    limit(state, ctx, "discover", me, Window::Minute, 120).await?;
    let sort = match q.sort.as_deref() {
        None | Some("POPULAR") => Sort::Popular,
        Some("TRENDING") => Sort::Trending,
        Some("NEW") => Sort::New,
        Some("VERIFIED") => Sort::Verified,
        Some(_) => {
            return Err(Error::validation(
                "sort must be POPULAR, TRENDING, NEW or VERIFIED",
            ))
        }
    };
    let page = match q.limit {
        None => DISCOVER_DEFAULT,
        Some(n) if (1..=DISCOVER_MAX).contains(&n) => n,
        Some(_) => return Err(Error::validation("limit must be between 1 and 50")),
    };
    let cursor = match q.cursor.as_deref() {
        None | Some("") => None,
        Some(raw) => {
            Some(parse_cursor(raw, sort).ok_or_else(|| Error::validation("cursor is invalid"))?)
        }
    };
    if q.category
        .as_deref()
        .is_some_and(|c| !CATEGORIES.contains(&c))
    {
        return Err(Error::validation("category is not valid"));
    }
    if q.language.as_deref().is_some_and(|l| !valid_language(l)) {
        return Err(Error::validation("language is not valid"));
    }
    if q.country.as_deref().is_some_and(|c| !valid_country(c)) {
        return Err(Error::validation("country is not valid"));
    }
    let text =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(64).collect::<String>());
    let mut values: Vec<sea_orm::Value> = vec![ctx.scope.environment_id.into()];
    let mut push = |v: sea_orm::Value| {
        values.push(v);
        format!("${}", values.len())
    };
    let mut filters = vec![
        "c.environment_id = $1".to_string(),
        "c.visibility = 'PUBLIC'".to_string(),
        "c.moderation_state = 'ACTIVE'".to_string(),
    ];
    if let Some(t) = &text {
        let words = push(t.clone().into());
        let anywhere = push(format!("%{}%", like(t)).into());
        let handle = push(format!("{}%", like(&normalize_handle(t))).into());
        filters.push(format!(
            "(to_tsvector('simple', c.name || ' ' || coalesce(c.handle, '') || ' ' || coalesce(c.description, '')) \
               @@ plainto_tsquery('simple', {words}) OR c.name ILIKE {anywhere} OR c.description ILIKE {anywhere} \
               OR c.handle LIKE {handle})"
        ));
    }
    if let Some(c) = &q.category {
        filters.push(format!("c.category = {}", push(c.clone().into())));
    }
    if let Some(c) = &q.country {
        filters.push(format!("c.country = {}", push(c.clone().into())));
    }
    if let Some(l) = &q.language {
        let exact = push(l.clone().into());
        let base = push(format!("{}-%", like(l)).into());
        filters.push(format!("(c.language = {exact} OR c.language LIKE {base})"));
    }
    if sort == Sort::Verified {
        filters.push("c.verified_at IS NOT NULL".into());
    }
    let take = page + 1;
    let (sql, offset) = match sort {
        Sort::Popular | Sort::Verified => {
            if let Some(Cursor::Count(n, id)) = &cursor {
                let n = push((*n).into());
                let id = push((*id).into());
                filters.push(format!("(c.follower_count, c.id) < ({n}, {id})"));
            }
            (
                format!(
                    "SELECT c.id, c.follower_count AS followers, c.created_at FROM messaging_channels c WHERE {} \
                     ORDER BY c.follower_count DESC, c.id DESC LIMIT {take}",
                    filters.join(" AND ")
                ),
                0,
            )
        }
        Sort::New => {
            if let Some(Cursor::Time(at, id)) = &cursor {
                let at = push((*at).into());
                let id = push((*id).into());
                filters.push(format!("(c.created_at, c.id) < ({at}, {id})"));
            }
            (
                format!(
                    "SELECT c.id, c.follower_count AS followers, c.created_at FROM messaging_channels c WHERE {} \
                     ORDER BY c.created_at DESC, c.id DESC LIMIT {take}",
                    filters.join(" AND ")
                ),
                0,
            )
        }
        Sort::Trending => {
            let off = match cursor {
                Some(Cursor::Offset(o)) => o,
                _ => 0,
            };
            let since = push((state.now() - chrono::Duration::days(TRENDING_DAYS)).into());
            let take = take.min(TRENDING_MAX - off + 1).max(0);
            (
                format!(
                    "SELECT c.id, c.follower_count AS followers, c.created_at FROM messaging_channels c \
                       JOIN ({}) s ON s.channel_id = c.id WHERE {} AND s.score > 0 \
                     ORDER BY s.score DESC, c.follower_count DESC, c.id DESC OFFSET {off} LIMIT {take}",
                    trending_scores(&since),
                    filters.join(" AND ")
                ),
                off,
            )
        }
    };
    let ranked: Vec<Ranked> = db::all(&state.db, &sql, values).await?;
    let more = ranked.len() as i64 > page;
    let shown: Vec<&Ranked> = ranked.iter().take(page as usize).collect();
    let next = match (more, shown.last()) {
        (true, Some(last)) => Some(match sort {
            Sort::Popular | Sort::Verified => format!("p.{}.{}", last.followers, last.id),
            Sort::New => format!("n.{}.{}", last.created_at.timestamp_micros(), last.id),
            Sort::Trending => {
                let o = offset + page;
                if o >= TRENDING_MAX {
                    String::new()
                } else {
                    format!("t.{o}")
                }
            }
        })
        .filter(|s| !s.is_empty()),
        _ => None,
    };
    let ids: Vec<Uuid> = shown.iter().map(|r| r.id).collect();
    let rows = rows_for(&state.db, ctx, me, &ids).await?;
    Ok(json!({
        "channels": views(&state.db, me, rows).await?,
        "nextCursor": next,
    }))
}

/// Per channel: net follows ×5 + reactions + unique post views ÷10 in the window starting
/// at `since` (all recorded events; nothing estimated).
fn trending_scores(since: &str) -> String {
    format!(
        "SELECT ch.id AS channel_id, \
           coalesce((SELECT sum(CASE WHEN e.kind = 'FOLLOW' THEN 5 ELSE -5 END) FROM messaging_channel_follow_events e \
              WHERE e.channel_id = ch.id AND e.occurred_at > {since}), 0) \
         + coalesce((SELECT count(*) FROM messaging_channel_reactions r JOIN messaging_channel_posts p ON p.id = r.post_id \
              WHERE p.channel_id = ch.id AND r.reacted_at > {since}), 0) \
         + coalesce((SELECT count(*) FROM messaging_channel_post_views v JOIN messaging_channel_posts p ON p.id = v.post_id \
              WHERE p.channel_id = ch.id AND v.viewed_at > {since}), 0) / 10 AS score \
         FROM messaging_channels ch WHERE ch.environment_id = $1 AND ch.visibility = 'PUBLIC' AND ch.moderation_state = 'ACTIVE'"
    )
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuggestQuery {
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(FromQueryResult)]
struct IdRow {
    id: Uuid,
}

/// Up to 8 channels: those whose name or username starts with `q`, or (without `q`) the
/// trending, then most followed, public channels the caller does not follow yet.
pub async fn suggestions(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    q: SuggestQuery,
) -> Result<Value> {
    let me = acting.principal_id;
    limit(state, ctx, "discover", me, Window::Minute, 120).await?;
    let env = ctx.scope.environment_id;
    let text = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let ids: Vec<IdRow> = match text {
        Some(t) => {
            db::all(
                &state.db,
                "SELECT c.id FROM messaging_channels c WHERE c.environment_id = $1 AND c.visibility = 'PUBLIC' \
                   AND c.moderation_state = 'ACTIVE' AND (c.name ILIKE $2 OR c.handle LIKE $3) \
                 ORDER BY (c.verified_at IS NOT NULL) DESC, c.follower_count DESC, c.id DESC LIMIT 8",
                vec![
                    env.into(),
                    format!("{}%", like(t)).into(),
                    format!("{}%", like(&normalize_handle(t))).into(),
                ],
            )
            .await?
        }
        None => {
            let since = state.now() - chrono::Duration::days(TRENDING_DAYS);
            db::all(
                &state.db,
                &format!(
                    "SELECT c.id FROM messaging_channels c JOIN ({}) s ON s.channel_id = c.id \
                     WHERE NOT EXISTS (SELECT 1 FROM messaging_channel_followers f WHERE f.channel_id = c.id AND f.principal_id = $3) \
                     ORDER BY s.score DESC, c.follower_count DESC, c.id DESC LIMIT 8",
                    trending_scores("$2")
                ),
                vec![env.into(), since.into(), me.into()],
            )
            .await?
        }
    };
    let ids: Vec<Uuid> = ids.into_iter().map(|r| r.id).collect();
    let rows = rows_for(&state.db, ctx, me, &ids).await?;
    let avatars = media_of(
        &state.db,
        rows.iter().filter_map(|r| r.avatar_media_id).collect(),
    )
    .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "channelId": r.id,
                "name": r.name,
                "handle": r.handle,
                "verified": r.verified_at.is_some(),
                "followerCount": r.followers,
                "following": r.following,
                "avatar": r.avatar_media_id.and_then(|m| avatars.get(&m)).map(|m| media::view(m, None)),
            })
        })
        .collect();
    Ok(json!({ "suggestions": out }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupQuery {
    pub handle: String,
}

/// A username: whether it is valid, free, and the channel holding it (when visible).
pub async fn lookup(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    q: LookupQuery,
) -> Result<Value> {
    let me = acting.principal_id;
    limit(state, ctx, "discover", me, Window::Minute, 120).await?;
    let handle = normalize_handle(&q.handle);
    if handle.len() > 64 {
        return Err(Error::validation("handle is too long"));
    }
    if let Some(problem) = handle_problem(&handle) {
        return Ok(
            json!({ "handle": handle, "valid": problem != "INVALID", "available": false, "reason": problem, "channel": null }),
        );
    }
    let holder: Option<IdRow> = db::one(
        &state.db,
        "SELECT id FROM messaging_channels WHERE environment_id = $1 AND handle = $2",
        vec![ctx.scope.environment_id.into(), handle.clone().into()],
    )
    .await?;
    let channel = match &holder {
        Some(h) => rows_for(&state.db, ctx, me, &[h.id]).await?,
        None => vec![],
    };
    let channel = views(&state.db, me, channel).await?.into_iter().next();
    Ok(json!({
        "handle": handle,
        "valid": true,
        "available": holder.is_none(),
        "reason": if holder.is_some() { json!("TAKEN") } else { Value::Null },
        "channel": channel,
    }))
}

// ----------------------------------------------------------------------------- media

#[derive(FromQueryResult)]
struct MediaUse {
    channel_id: Uuid,
    avatar: bool,
}

/// Whether `viewer` may read channel media `media`: a post of a channel whose posts the
/// viewer may read, or the picture of a channel the viewer may see.
pub(crate) async fn media_readable(
    state: &AppState,
    ctx: &TrustedContext,
    viewer: Uuid,
    media: Uuid,
) -> Result<bool> {
    let uses: Vec<MediaUse> = db::all(
        &state.db,
        "SELECT p.channel_id, false AS avatar FROM messaging_channel_posts p WHERE p.media_id = $1 \
         UNION SELECT c.id AS channel_id, true AS avatar FROM messaging_channels c WHERE c.avatar_media_id = $1",
        vec![media.into()],
    )
    .await?;
    for u in uses {
        let row: Option<ChannelRow> = db::one(
            &state.db,
            &format!("{CHANNEL_SELECT} WHERE c.environment_id = $2 AND c.id = $3"),
            vec![
                viewer.into(),
                ctx.scope.environment_id.into(),
                u.channel_id.into(),
            ],
        )
        .await?;
        if let Some(r) = row {
            if (u.avatar && r.detail_visible(viewer)) || r.posts_readable(viewer) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// ------------------------------------------------------------------------- the sweep

/// The channel part of the social sweep: due scheduled posts are published and drifted
/// follower counts (an older image does not keep them) are recounted.
pub async fn sweep(state: &AppState) -> Result<u64> {
    let published = posts::publish_due(state, 100).await?;
    db::exec(
        &state.db,
        "UPDATE messaging_channels c SET follower_count = x.n FROM ( \
           SELECT ch.id, (SELECT count(*) FROM messaging_channel_followers f WHERE f.channel_id = ch.id) AS n \
             FROM messaging_channels ch ORDER BY ch.updated_at DESC LIMIT 500) x \
         WHERE c.id = x.id AND c.follower_count <> x.n",
        vec![],
    )
    .await?;
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_validated_and_reserved_words_refused() {
        assert_eq!(handle_problem("daily_news"), None);
        assert_eq!(handle_problem("news9"), None);
        for bad in [
            "abc",
            "9news",
            "Daily",
            "daily-news",
            "daily_",
            "da__ily",
            &"a".repeat(33),
        ] {
            assert_eq!(handle_problem(bad), Some("INVALID"), "{bad}");
        }
        for reserved in [
            "admin",
            "support",
            "cfmeet_news",
            "the_official_one",
            "verifiedme",
        ] {
            assert_eq!(handle_problem(reserved), Some("RESERVED"), "{reserved}");
        }
        assert_eq!(normalize_handle("  @Daily_News "), "daily_news");
    }

    #[test]
    fn languages_and_countries_are_codes() {
        assert!(valid_language("en") && valid_language("pt-BR") && valid_language("yor"));
        assert!(!valid_language("EN") && !valid_language("english") && !valid_language("pt-br"));
        assert!(valid_country("NG") && !valid_country("ng") && !valid_country("NGA"));
    }

    #[test]
    fn cursors_belong_to_their_sort() {
        let id = Uuid::new_v4();
        assert_eq!(
            parse_cursor(&format!("p.12.{id}"), Sort::Popular),
            Some(Cursor::Count(12, id))
        );
        assert_eq!(parse_cursor(&format!("p.12.{id}"), Sort::New), None);
        assert_eq!(
            parse_cursor("t.30", Sort::Trending),
            Some(Cursor::Offset(30))
        );
        assert_eq!(parse_cursor("t.9999", Sort::Trending), None);
        assert_eq!(parse_cursor("garbage", Sort::Popular), None);
    }

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(like("50%_off\\"), "50\\%\\_off\\\\");
    }
}

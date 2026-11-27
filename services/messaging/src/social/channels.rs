//! Channels: anyone creates one (at most 10 each); others find it in the directory of the
//! environment, follow it, read its posts and react with one emoji each. Only the owner
//! posts. Unread counts per follower.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    clean_text,
    media::{self, MediaKind, MediaRow, Purpose},
    not_found, Idempotent,
};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

pub const MAX_CHANNELS_PER_OWNER: i64 = 10;
pub const MAX_NAME: usize = 80;
pub const MAX_DESCRIPTION: usize = 500;
pub const MAX_POST: usize = 4000;
pub const PAGE: i64 = 30;

#[derive(Debug, Clone, FromQueryResult)]
struct ChannelRow {
    id: Uuid,
    owner_principal_id: Uuid,
    name: String,
    description: Option<String>,
    created_at: DateTime<Utc>,
    followers: i64,
    following: bool,
    unread: i64,
}

/// `$1` = caller, `$2` = environment.
const CHANNEL_SELECT: &str = "SELECT c.id, c.owner_principal_id, c.name, c.description, c.created_at, \
        (SELECT count(*) FROM messaging_channel_followers f WHERE f.channel_id = c.id) AS followers, \
        (mf.principal_id IS NOT NULL) AS following, \
        coalesce((SELECT count(*) FROM messaging_channel_posts p WHERE p.channel_id = c.id AND mf.principal_id IS NOT NULL \
            AND p.created_at > mf.last_read_at AND p.author_principal_id <> $1), 0) AS unread \
   FROM messaging_channels c \
   LEFT JOIN messaging_channel_followers mf ON mf.channel_id = c.id AND mf.principal_id = $1";

#[derive(Debug, Clone, FromQueryResult)]
struct PostRow {
    id: Uuid,
    channel_id: Uuid,
    kind: String,
    words: Option<String>,
    media_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    mine: Option<String>,
    reactions: String,
}

/// `$1` = caller.
const POST_COLUMNS: &str = "p.id, p.channel_id, p.kind, p.words, p.media_id, p.created_at, \
    (SELECT emoji FROM messaging_channel_reactions r WHERE r.post_id = p.id AND r.principal_id = $1) AS mine, \
    (SELECT coalesce(json_agg(json_build_object('emoji', x.emoji, 'count', x.n) ORDER BY x.n DESC, x.emoji), '[]'::json)::text \
       FROM (SELECT emoji, count(*) AS n FROM messaging_channel_reactions r WHERE r.post_id = p.id GROUP BY emoji) x) AS reactions";

async fn media_of<C: ConnectionTrait>(db: &C, ids: Vec<Uuid>) -> Result<HashMap<Uuid, MediaRow>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(db::all::<MediaRow, _>(
        db,
        "SELECT id, tenant_id, application_id, environment_id, owner_principal_id, purpose, kind, media_type, \
           size_bytes, part_bytes, part_count, duration_ms, state, created_at, completed_at \
           FROM messaging_social_media WHERE id = ANY($1)",
        vec![ids.into()],
    )
    .await?
    .into_iter()
    .map(|m| (m.id, m))
    .collect())
}

async fn post_views<C: ConnectionTrait>(db: &C, rows: &[PostRow]) -> Result<Vec<Value>> {
    let media = media_of(db, rows.iter().filter_map(|r| r.media_id).collect()).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let reactions: Vec<Value> = serde_json::from_str(&r.reactions).unwrap_or_default();
            json!({
                "postId": r.id,
                "channelId": r.channel_id,
                "kind": r.kind,
                "text": r.words,
                "media": r.media_id.and_then(|m| media.get(&m)).map(|m| media::view(m, None)),
                "createdAt": rfc3339(r.created_at),
                "reactions": reactions,
                "myReaction": r.mine,
            })
        })
        .collect())
}

async fn views<C: ConnectionTrait>(db: &C, me: Uuid, rows: Vec<ChannelRow>) -> Result<Vec<Value>> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let latest: Vec<PostRow> = if ids.is_empty() {
        vec![]
    } else {
        db::all(
            db,
            &format!(
                "SELECT DISTINCT ON (p.channel_id) {POST_COLUMNS} FROM messaging_channel_posts p \
                  WHERE p.channel_id = ANY($2) ORDER BY p.channel_id, p.created_at DESC"
            ),
            vec![me.into(), ids.into()],
        )
        .await?
    };
    let rendered = post_views(db, &latest).await?;
    let last: HashMap<Uuid, Value> = latest.iter().map(|p| p.channel_id).zip(rendered).collect();
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "channelId": r.id,
                "name": r.name,
                "description": r.description,
                "ownerPrincipalId": r.owner_principal_id,
                "followerCount": r.followers,
                "following": r.following,
                "isOwner": r.owner_principal_id == me,
                "unreadCount": r.unread,
                "lastPost": last.get(&r.id).cloned().unwrap_or(Value::Null),
                "createdAt": rfc3339(r.created_at),
            })
        })
        .collect())
}

async fn one(state: &AppState, ctx: &TrustedContext, me: Uuid, id: Uuid) -> Result<Value> {
    let rows: Vec<ChannelRow> = db::all(
        &state.db,
        &format!("{CHANNEL_SELECT} WHERE c.environment_id = $2 AND c.id = $3"),
        vec![me.into(), ctx.scope.environment_id.into(), id.into()],
    )
    .await?;
    views(&state.db, me, rows)
        .await?
        .into_iter()
        .next()
        .ok_or_else(not_found)
}

/// The directory: channels of the environment, most followed first, optionally matching `q`.
pub async fn directory(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    q: Option<&str>,
) -> Result<Value> {
    let pattern = q.map(str::trim).filter(|s| !s.is_empty()).map(|s| {
        let escaped: String = s
            .chars()
            .take(64)
            .flat_map(|c| match c {
                '%' | '_' | '\\' => vec!['\\', c],
                c => vec![c],
            })
            .collect();
        format!("%{escaped}%")
    });
    let rows: Vec<ChannelRow> = db::all(
        &state.db,
        &format!(
            "{CHANNEL_SELECT} WHERE c.environment_id = $2 AND ($3::text IS NULL OR c.name ILIKE $3 OR c.description ILIKE $3) \
             ORDER BY followers DESC, c.name LIMIT 50"
        ),
        vec![acting.principal_id.into(), ctx.scope.environment_id.into(), pattern.into()],
    )
    .await?;
    Ok(json!({ "channels": views(&state.db, acting.principal_id, rows).await? }))
}

/// Channels the caller follows or owns, latest activity first.
pub async fn following(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let rows: Vec<ChannelRow> = db::all(
        &state.db,
        &format!(
            "{CHANNEL_SELECT} WHERE c.environment_id = $2 AND (mf.principal_id IS NOT NULL OR c.owner_principal_id = $1) \
             ORDER BY coalesce((SELECT max(p.created_at) FROM messaging_channel_posts p WHERE p.channel_id = c.id), c.created_at) DESC \
             LIMIT 200"
        ),
        vec![acting.principal_id.into(), ctx.scope.environment_id.into()],
    )
    .await?;
    Ok(json!({ "channels": views(&state.db, acting.principal_id, rows).await? }))
}

pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    one(state, ctx, acting.principal_id, id).await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// `""` clears it.
    #[serde(default)]
    pub description: Option<String>,
}

fn fields(
    req: &ChannelRequest,
    creating: bool,
) -> Result<(Option<String>, Option<Option<String>>)> {
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
    Ok((name, description))
}

pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: ChannelRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let env = ctx.scope.environment_id;
    let (name, description) = fields(&req, true)?;
    let owned = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_channels WHERE environment_id = $1 AND owner_principal_id = $2",
        vec![env.into(), me.into()],
    )
    .await?;
    if owned >= MAX_CHANNELS_PER_OWNER {
        return Err(Error::reason(ErrorCode::QuotaExceeded, "TOO_MANY_CHANNELS"));
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
                       description, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
                    vec![
                        id.into(),
                        ctx2.scope.tenant_id.into(),
                        ctx2.scope.application_id.into(),
                        env.into(),
                        me.into(),
                        name.into(),
                        description.flatten().into(),
                        now.into(),
                    ],
                )
                .await?;
                db::exec(
                    txn,
                    "INSERT INTO messaging_channel_followers (channel_id, principal_id, followed_at, last_read_at) VALUES ($1, $2, $3, $3)",
                    vec![id.into(), me.into(), now.into()],
                )
                .await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    one(state, ctx, me, id).await
}

#[derive(FromQueryResult)]
struct OwnerRow {
    owner_principal_id: Uuid,
}

/// The channel exists (404 otherwise) and, with `owner`, the caller owns it (403).
async fn require(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    id: Uuid,
    owner: bool,
) -> Result<()> {
    let row: OwnerRow = db::one(
        &state.db,
        "SELECT owner_principal_id FROM messaging_channels WHERE id = $1 AND environment_id = $2",
        vec![id.into(), ctx.scope.environment_id.into()],
    )
    .await?
    .ok_or_else(not_found)?;
    if owner && row.owner_principal_id != me {
        return Err(Error::Code {
            code: ErrorCode::PrincipalNotAuthorized,
            reason: Some("NOT_CHANNEL_OWNER"),
            message: Some("Only the channel's owner can do this".into()),
        });
    }
    Ok(())
}

pub async fn update(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ChannelRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    require(state, ctx, me, id, true).await?;
    let (name, description) = fields(&req, false)?;
    db::exec(
        &state.db,
        "UPDATE messaging_channels SET name = coalesce($2, name), description = CASE WHEN $3 THEN $4 ELSE description END, \
           updated_at = $5 WHERE id = $1",
        vec![
            id.into(),
            name.into(),
            description.is_some().into(),
            description.flatten().into(),
            state.now().into(),
        ],
    )
    .await?;
    one(state, ctx, me, id).await
}

pub async fn delete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    require(state, ctx, acting.principal_id, id, true).await?;
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
    require(state, ctx, me, id, false).await?;
    if follow {
        db::exec(
            &state.db,
            "INSERT INTO messaging_channel_followers (channel_id, principal_id, followed_at, last_read_at) VALUES ($1, $2, $3, $3) \
             ON CONFLICT DO NOTHING",
            vec![id.into(), me.into(), state.now().into()],
        )
        .await?;
    } else {
        db::exec(
            &state.db,
            "DELETE FROM messaging_channel_followers WHERE channel_id = $1 AND principal_id = $2",
            vec![id.into(), me.into()],
        )
        .await?;
    }
    one(state, ctx, me, id).await
}

/// Newest first, 30 at a time (`before` for older). Reading the newest marks them read.
pub async fn posts(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    before: Option<DateTime<Utc>>,
) -> Result<Value> {
    let me = acting.principal_id;
    require(state, ctx, me, id, false).await?;
    let rows: Vec<PostRow> = db::all(
        &state.db,
        &format!(
            "SELECT {POST_COLUMNS} FROM messaging_channel_posts p WHERE p.channel_id = $2 \
               AND ($3::timestamptz IS NULL OR p.created_at < $3) ORDER BY p.created_at DESC LIMIT {PAGE}"
        ),
        vec![me.into(), id.into(), before.into()],
    )
    .await?;
    if before.is_none() {
        db::exec(
            &state.db,
            "UPDATE messaging_channel_followers SET last_read_at = $3 WHERE channel_id = $1 AND principal_id = $2",
            vec![id.into(), me.into(), state.now().into()],
        )
        .await?;
    }
    Ok(json!({ "posts": post_views(&state.db, &rows).await? }))
}

async fn post_view(state: &AppState, me: Uuid, post: Uuid) -> Result<Value> {
    let rows: Vec<PostRow> = db::all(
        &state.db,
        &format!("SELECT {POST_COLUMNS} FROM messaging_channel_posts p WHERE p.id = $2"),
        vec![me.into(), post.into()],
    )
    .await?;
    post_views(&state.db, &rows)
        .await?
        .into_iter()
        .next()
        .ok_or_else(not_found)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostRequest {
    /// Words, or a photo / video caption (at most 4000 characters).
    #[serde(default)]
    pub text: Option<String>,
    /// A completed channel upload of the owner.
    #[serde(default)]
    pub media_id: Option<Uuid>,
}

/// Post in the caller's channel (`201`; idempotent replay `200`).
pub async fn create_post(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: PostRequest,
    key: Option<String>,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let env = ctx.scope.environment_id;
    let idem = key.map(|k| {
        let mut body = serde_json::to_value(&req).unwrap_or(Value::Null);
        body["channelId"] = json!(id);
        Idempotent::new("channel-post.create", k, me, &body)
    });
    let replay = |stored: Value| async move {
        let post = stored["postId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok());
        match post {
            Some(p) => match post_view(state, me, p).await {
                Ok(v) => Ok((200, v)),
                Err(_) => Ok((200, stored)),
            },
            None => Ok((200, stored)),
        }
    };
    if let Some(idem) = &idem {
        if let Some(stored) = super::replayed(&state.db, env, idem, state.now()).await? {
            return replay(stored).await;
        }
    }
    require(state, ctx, me, id, true).await?;
    let text = clean_text(&req.text, MAX_POST, "A post")?;
    let kind = match req.media_id {
        None => {
            if text.is_none() {
                return Err(Error::validation("Write something or add a photo or video"));
            }
            "TEXT"
        }
        Some(m) => match media::usable(&state.db, ctx, me, Purpose::Channel, m)
            .await?
            .media_kind()
        {
            Some(MediaKind::Image) => "IMAGE",
            Some(MediaKind::Video) => "VIDEO",
            Some(MediaKind::Audio) => "AUDIO",
            None => return Err(Error::code(ErrorCode::InternalError)),
        },
    };
    let post = Uuid::new_v4();
    let now = state.now();
    let media_id = req.media_id;
    let stored = state
        .db
        .transaction::<_, Option<Value>, Error>(move |txn| {
            Box::pin(async move {
                if let Some(idem) = &idem {
                    if let Some(stored) = super::claim(txn, env, idem, now).await? {
                        return Ok(Some(stored));
                    }
                }
                db::exec(
                    txn,
                    "INSERT INTO messaging_channel_posts (id, channel_id, author_principal_id, kind, words, media_id, created_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    vec![post.into(), id.into(), me.into(), kind.into(), text.into(), media_id.into(), now.into()],
                )
                .await?;
                if let Some(idem) = &idem {
                    super::remember(txn, env, idem, &json!({ "postId": post })).await?;
                }
                Ok(None)
            })
        })
        .await
        .map_err(Error::from)?;
    if let Some(stored) = stored {
        return replay(stored).await;
    }
    Ok((201, post_view(state, me, post).await?))
}

pub async fn delete_post(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
) -> Result<Value> {
    require(state, ctx, acting.principal_id, id, true).await?;
    let n = db::exec(
        &state.db,
        "DELETE FROM messaging_channel_posts WHERE id = $1 AND channel_id = $2",
        vec![post.into(), id.into()],
    )
    .await?;
    if n == 0 {
        return Err(not_found());
    }
    Ok(json!({ "postId": post, "deleted": true }))
}

/// The caller's one reaction to a post (`None` removes it).
pub async fn react(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
    emoji: Option<String>,
) -> Result<Value> {
    let me = acting.principal_id;
    require(state, ctx, me, id, false).await?;
    let exists = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_channel_posts WHERE id = $1 AND channel_id = $2",
        vec![post.into(), id.into()],
    )
    .await?;
    if exists == 0 {
        return Err(not_found());
    }
    match emoji {
        Some(e) => {
            if !crate::engagement::valid_reaction(&e) {
                return Err(Error::validation(
                    "A reaction is one emoji or a :short_code:",
                ));
            }
            db::exec(
                &state.db,
                "INSERT INTO messaging_channel_reactions (post_id, principal_id, emoji, reacted_at) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (post_id, principal_id) DO UPDATE SET emoji = EXCLUDED.emoji, reacted_at = EXCLUDED.reacted_at",
                vec![post.into(), me.into(), e.into(), state.now().into()],
            )
            .await?;
        }
        None => {
            db::exec(
                &state.db,
                "DELETE FROM messaging_channel_reactions WHERE post_id = $1 AND principal_id = $2",
                vec![post.into(), me.into()],
            )
            .await?;
        }
    }
    post_view(state, me, post).await
}

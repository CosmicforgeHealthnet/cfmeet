//! Channel posts (ADR 0072): every kind, polls, scheduling, pins, edits, reactions, views,
//! stars and forwards.
//!
//! * Kinds: TEXT; IMAGE / VIDEO / AUDIO / DOCUMENT from a COMPLETE channel upload of the
//!   poster; VOICE (audio presented as a voice note), GIF (a GIF image or a short video),
//!   STICKER (a WebP / PNG image of at most 1 MiB); POLL (2-12 options, single or multiple
//!   choice). Text may carry a link preview the poster's device built from the link itself
//!   (the chat link-card model: the server never fetches a URL); its URL must appear in the
//!   text.
//! * `publishAt` (1 minute to 30 days ahead) keeps the post in
//!   `messaging_channel_scheduled_posts` until the sweep publishes it ([`publish_due`]).
//! * Views: one row per person and post (a person viewing again changes nothing), recorded
//!   for followers and visitors, never for the channel's owner and admins.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{forbidden, limit, load, media_of, ChannelRow, MAX_POST, PAGE};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::Window,
    social::{
        clean_text, eligible_contacts,
        media::{self, MediaKind, Purpose},
        not_found, Idempotent,
    },
    state::AppState,
};

pub const MAX_PINNED: i64 = 3;
pub const MAX_POLL_OPTIONS: usize = 12;
pub const MAX_POLL_OPTION: usize = 100;
pub const MAX_QUESTION: usize = 300;
pub const MAX_URL: usize = 2048;
pub const MAX_STICKER_BYTES: i64 = 1024 * 1024;
pub const MAX_SCHEDULE_DAYS: i64 = 30;
pub const MAX_VIEW_BATCH: usize = 100;
pub const MAX_FORWARD_TARGETS: usize = 20;
pub const POSTS_PER_HOUR: i64 = 120;

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PostRow {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author_principal_id: Uuid,
    pub kind: String,
    pub words: Option<String>,
    pub media_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub pinned_at: Option<DateTime<Utc>>,
    pub link_preview: Option<Value>,
    pub mine: Option<String>,
    pub reactions: String,
    pub starred: bool,
    pub views: i64,
}

/// `$1` = caller.
pub(crate) const POST_COLUMNS: &str = "p.id, p.channel_id, p.author_principal_id, p.kind, p.words, p.media_id, \
    p.created_at, p.edited_at, p.pinned_at, p.link_preview, \
    (SELECT emoji FROM messaging_channel_reactions r WHERE r.post_id = p.id AND r.principal_id = $1) AS mine, \
    (SELECT coalesce(json_agg(json_build_object('emoji', x.emoji, 'count', x.n) ORDER BY x.n DESC, x.emoji), '[]'::json)::text \
       FROM (SELECT emoji, count(*) AS n FROM messaging_channel_reactions r WHERE r.post_id = p.id GROUP BY emoji) x) AS reactions, \
    EXISTS (SELECT 1 FROM messaging_channel_stars s WHERE s.post_id = p.id AND s.principal_id = $1) AS starred, \
    (SELECT count(*) FROM messaging_channel_post_views v WHERE v.post_id = p.id) AS views";

#[derive(Debug, Clone, FromQueryResult)]
struct PollRow {
    post_id: Uuid,
    question: String,
    multiple: bool,
    options: Value,
    closes_at: Option<DateTime<Utc>>,
    voters: i64,
    counts: String,
    mine: String,
}

async fn polls<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    ids: Vec<Uuid>,
    now: DateTime<Utc>,
) -> Result<HashMap<Uuid, Value>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<PollRow> = db::all(
        db,
        "SELECT pl.post_id, pl.question, pl.multiple, pl.options, pl.closes_at, \
           (SELECT count(DISTINCT v.principal_id) FROM messaging_channel_poll_votes v WHERE v.post_id = pl.post_id) AS voters, \
           (SELECT coalesce(json_agg(json_build_object('i', x.option_index, 'n', x.n)), '[]'::json)::text FROM \
              (SELECT option_index, count(*) AS n FROM messaging_channel_poll_votes v WHERE v.post_id = pl.post_id GROUP BY option_index) x) AS counts, \
           (SELECT coalesce(json_agg(v.option_index ORDER BY v.option_index), '[]'::json)::text \
              FROM messaging_channel_poll_votes v WHERE v.post_id = pl.post_id AND v.principal_id = $1) AS mine \
         FROM messaging_channel_polls pl WHERE pl.post_id = ANY($2)",
        vec![me.into(), ids.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let counts: Vec<Value> = serde_json::from_str(&r.counts).unwrap_or_default();
            let by: HashMap<i64, i64> = counts
                .iter()
                .filter_map(|c| Some((c["i"].as_i64()?, c["n"].as_i64()?)))
                .collect();
            let mine: Vec<i64> = serde_json::from_str(&r.mine).unwrap_or_default();
            let options: Vec<Value> = r
                .options
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    json!({ "index": i, "text": text, "votes": by.get(&(i as i64)).copied().unwrap_or(0) })
                })
                .collect();
            (
                r.post_id,
                json!({
                    "question": r.question,
                    "multiple": r.multiple,
                    "options": options,
                    "totalVoters": r.voters,
                    "myVotes": mine,
                    "closesAt": r.closes_at.map(rfc3339),
                    "closed": r.closes_at.is_some_and(|c| c <= now),
                }),
            )
        })
        .collect())
}

/// Views of posts; `staff` callers also see the unique view count.
pub(crate) async fn post_views<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    rows: &[PostRow],
    staff: bool,
    now: DateTime<Utc>,
) -> Result<Vec<Value>> {
    let media = media_of(db, rows.iter().filter_map(|r| r.media_id).collect()).await?;
    let polls = polls(
        db,
        me,
        rows.iter()
            .filter(|r| r.kind == "POLL")
            .map(|r| r.id)
            .collect(),
        now,
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let reactions: Vec<Value> = serde_json::from_str(&r.reactions).unwrap_or_default();
            json!({
                "postId": r.id,
                "channelId": r.channel_id,
                "state": "PUBLISHED",
                "kind": r.kind,
                "text": r.words,
                "media": r.media_id.and_then(|m| media.get(&m)).map(|m| media::view(m, None)),
                "linkPreview": r.link_preview,
                "poll": polls.get(&r.id),
                "createdAt": rfc3339(r.created_at),
                "edited": r.edited_at.is_some(),
                "editedAt": r.edited_at.map(rfc3339),
                "pinned": r.pinned_at.is_some(),
                "pinnedAt": r.pinned_at.map(rfc3339),
                "reactions": reactions,
                "myReaction": r.mine,
                "starred": r.starred,
                "viewCount": if staff { json!(r.views) } else { Value::Null },
                "mine": r.author_principal_id == me,
            })
        })
        .collect())
}

/// The newest post of each channel in `ids` (the caller may read them all).
pub(crate) async fn latest<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Value>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<PostRow> = db::all(
        db,
        &format!(
            "SELECT DISTINCT ON (p.channel_id) {POST_COLUMNS} FROM messaging_channel_posts p \
              WHERE p.channel_id = ANY($2) ORDER BY p.channel_id, p.created_at DESC"
        ),
        vec![me.into(), ids.to_vec().into()],
    )
    .await?;
    let rendered = post_views(db, me, &rows, false, Utc::now()).await?;
    Ok(rows.iter().map(|p| p.channel_id).zip(rendered).collect())
}

/// The channel's pinned posts, newest pin first.
pub(crate) async fn pinned<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    channel: Uuid,
    staff: bool,
    now: DateTime<Utc>,
) -> Result<Vec<Value>> {
    let rows: Vec<PostRow> = db::all(
        db,
        &format!(
            "SELECT {POST_COLUMNS} FROM messaging_channel_posts p WHERE p.channel_id = $2 AND p.pinned_at IS NOT NULL \
             ORDER BY p.pinned_at DESC LIMIT {MAX_PINNED}"
        ),
        vec![me.into(), channel.into()],
    )
    .await?;
    post_views(db, me, &rows, staff, now).await
}

/// The channel, readable by the caller (404 when hidden, 403 when suspended for them).
async fn readable(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    id: Uuid,
) -> Result<ChannelRow> {
    let row = load(&state.db, ctx, me, id).await?;
    if !row.posts_readable(me) {
        return Err(forbidden(
            "CHANNEL_SUSPENDED",
            "This channel was suspended by CF Meet",
        ));
    }
    Ok(row)
}

async fn post_row<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    channel: Uuid,
    post: Uuid,
) -> Result<PostRow> {
    db::one(
        db,
        &format!("SELECT {POST_COLUMNS} FROM messaging_channel_posts p WHERE p.id = $2 AND p.channel_id = $3"),
        vec![me.into(), post.into(), channel.into()],
    )
    .await?
    .ok_or_else(not_found)
}

async fn post_view(
    state: &AppState,
    me: Uuid,
    channel: Uuid,
    post: Uuid,
    staff: bool,
) -> Result<Value> {
    let row = post_row(&state.db, me, channel, post).await?;
    post_views(&state.db, me, &[row], staff, state.now())
        .await?
        .into_iter()
        .next()
        .ok_or_else(not_found)
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
    let row = readable(state, ctx, me, id).await?;
    let rows: Vec<PostRow> = db::all(
        &state.db,
        &format!(
            "SELECT {POST_COLUMNS} FROM messaging_channel_posts p WHERE p.channel_id = $2 \
               AND ($3::timestamptz IS NULL OR p.created_at < $3) ORDER BY p.created_at DESC LIMIT {PAGE}"
        ),
        vec![me.into(), id.into(), before.into()],
    )
    .await?;
    if before.is_none() && row.following {
        db::exec(
            &state.db,
            "UPDATE messaging_channel_followers SET last_read_at = $3 WHERE channel_id = $1 AND principal_id = $2",
            vec![id.into(), me.into(), state.now().into()],
        )
        .await?;
    }
    Ok(json!({ "posts": post_views(&state.db, me, &rows, row.staff(me), state.now()).await? }))
}

// --------------------------------------------------------------------------- creating

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkPreview {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub site_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollRequest {
    pub question: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub multiple: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostRequest {
    /// Words, or a caption (at most 4000 characters). Not with a poll.
    #[serde(default)]
    pub text: Option<String>,
    /// A COMPLETE channel upload of the poster.
    #[serde(default)]
    pub media_id: Option<Uuid>,
    /// `VOICE` (audio), `GIF` (GIF image or video) or `STICKER` (WebP / PNG ≤ 1 MiB).
    #[serde(default)]
    pub presentation: Option<String>,
    /// The poster's device-built preview of a link in the text.
    #[serde(default)]
    pub link_preview: Option<LinkPreview>,
    #[serde(default)]
    pub poll: Option<PollRequest>,
    /// Publish later (1 minute to 30 days ahead).
    #[serde(default)]
    pub publish_at: Option<DateTime<Utc>>,
}

/// A checked link preview (its URL must appear in `text`).
fn link_preview(lp: &Option<LinkPreview>, text: Option<&str>) -> Result<Option<Value>> {
    let Some(lp) = lp else { return Ok(None) };
    let url = lp.url.trim();
    let scheme_ok = url.starts_with("https://") || url.starts_with("http://");
    if !scheme_ok || url.len() > MAX_URL || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(Error::validation(
            "linkPreview.url must be an http(s) link of at most 2048 characters",
        ));
    }
    if !text.is_some_and(|t| t.contains(url)) {
        return Err(Error::validation(
            "A link preview needs its link in the post's text",
        ));
    }
    Ok(Some(json!({
        "url": url,
        "title": clean_text(&lp.title, 200, "A link title")?,
        "description": clean_text(&lp.description, 500, "A link description")?,
        "siteName": clean_text(&lp.site_name, 100, "A site name")?,
    })))
}

fn poll_of(p: &PollRequest) -> Result<Value> {
    let question = clean_text(&Some(p.question.clone()), MAX_QUESTION, "A poll question")?
        .ok_or_else(|| Error::validation("Ask a question"))?;
    if !(2..=MAX_POLL_OPTIONS).contains(&p.options.len()) {
        return Err(Error::validation("A poll has 2 to 12 options"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut options = Vec::with_capacity(p.options.len());
    for o in &p.options {
        let o = clean_text(&Some(o.clone()), MAX_POLL_OPTION, "A poll option")?
            .ok_or_else(|| Error::validation("A poll option can't be empty"))?;
        if !seen.insert(o.to_lowercase()) {
            return Err(Error::validation("Poll options must differ"));
        }
        options.push(Value::String(o));
    }
    Ok(json!({ "question": question, "options": options, "multiple": p.multiple }))
}

/// The post's kind and media from the request (media of the poster, COMPLETE).
async fn kind_of(
    state: &AppState,
    ctx: &TrustedContext,
    me: Uuid,
    req: &PostRequest,
    text: &Option<String>,
) -> Result<&'static str> {
    if req.poll.is_some() {
        if req.media_id.is_some() || text.is_some() || req.presentation.is_some() {
            return Err(Error::validation(
                "A poll carries only its question and options",
            ));
        }
        return Ok("POLL");
    }
    let Some(m) = req.media_id else {
        if req.presentation.is_some() {
            return Err(Error::validation("presentation needs media"));
        }
        if text.is_none() {
            return Err(Error::validation(
                "Write something or add a photo, video, voice note or file",
            ));
        }
        return Ok("TEXT");
    };
    let row = media::usable(&state.db, ctx, me, Purpose::Channel, m).await?;
    let kind = row
        .media_kind()
        .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(match (kind, req.presentation.as_deref()) {
        (MediaKind::Image, None) => "IMAGE",
        (MediaKind::Video, None) => "VIDEO",
        (MediaKind::Audio, None) => "AUDIO",
        (MediaKind::Document, None) => "DOCUMENT",
        (MediaKind::Audio, Some("VOICE")) => "VOICE",
        (MediaKind::Image, Some("GIF")) if row.media_type == "image/gif" => "GIF",
        (MediaKind::Video, Some("GIF")) => "GIF",
        (MediaKind::Image, Some("STICKER"))
            if matches!(row.media_type.as_str(), "image/webp" | "image/png")
                && row.size_bytes <= MAX_STICKER_BYTES =>
        {
            "STICKER"
        }
        _ => {
            return Err(Error::validation(
                "presentation doesn't match the media (VOICE: audio; GIF: a GIF or video; STICKER: WebP / PNG up to 1 MB)",
            ))
        }
    })
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct ScheduledRow {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author_principal_id: Uuid,
    pub kind: String,
    pub words: Option<String>,
    pub media_id: Option<Uuid>,
    pub link_preview: Option<Value>,
    pub poll: Option<Value>,
    pub publish_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

const SCHEDULED_COLUMNS: &str =
    "s.id, s.channel_id, s.author_principal_id, s.kind, s.words, s.media_id, s.link_preview, s.poll, s.publish_at, s.created_at";

async fn scheduled_views<C: ConnectionTrait>(db: &C, rows: &[ScheduledRow]) -> Result<Vec<Value>> {
    let media = media_of(db, rows.iter().filter_map(|r| r.media_id).collect()).await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "postId": r.id,
                "channelId": r.channel_id,
                "state": "SCHEDULED",
                "kind": r.kind,
                "text": r.words,
                "media": r.media_id.and_then(|m| media.get(&m)).map(|m| media::view(m, None)),
                "linkPreview": r.link_preview,
                "poll": r.poll.as_ref().map(|p| json!({
                    "question": p["question"],
                    "multiple": p["multiple"],
                    "options": p["options"].as_array().cloned().unwrap_or_default().iter().enumerate()
                        .map(|(i, t)| json!({ "index": i, "text": t, "votes": 0 })).collect::<Vec<_>>(),
                    "totalVoters": 0,
                    "myVotes": [],
                    "closesAt": null,
                    "closed": false,
                })),
                "publishAt": rfc3339(r.publish_at),
                "createdAt": rfc3339(r.created_at),
                "edited": false,
                "pinned": false,
                "reactions": [],
                "myReaction": null,
                "starred": false,
                "viewCount": 0,
            })
        })
        .collect())
}

async fn scheduled_view<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<Value>> {
    let rows: Vec<ScheduledRow> = db::all(
        db,
        &format!(
            "SELECT {SCHEDULED_COLUMNS} FROM messaging_channel_scheduled_posts s WHERE s.id = $1"
        ),
        vec![id.into()],
    )
    .await?;
    Ok(scheduled_views(db, &rows).await?.into_iter().next())
}

/// Inserts a published post (and its poll) in `txn`.
#[allow(clippy::too_many_arguments)]
async fn insert_post<C: ConnectionTrait>(
    txn: &C,
    id: Uuid,
    channel: Uuid,
    author: Uuid,
    kind: &str,
    words: Option<String>,
    media_id: Option<Uuid>,
    link_preview: Option<Value>,
    poll: Option<&Value>,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        txn,
        "INSERT INTO messaging_channel_posts (id, channel_id, author_principal_id, kind, words, media_id, created_at, link_preview) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            id.into(),
            channel.into(),
            author.into(),
            kind.into(),
            words.into(),
            media_id.into(),
            now.into(),
            link_preview.into(),
        ],
    )
    .await?;
    if let Some(p) = poll {
        db::exec(
            txn,
            "INSERT INTO messaging_channel_polls (post_id, question, multiple, options) VALUES ($1, $2, $3, $4)",
            vec![
                id.into(),
                p["question"].as_str().unwrap_or_default().into(),
                p["multiple"].as_bool().unwrap_or(false).into(),
                p["options"].clone().into(),
            ],
        )
        .await?;
    }
    Ok(())
}

/// Post in a channel the caller may post in (`201`; a scheduled post answers its
/// `SCHEDULED` view; idempotent replay `200`).
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
        if let Some(p) = post {
            if let Ok(v) = post_view(state, me, id, p, true).await {
                return Ok((200, v));
            }
            if let Ok(Some(v)) = scheduled_view(&state.db, p).await {
                return Ok((200, v));
            }
        }
        Ok((200, stored))
    };
    if let Some(idem) = &idem {
        if let Some(stored) = crate::social::replayed(&state.db, env, idem, state.now()).await? {
            return replay(stored).await;
        }
    }
    let row = load(&state.db, ctx, me, id).await?;
    if !row.perms(me).post {
        return Err(forbidden(
            "NOT_CHANNEL_POSTER",
            "Only the channel's owner or an admin who can post can do this",
        ));
    }
    row.require_open()?;
    let text = clean_text(&req.text, MAX_POST, "A post")?;
    let kind = kind_of(state, ctx, me, &req, &text).await?;
    let preview = link_preview(&req.link_preview, text.as_deref())?;
    let poll = req.poll.as_ref().map(poll_of).transpose()?;
    let now = state.now();
    if let Some(at) = req.publish_at {
        if at < now + chrono::Duration::minutes(1)
            || at > now + chrono::Duration::days(MAX_SCHEDULE_DAYS)
        {
            return Err(Error::validation(
                "publishAt must be between 1 minute and 30 days from now",
            ));
        }
    }
    limit(state, ctx, "post", me, Window::Hour, POSTS_PER_HOUR).await?;
    let post = Uuid::new_v4();
    let media_id = req.media_id;
    let publish_at = req.publish_at;
    let stored = state
        .db
        .transaction::<_, Option<Value>, Error>(move |txn| {
            Box::pin(async move {
                if let Some(idem) = &idem {
                    if let Some(stored) = crate::social::claim(txn, env, idem, now).await? {
                        return Ok(Some(stored));
                    }
                }
                match publish_at {
                    Some(at) => {
                        db::exec(
                            txn,
                            "INSERT INTO messaging_channel_scheduled_posts (id, channel_id, author_principal_id, kind, words, \
                               media_id, link_preview, poll, publish_at, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                            vec![
                                post.into(),
                                id.into(),
                                me.into(),
                                kind.into(),
                                text.into(),
                                media_id.into(),
                                preview.into(),
                                poll.into(),
                                at.into(),
                                now.into(),
                            ],
                        )
                        .await?;
                        super::audit(txn, id, me, "POST_SCHEDULED", None, Some(post), now).await?;
                    }
                    None => {
                        insert_post(txn, post, id, me, kind, text, media_id, preview, poll.as_ref(), now)
                            .await?;
                    }
                }
                if let Some(idem) = &idem {
                    crate::social::remember(txn, env, idem, &json!({ "postId": post })).await?;
                }
                Ok(None)
            })
        })
        .await
        .map_err(Error::from)?;
    if let Some(stored) = stored {
        return replay(stored).await;
    }
    if publish_at.is_some() {
        let v = scheduled_view(&state.db, post)
            .await?
            .ok_or_else(not_found)?;
        return Ok((201, v));
    }
    Ok((201, post_view(state, me, id, post, true).await?))
}

/// Moves due scheduled posts into the channel (at most `max` per pass). A post whose author
/// can no longer post there is dropped; a suspended channel's posts wait.
pub async fn publish_due(state: &AppState, max: i64) -> Result<u64> {
    let now = state.now();
    db::exec(
        &state.db,
        "DELETE FROM messaging_channel_scheduled_posts s USING messaging_channels c \
          WHERE s.channel_id = c.id AND s.publish_at <= $1 AND s.author_principal_id <> c.owner_principal_id \
            AND NOT EXISTS (SELECT 1 FROM messaging_channel_admins a WHERE a.channel_id = c.id \
                              AND a.principal_id = s.author_principal_id AND a.can_post)",
        vec![now.into()],
    )
    .await?;
    let published = state
        .db
        .transaction::<_, u64, Error>(move |txn| {
            Box::pin(async move {
                let due: Vec<ScheduledRow> = db::all(
                    txn,
                    &format!(
                        "SELECT {SCHEDULED_COLUMNS} FROM messaging_channel_scheduled_posts s \
                           JOIN messaging_channels c ON c.id = s.channel_id \
                          WHERE s.publish_at <= $1 AND c.moderation_state IN ('ACTIVE', 'RESTRICTED') \
                          ORDER BY s.publish_at LIMIT $2 FOR UPDATE OF s SKIP LOCKED"
                    ),
                    vec![now.into(), max.into()],
                )
                .await?;
                let mut n = 0;
                for s in due {
                    insert_post(
                        txn,
                        s.id,
                        s.channel_id,
                        s.author_principal_id,
                        &s.kind,
                        s.words.clone(),
                        s.media_id,
                        s.link_preview.clone(),
                        s.poll.as_ref(),
                        now,
                    )
                    .await?;
                    db::exec(
                        txn,
                        "DELETE FROM messaging_channel_scheduled_posts WHERE id = $1",
                        vec![s.id.into()],
                    )
                    .await?;
                    n += 1;
                }
                Ok(n)
            })
        })
        .await
        .map_err(Error::from)?;
    if published > 0 {
        crate::metrics::inc("messaging_channel_scheduled_published_total", &[]);
    }
    Ok(published)
}

/// The channel's scheduled posts (owner and admins).
pub async fn scheduled(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if !row.staff(me) {
        return Err(forbidden(
            "NOT_CHANNEL_ADMIN",
            "Only the channel's owner and admins can do this",
        ));
    }
    let rows: Vec<ScheduledRow> = db::all(
        &state.db,
        &format!(
            "SELECT {SCHEDULED_COLUMNS} FROM messaging_channel_scheduled_posts s WHERE s.channel_id = $1 \
             ORDER BY s.publish_at LIMIT 200"
        ),
        vec![id.into()],
    )
    .await?;
    Ok(json!({ "posts": scheduled_views(&state.db, &rows).await? }))
}

/// Cancel a scheduled post (an editor, or its author while they can post).
pub async fn cancel_scheduled(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    let author: Option<ScheduledRow> = db::one(
        &state.db,
        &format!("SELECT {SCHEDULED_COLUMNS} FROM messaging_channel_scheduled_posts s WHERE s.id = $1 AND s.channel_id = $2"),
        vec![post.into(), id.into()],
    )
    .await?;
    let s = author.ok_or_else(not_found)?;
    let perms = row.perms(me);
    if !(perms.edit || (perms.post && s.author_principal_id == me)) {
        return Err(forbidden(
            "NOT_CHANNEL_EDITOR",
            "Only an editor or the post's author can do this",
        ));
    }
    db::exec(
        &state.db,
        "DELETE FROM messaging_channel_scheduled_posts WHERE id = $1",
        vec![post.into()],
    )
    .await?;
    super::audit(
        &state.db,
        id,
        me,
        "SCHEDULED_POST_CANCELLED",
        None,
        Some(post),
        state.now(),
    )
    .await?;
    Ok(json!({ "postId": post, "deleted": true }))
}

// ---------------------------------------------------------------------- changing posts

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditRequest {
    /// The new words or caption (`""` clears a caption).
    #[serde(default)]
    pub text: Option<String>,
    /// A new device-built link preview; `null` removes it.
    #[serde(default, deserialize_with = "super::double")]
    pub link_preview: Option<Option<LinkPreview>>,
}

/// Edit a post's words, caption or link preview (marked edited).
pub async fn edit_post(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
    req: EditRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    row.require_open()?;
    let current = post_row(&state.db, me, id, post).await?;
    let perms = row.perms(me);
    if !(perms.edit || (perms.post && current.author_principal_id == me)) {
        return Err(forbidden(
            "NOT_CHANNEL_EDITOR",
            "Only an editor or the post's author can do this",
        ));
    }
    if current.kind == "POLL" {
        return Err(Error::reason(ErrorCode::Conflict, "POLL_NOT_EDITABLE"));
    }
    if req.text.is_none() && req.link_preview.is_none() {
        return Err(Error::validation("Nothing to change"));
    }
    let text = match &req.text {
        Some(_) => clean_text(&req.text, MAX_POST, "A post")?,
        None => current.words.clone(),
    };
    if text.is_none() && current.media_id.is_none() {
        return Err(Error::validation("A text post needs its text"));
    }
    let preview = match &req.link_preview {
        Some(lp) => link_preview(lp, text.as_deref())?,
        None => current.link_preview.clone().filter(|lp| {
            lp["url"]
                .as_str()
                .is_some_and(|u| text.as_deref().is_some_and(|t| t.contains(u)))
        }),
    };
    let now = state.now();
    db::exec(
        &state.db,
        "UPDATE messaging_channel_posts SET words = $2, link_preview = $3, edited_at = $4 WHERE id = $1",
        vec![post.into(), text.into(), preview.into(), now.into()],
    )
    .await?;
    if current.author_principal_id != me {
        super::audit(
            &state.db,
            id,
            me,
            "POST_EDITED",
            Some(current.author_principal_id),
            Some(post),
            now,
        )
        .await?;
    }
    post_view(state, me, id, post, true).await
}

pub async fn delete_post(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    let current = post_row(&state.db, me, id, post).await?;
    let perms = row.perms(me);
    if !(perms.edit || (perms.post && current.author_principal_id == me)) {
        return Err(forbidden(
            "NOT_CHANNEL_EDITOR",
            "Only an editor or the post's author can do this",
        ));
    }
    db::exec(
        &state.db,
        "DELETE FROM messaging_channel_posts WHERE id = $1 AND channel_id = $2",
        vec![post.into(), id.into()],
    )
    .await?;
    super::audit(
        &state.db,
        id,
        me,
        "POST_DELETED",
        None,
        Some(post),
        state.now(),
    )
    .await?;
    Ok(json!({ "postId": post, "deleted": true }))
}

/// Pin or unpin a post (at most 3 pinned).
pub async fn set_pinned(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
    pin: bool,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    let perms = row.perms(me);
    if !(perms.edit || perms.post) {
        return Err(forbidden(
            "NOT_CHANNEL_ADMIN",
            "Only the channel's owner and admins can do this",
        ));
    }
    row.require_open()?;
    let current = post_row(&state.db, me, id, post).await?;
    let now = state.now();
    if pin && current.pinned_at.is_none() {
        let pinned = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_channel_posts WHERE channel_id = $1 AND pinned_at IS NOT NULL",
            vec![id.into()],
        )
        .await?;
        if pinned >= MAX_PINNED {
            return Err(Error::Code {
                code: ErrorCode::Conflict,
                reason: Some("TOO_MANY_PINS"),
                message: Some("Unpin a post first (at most 3)".into()),
            });
        }
    }
    db::exec(
        &state.db,
        "UPDATE messaging_channel_posts SET pinned_at = CASE WHEN $2 THEN coalesce(pinned_at, $3) ELSE NULL END WHERE id = $1",
        vec![post.into(), pin.into(), now.into()],
    )
    .await?;
    super::audit(
        &state.db,
        id,
        me,
        if pin { "POST_PINNED" } else { "POST_UNPINNED" },
        None,
        Some(post),
        now,
    )
    .await?;
    post_view(state, me, id, post, true).await
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
    let row = readable(state, ctx, me, id).await?;
    post_row(&state.db, me, id, post).await?;
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
    post_view(state, me, id, post, row.staff(me)).await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoteRequest {
    /// One option for a single-choice poll; one or more distinct ones otherwise.
    pub option_indexes: Vec<i16>,
}

/// The caller's vote (one per follower; a new vote replaces theirs). `None` retracts it.
pub async fn vote(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
    req: Option<VoteRequest>,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = readable(state, ctx, me, id).await?;
    if row.role(me) == super::Role::Visitor {
        return Err(forbidden("FOLLOW_TO_VOTE", "Follow the channel to vote"));
    }
    #[derive(FromQueryResult)]
    struct Poll {
        multiple: bool,
        options: Value,
        closes_at: Option<DateTime<Utc>>,
    }
    let poll: Poll = db::one(
        &state.db,
        "SELECT pl.multiple, pl.options, pl.closes_at FROM messaging_channel_polls pl \
           JOIN messaging_channel_posts p ON p.id = pl.post_id WHERE pl.post_id = $1 AND p.channel_id = $2",
        vec![post.into(), id.into()],
    )
    .await?
    .ok_or_else(not_found)?;
    let now = state.now();
    if poll.closes_at.is_some_and(|c| c <= now) {
        return Err(Error::reason(ErrorCode::Conflict, "POLL_CLOSED"));
    }
    let picks: Vec<i16> = match &req {
        None => vec![],
        Some(v) => {
            let n = poll.options.as_array().map(|a| a.len()).unwrap_or(0) as i16;
            let mut picks = v.option_indexes.clone();
            picks.sort_unstable();
            picks.dedup();
            if picks.len() != v.option_indexes.len()
                || picks.is_empty()
                || picks.iter().any(|i| *i < 0 || *i >= n)
                || (!poll.multiple && picks.len() != 1)
            {
                return Err(Error::validation(if poll.multiple {
                    "Choose one or more of the poll's options"
                } else {
                    "Choose one of the poll's options"
                }));
            }
            picks
        }
    };
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                db::exec(
                    txn,
                    "DELETE FROM messaging_channel_poll_votes WHERE post_id = $1 AND principal_id = $2",
                    vec![post.into(), me.into()],
                )
                .await?;
                for i in picks {
                    db::exec(
                        txn,
                        "INSERT INTO messaging_channel_poll_votes (post_id, principal_id, option_index, voted_at) VALUES ($1, $2, $3, $4)",
                        vec![post.into(), me.into(), i.into(), now.into()],
                    )
                    .await?;
                }
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    post_view(state, me, id, post, row.staff(me)).await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewsRequest {
    pub post_ids: Vec<Uuid>,
}

/// Records that the caller saw these posts (once per person and post; never for the
/// channel's owner and admins).
pub async fn record_views(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ViewsRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    if req.post_ids.is_empty() || req.post_ids.len() > MAX_VIEW_BATCH {
        return Err(Error::validation("Between 1 and 100 posts"));
    }
    let row = readable(state, ctx, me, id).await?;
    if row.staff(me) {
        return Ok(json!({ "recorded": 0 }));
    }
    let n = db::exec(
        &state.db,
        "INSERT INTO messaging_channel_post_views (post_id, principal_id, viewed_at) \
         SELECT p.id, $3, $4 FROM messaging_channel_posts p WHERE p.channel_id = $1 AND p.id = ANY($2) \
         ON CONFLICT DO NOTHING",
        vec![
            id.into(),
            req.post_ids.into(),
            me.into(),
            state.now().into(),
        ],
    )
    .await?;
    Ok(json!({ "recorded": n }))
}

/// Star or unstar a post (the caller's own list).
pub async fn set_starred(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
    star: bool,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = readable(state, ctx, me, id).await?;
    post_row(&state.db, me, id, post).await?;
    if star {
        db::exec(
            &state.db,
            "INSERT INTO messaging_channel_stars (principal_id, post_id, starred_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            vec![me.into(), post.into(), state.now().into()],
        )
        .await?;
    } else {
        db::exec(
            &state.db,
            "DELETE FROM messaging_channel_stars WHERE principal_id = $1 AND post_id = $2",
            vec![me.into(), post.into()],
        )
        .await?;
    }
    post_view(state, me, id, post, row.staff(me)).await
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StarredQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(FromQueryResult)]
struct StarRow {
    post_id: Uuid,
    channel_id: Uuid,
    starred_at: DateTime<Utc>,
}

/// The caller's starred channel posts, newest star first, from channels they can still read.
pub async fn starred(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    q: StarredQuery,
) -> Result<Value> {
    let me = acting.principal_id;
    let page = match q.limit {
        None => 30,
        Some(n) if (1..=100).contains(&n) => n,
        Some(_) => return Err(Error::validation("limit must be between 1 and 100")),
    };
    let cursor = match q.cursor.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(
            crate::operator_reads::decode_cursor(raw)
                .ok_or_else(|| Error::validation("cursor is invalid"))?,
        ),
    };
    let (at, after) = cursor
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    let stars: Vec<StarRow> = db::all(
        &state.db,
        "SELECT s.post_id, p.channel_id, s.starred_at FROM messaging_channel_stars s \
           JOIN messaging_channel_posts p ON p.id = s.post_id \
           JOIN messaging_channels c ON c.id = p.channel_id AND c.environment_id = $2 \
          WHERE s.principal_id = $1 AND ($3::timestamptz IS NULL OR (s.starred_at, s.post_id) < ($3, $4)) \
          ORDER BY s.starred_at DESC, s.post_id DESC LIMIT $5",
        vec![
            me.into(),
            ctx.scope.environment_id.into(),
            at.into(),
            after.into(),
            (page + 1).into(),
        ],
    )
    .await?;
    let more = stars.len() as i64 > page;
    let stars: Vec<&StarRow> = stars.iter().take(page as usize).collect();
    let next = if more {
        stars
            .last()
            .map(|s| crate::operator_reads::encode_cursor(s.starred_at, s.post_id))
    } else {
        None
    };
    let mut channel_ids: Vec<Uuid> = stars.iter().map(|s| s.channel_id).collect();
    channel_ids.sort();
    channel_ids.dedup();
    let channels = super::rows_for(&state.db, ctx, me, &channel_ids).await?;
    let channels: HashMap<Uuid, ChannelRow> = channels
        .into_iter()
        .filter(|c| c.posts_readable(me))
        .map(|c| (c.id, c))
        .collect();
    let post_ids: Vec<Uuid> = stars
        .iter()
        .filter(|s| channels.contains_key(&s.channel_id))
        .map(|s| s.post_id)
        .collect();
    let rows: Vec<PostRow> = db::all(
        &state.db,
        &format!("SELECT {POST_COLUMNS} FROM messaging_channel_posts p WHERE p.id = ANY($2)"),
        vec![me.into(), post_ids.into()],
    )
    .await?;
    let rendered: HashMap<Uuid, Value> = rows
        .iter()
        .map(|r| r.id)
        .zip(post_views(&state.db, me, &rows, false, state.now()).await?)
        .collect();
    let items: Vec<Value> = stars
        .iter()
        .filter_map(|s| {
            let c = channels.get(&s.channel_id)?;
            Some(json!({
                "post": rendered.get(&s.post_id)?,
                "channel": {
                    "channelId": c.id,
                    "name": c.name,
                    "handle": c.handle,
                    "verified": c.verified_at.is_some(),
                },
                "starredAt": rfc3339(s.starred_at),
            }))
        })
        .collect();
    Ok(json!({ "items": items, "nextCursor": next }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardRequest {
    pub principal_ids: Vec<Uuid>,
}

/// Shares a post into the caller's direct chats with mutual contacts (a message with the
/// channel's name, an excerpt and the post's link). Recipients who cannot read the channel
/// see only that message.
pub async fn forward(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    post: Uuid,
    req: ForwardRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    if req.principal_ids.is_empty() || req.principal_ids.len() > MAX_FORWARD_TARGETS {
        return Err(Error::validation("Between 1 and 20 recipients"));
    }
    let row = readable(state, ctx, me, id).await?;
    row.require_open()?;
    let current = post_row(&state.db, me, id, post).await?;
    limit(state, ctx, "forward", me, Window::Hour, 30).await?;
    let mut targets = req.principal_ids.clone();
    targets.sort();
    targets.dedup();
    targets.retain(|t| *t != me);
    let contacts = eligible_contacts(state, ctx, me, Some(&targets)).await?;
    let excerpt: String = current
        .words
        .as_deref()
        .unwrap_or(match current.kind.as_str() {
            "IMAGE" => "Photo",
            "VIDEO" => "Video",
            "AUDIO" => "Audio",
            "VOICE" => "Voice message",
            "DOCUMENT" => "Document",
            "GIF" => "GIF",
            "STICKER" => "Sticker",
            "POLL" => "Poll",
            _ => "Post",
        })
        .chars()
        .take(280)
        .collect();
    let text = format!(
        "{} (channel): {excerpt}\ncfmeet://channel/{id}/post/{post}",
        row.name
    );
    let (mut forwarded, mut skipped) = (0i32, 0i32);
    for t in targets {
        if !contacts.contains_key(&t) {
            skipped += 1;
            continue;
        }
        let conversation = match crate::social::status::direct_with(state, ctx, acting, t).await {
            Ok(c) => c,
            Err(e) if e.error_code() == ErrorCode::ServiceUnavailable => return Err(e),
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        match crate::social::status::send_text(
            state,
            ctx,
            acting,
            conversation,
            Uuid::new_v4(),
            text.clone(),
        )
        .await
        {
            Ok(_) => forwarded += 1,
            Err(_) => skipped += 1,
        }
    }
    if forwarded > 0 {
        db::exec(
            &state.db,
            "INSERT INTO messaging_channel_forwards (id, channel_id, post_id, recipients, occurred_at) VALUES ($1, $2, $3, $4, $5)",
            vec![
                Uuid::new_v4().into(),
                id.into(),
                post.into(),
                forwarded.into(),
                state.now().into(),
            ],
        )
        .await?;
    }
    Ok(json!({ "postId": post, "forwarded": forwarded, "skipped": skipped }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(url: &str) -> Option<LinkPreview> {
        Some(LinkPreview {
            url: url.into(),
            title: Some("Title".into()),
            description: None,
            site_name: None,
        })
    }

    #[test]
    fn link_previews_need_their_link_in_the_text() {
        assert!(link_preview(
            &lp("https://example.com/a"),
            Some("see https://example.com/a")
        )
        .is_ok());
        assert!(link_preview(&lp("https://example.com/a"), Some("no link")).is_err());
        assert!(link_preview(&lp("javascript:alert(1)"), Some("javascript:alert(1)")).is_err());
        assert!(link_preview(&lp("https://a b"), Some("https://a b")).is_err());
        assert_eq!(link_preview(&None, Some("x")).unwrap(), None);
    }

    #[test]
    fn polls_have_two_to_twelve_distinct_options() {
        let p = |opts: &[&str]| PollRequest {
            question: "Which?".into(),
            options: opts.iter().map(|s| s.to_string()).collect(),
            multiple: false,
        };
        assert!(poll_of(&p(&["a", "b"])).is_ok());
        assert!(poll_of(&p(&["a"])).is_err());
        assert!(poll_of(&p(&["a", "A"])).is_err());
        assert!(poll_of(&p(&["a", ""])).is_err());
        let many: Vec<String> = (0..13).map(|i| i.to_string()).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(poll_of(&p(&many)).is_err());
    }
}

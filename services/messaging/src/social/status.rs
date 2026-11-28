//! Statuses: text, photo, video (≤ 60 s shown) or voice note, live for 24 hours, seen only
//! by the audience SNAPSHOT taken when they were published.
//!
//! **The gate** ([`Gate::allows`]) decides every read by someone other than the author:
//!
//! 1. the status is live (`expires_at` in the future);
//! 2. the reader is in its snapshot (`audience_principals`: the author's mutual contacts the
//!    chosen audience selected at publish time; a Messaging block removes the pair from each
//!    other's snapshots for good, [`revoke_pair`]);
//! 3. the reader and the author are STILL mutual, unblocked contacts (Communications Hub)
//!    whose relationship has not (re)started after the snapshot — so removing and re-adding
//!    a contact, or blocking and unblocking, never restores access to earlier statuses;
//! 4. there is no Messaging block between them;
//! 5. a reshare additionally needs the reader to pass 1–4 for the ORIGINAL status (sharing
//!    never widens an audience).
//!
//! Anything else answers `404 NOT_FOUND`, exactly like a status that does not exist.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    audience::{self, AudienceChoice, Snapshot},
    blocked_with, clean_text, eligible_contacts,
    media::{self, MediaKind, MediaRow, Purpose},
    not_found, notices, valid_color, Contacts, Idempotent, DURATION_SLACK_MS, MAX_SEGMENT_MS,
    STATUS_LIFETIME_HOURS,
};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

/// Live statuses one principal may have at once.
pub const MAX_LIVE_STATUSES: i64 = 100;
pub const MAX_TEXT: usize = 700;
pub const MAX_CAPTION: usize = 700;
pub const MAX_MUSIC_TITLE: usize = 120;
pub const MAX_MENTIONS: usize = 20;
/// Statuses deleted in one call.
pub const MAX_BULK_DELETE: usize = 100;
/// Principals of one rings lookup / one forward.
pub const MAX_RING_PRINCIPALS: usize = 200;
pub const MAX_FORWARD_TARGETS: usize = 20;
pub const MAX_REPLY: usize = 4000;
/// Viewers listed per status.
pub const MAX_VIEWERS: i64 = 500;
/// Replies listed per status (newest first).
pub const MAX_REPLIES: i64 = 200;
/// The text the server puts before a reply in the chat ([`reply`]); the author's replies
/// list shows only what the replier wrote.
const REPLY_PREFIX: &str = "Replying to your status (";

#[derive(Debug, Clone, FromQueryResult)]
pub struct StatusRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub author_principal_id: Uuid,
    pub kind: String,
    pub words: Option<String>,
    pub caption: Option<String>,
    pub style: Value,
    pub media_id: Option<Uuid>,
    pub start_ms: Option<i32>,
    pub end_ms: Option<i32>,
    pub music_media_id: Option<Uuid>,
    pub music_start_ms: Option<i32>,
    pub music_end_ms: Option<i32>,
    pub music_title: Option<String>,
    pub group_id: Option<Uuid>,
    pub sequence: Option<i32>,
    pub audience_mode: String,
    pub audience_list_id: Option<Uuid>,
    pub audience_label: Option<String>,
    pub audience_principals: Vec<Uuid>,
    pub excluded_count: i32,
    pub audience_as_of: Option<DateTime<Utc>>,
    pub allow_sharing: bool,
    pub origin_status_id: Option<Uuid>,
    pub origin_author_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub const COLUMNS: &str = "id, tenant_id, application_id, environment_id, author_principal_id, kind, words, caption, \
    style, media_id, start_ms, end_ms, music_media_id, music_start_ms, music_end_ms, music_title, group_id, sequence, \
    audience_mode, audience_list_id, audience_label, audience_principals, excluded_count, audience_as_of, allow_sharing, \
    origin_status_id, origin_author_principal_id, created_at, expires_at";

async fn load<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    id: Uuid,
) -> Result<Option<StatusRow>> {
    Ok(db::one(
        db,
        &format!("SELECT {COLUMNS} FROM messaging_statuses WHERE id = $1 AND environment_id = $2"),
        vec![id.into(), environment_id.into()],
    )
    .await?)
}

async fn load_many<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<StatusRow>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    Ok(db::all(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_statuses WHERE id = ANY($1) AND environment_id = $2"
        ),
        vec![ids.to_vec().into(), environment_id.into()],
    )
    .await?)
}

/// Live statuses using media `id` (as their media or their music).
pub async fn using_media<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<StatusRow>> {
    Ok(db::all(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_statuses WHERE environment_id = $1 AND (media_id = $2 OR music_media_id = $2) \
               AND expires_at > $3 LIMIT 200"
        ),
        vec![environment_id.into(), id.into(), now.into()],
    )
    .await?)
}

// ------------------------------------------------------------------------------ gate

/// Everything the gate needs for one reader: their eligible contacts, their Messaging
/// blocks and the originals of reshares.
pub struct Gate {
    viewer: Uuid,
    now: DateTime<Utc>,
    contacts: Contacts,
    blocked: HashSet<Uuid>,
    origins: HashMap<Uuid, StatusRow>,
}

impl Gate {
    /// Loads the gate for `viewer` over `statuses` (one Communications lookup, restricted
    /// to the authors involved).
    pub async fn load(
        state: &AppState,
        ctx: &TrustedContext,
        viewer: Uuid,
        statuses: &[StatusRow],
    ) -> Result<Self> {
        let origins = origins_of(state, ctx, statuses).await?;
        let mut authors: Vec<Uuid> = statuses
            .iter()
            .map(|s| s.author_principal_id)
            .chain(origins.values().map(|o| o.author_principal_id))
            .filter(|a| *a != viewer)
            .collect();
        authors.sort();
        authors.dedup();
        let contacts = if authors.is_empty() {
            Contacts::new()
        } else {
            eligible_contacts(state, ctx, viewer, Some(&authors)).await?
        };
        Self::with_contacts(state, ctx, viewer, contacts, origins).await
    }

    async fn with_contacts(
        state: &AppState,
        ctx: &TrustedContext,
        viewer: Uuid,
        contacts: Contacts,
        origins: HashMap<Uuid, StatusRow>,
    ) -> Result<Self> {
        Ok(Self {
            viewer,
            now: state.now(),
            blocked: blocked_with(&state.db, ctx.scope.environment_id, viewer).await?,
            contacts,
            origins,
        })
    }

    fn base(&self, s: &StatusRow) -> bool {
        if s.author_principal_id == self.viewer {
            return s.expires_at > self.now;
        }
        s.expires_at > self.now
            && s.audience_principals.contains(&self.viewer)
            && !self.blocked.contains(&s.author_principal_id)
            && match (self.contacts.get(&s.author_principal_id), s.audience_as_of) {
                (Some(since), Some(as_of)) => *since <= as_of,
                _ => false,
            }
    }

    /// Whether the reader may see `s` (see the module documentation).
    pub fn allows(&self, s: &StatusRow) -> bool {
        if s.author_principal_id == self.viewer && s.origin_status_id.is_none() {
            return s.expires_at > self.now;
        }
        if !self.base(s) {
            return false;
        }
        match s.origin_status_id {
            None => true,
            Some(o) => self.origins.get(&o).is_some_and(|o| self.base(o)),
        }
    }
}

async fn origins_of(
    state: &AppState,
    ctx: &TrustedContext,
    statuses: &[StatusRow],
) -> Result<HashMap<Uuid, StatusRow>> {
    let ids: Vec<Uuid> = statuses.iter().filter_map(|s| s.origin_status_id).collect();
    Ok(load_many(&state.db, ctx.scope.environment_id, &ids)
        .await?
        .into_iter()
        .map(|o| (o.id, o))
        .collect())
}

/// A live status `viewer` may see (anything else is 404).
async fn visible(
    state: &AppState,
    ctx: &TrustedContext,
    viewer: Uuid,
    id: Uuid,
) -> Result<StatusRow> {
    let row = load(&state.db, ctx.scope.environment_id, id)
        .await?
        .ok_or_else(not_found)?;
    let gate = Gate::load(state, ctx, viewer, std::slice::from_ref(&row)).await?;
    if !gate.allows(&row) {
        return Err(not_found());
    }
    Ok(row)
}

/// A Messaging block removes each principal from the other's live status snapshots for
/// good (unblocking never restores access to statuses published before).
pub async fn revoke_pair<C: ConnectionTrait>(
    txn: &C,
    environment_id: Uuid,
    a: Uuid,
    b: Uuid,
) -> Result<()> {
    for (author, other) in [(a, b), (b, a)] {
        db::exec(
            txn,
            "UPDATE messaging_statuses SET audience_principals = array_remove(audience_principals, $3) \
              WHERE environment_id = $1 AND author_principal_id = $2 AND $3 = ANY(audience_principals)",
            vec![environment_id.into(), author.into(), other.into()],
        )
        .await?;
        db::exec(
            txn,
            "DELETE FROM messaging_status_mentions m USING messaging_statuses s \
              WHERE m.status_id = s.id AND s.environment_id = $1 AND s.author_principal_id = $2 AND m.principal_id = $3",
            vec![environment_id.into(), author.into(), other.into()],
        )
        .await?;
    }
    Ok(())
}

// ----------------------------------------------------------------------------- views

#[derive(FromQueryResult)]
struct MyView {
    status_id: Uuid,
    liked: bool,
    reaction: Option<String>,
}

#[derive(FromQueryResult)]
struct Mention {
    status_id: Uuid,
    principal_id: Uuid,
}

#[derive(FromQueryResult)]
struct Count {
    status_id: Uuid,
    n: i64,
}

/// Orders statuses for display: a group (`groupId`) stays together at the time its first
/// item was published, in `sequence` order, whatever order its items arrived in.
pub fn display_order(rows: &mut [StatusRow]) {
    let mut anchor: HashMap<(Uuid, Uuid), DateTime<Utc>> = HashMap::new();
    for r in rows.iter() {
        let key = (r.author_principal_id, r.group_id.unwrap_or(r.id));
        anchor
            .entry(key)
            .and_modify(|t| *t = (*t).min(r.created_at))
            .or_insert(r.created_at);
    }
    rows.sort_by(|a, b| {
        let ka = (a.author_principal_id, a.group_id.unwrap_or(a.id));
        let kb = (b.author_principal_id, b.group_id.unwrap_or(b.id));
        anchor[&ka]
            .cmp(&anchor[&kb])
            .then(ka.1.cmp(&kb.1))
            .then(a.sequence.unwrap_or(0).cmp(&b.sequence.unwrap_or(0)))
            .then(a.created_at.cmp(&b.created_at))
    });
}

const STYLE_KEYS: &[&str] = &[
    "textColor",
    "textAlign",
    "textSize",
    "background",
    "backgroundGradient",
    "font",
];

/// Renders `rows` as `viewer` sees them (the author gets the owner fields).
async fn render(state: &AppState, viewer: Uuid, rows: &[StatusRow]) -> Result<Vec<Value>> {
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let media_ids: Vec<Uuid> = rows
        .iter()
        .flat_map(|r| [r.media_id, r.music_media_id])
        .flatten()
        .collect();
    let media: HashMap<Uuid, MediaRow> = if media_ids.is_empty() {
        HashMap::new()
    } else {
        db::all::<MediaRow, _>(
            &state.db,
            "SELECT id, tenant_id, application_id, environment_id, owner_principal_id, purpose, kind, media_type, \
               size_bytes, part_bytes, part_count, duration_ms, state, created_at, completed_at \
               FROM messaging_social_media WHERE id = ANY($1)",
            vec![media_ids.into()],
        )
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect()
    };
    let mine: HashMap<Uuid, MyView> = db::all::<MyView, _>(
        &state.db,
        "SELECT status_id, liked, reaction FROM messaging_status_views WHERE viewer_principal_id = $1 AND status_id = ANY($2)",
        vec![viewer.into(), ids.clone().into()],
    )
    .await?
    .into_iter()
    .map(|v| (v.status_id, v))
    .collect();
    let mut mentions: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for m in db::all::<Mention, _>(
        &state.db,
        "SELECT status_id, principal_id FROM messaging_status_mentions WHERE status_id = ANY($1) ORDER BY principal_id",
        vec![ids.clone().into()],
    )
    .await?
    {
        mentions.entry(m.status_id).or_default().push(m.principal_id);
    }
    let owned: Vec<Uuid> = rows
        .iter()
        .filter(|r| r.author_principal_id == viewer)
        .map(|r| r.id)
        .collect();
    type Counts = HashMap<Uuid, i64>;
    let (views, shares, replies): (Counts, Counts, Counts) = if owned.is_empty() {
        (HashMap::new(), HashMap::new(), HashMap::new())
    } else {
        let views = db::all::<Count, _>(
            &state.db,
            "SELECT v.status_id, count(*) AS n FROM messaging_status_views v JOIN messaging_statuses s ON s.id = v.status_id \
              WHERE v.status_id = ANY($1) AND v.viewer_principal_id <> s.author_principal_id \
                AND v.viewer_principal_id = ANY(s.audience_principals) GROUP BY v.status_id",
            vec![owned.clone().into()],
        )
        .await?
        .into_iter()
        .map(|c| (c.status_id, c.n))
        .collect();
        let shares = db::all::<Count, _>(
            &state.db,
            "SELECT status_id, count(*) AS n FROM messaging_status_shares WHERE status_id = ANY($1) GROUP BY status_id",
            vec![owned.clone().into()],
        )
        .await?
        .into_iter()
        .map(|c| (c.status_id, c.n))
        .collect();
        // ADR 0073: the private replies recorded for the author (an upper bound: a reply
        // whose message was since deleted is counted until the list reads it).
        let replies = db::all::<Count, _>(
            &state.db,
            "SELECT status_id, count(*) AS n FROM messaging_status_replies WHERE status_id = ANY($1) AND author_principal_id = $2 \
               GROUP BY status_id",
            vec![owned.into(), viewer.into()],
        )
        .await?
        .into_iter()
        .map(|c| (c.status_id, c.n))
        .collect();
        (views, shares, replies)
    };
    let now = state.now();
    Ok(rows
        .iter()
        .map(|r| {
            let own = r.author_principal_id == viewer;
            let me = mine.get(&r.id);
            let mut v = json!({
                "statusId": r.id,
                "authorPrincipalId": r.author_principal_id,
                "kind": r.kind,
                "text": r.words,
                "caption": r.caption,
                "media": r.media_id.and_then(|m| media.get(&m)).map(|m| media::view(m, None)),
                "startMs": r.start_ms,
                "endMs": r.end_ms,
                "music": r.music_media_id.map(|m| json!({
                    "mediaId": m,
                    "media": media.get(&m).map(|x| media::view(x, None)),
                    "startMs": r.music_start_ms,
                    "endMs": r.music_end_ms,
                    "title": r.music_title,
                })),
                "musicTitle": r.music_title,
                "groupId": r.group_id,
                "sequence": r.sequence,
                "createdAt": rfc3339(r.created_at),
                "expiresAt": rfc3339(r.expires_at),
                "remainingSeconds": (r.expires_at - now).num_seconds().max(0),
                "viewed": own || me.is_some(),
                "liked": me.is_some_and(|m| m.liked),
                "reaction": me.and_then(|m| m.reaction.clone()),
                "mentionedMe": !own && mentions.get(&r.id).is_some_and(|m| m.contains(&viewer)),
                "closeFriends": r.audience_mode == "CLOSE_FRIENDS",
                "allowSharing": r.allow_sharing,
                "origin": match (r.origin_status_id, r.origin_author_principal_id) {
                    (Some(s), Some(a)) => json!({ "statusId": s, "authorPrincipalId": a }),
                    _ => Value::Null,
                },
            });
            for key in STYLE_KEYS {
                v[*key] = r.style.get(*key).cloned().unwrap_or(Value::Null);
            }
            if own {
                v["viewCount"] = json!(views.get(&r.id).copied().unwrap_or(0));
                v["shareCount"] = json!(shares.get(&r.id).copied().unwrap_or(0));
                v["replyCount"] = json!(replies.get(&r.id).copied().unwrap_or(0));
                v["mentions"] = json!(mentions.get(&r.id).cloned().unwrap_or_default());
                v["audience"] = json!({
                    "mode": r.audience_mode,
                    "label": r.audience_label,
                    "listId": r.audience_list_id,
                    "includedCount": r.audience_principals.len(),
                    "excludedCount": r.excluded_count,
                });
            }
            v
        })
        .collect())
}

async fn render_one(state: &AppState, viewer: Uuid, row: &StatusRow) -> Result<Value> {
    render(state, viewer, std::slice::from_ref(row))
        .await?
        .into_iter()
        .next()
        .ok_or(Error::code(ErrorCode::InternalError))
}

// ---------------------------------------------------------------------------- create

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum StatusKind {
    Text,
    Image,
    Video,
    Audio,
}

impl StatusKind {
    fn as_str(self) -> &'static str {
        match self {
            StatusKind::Text => "TEXT",
            StatusKind::Image => "IMAGE",
            StatusKind::Video => "VIDEO",
            StatusKind::Audio => "AUDIO",
        }
    }

    fn media(self) -> Option<MediaKind> {
        match self {
            StatusKind::Text => None,
            StatusKind::Image => Some(MediaKind::Image),
            StatusKind::Video => Some(MediaKind::Video),
            StatusKind::Audio => Some(MediaKind::Audio),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gradient {
    pub from: String,
    pub to: String,
    pub angle: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateStatus {
    pub kind: StatusKind,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub text_color: Option<String>,
    #[serde(default)]
    pub text_align: Option<String>,
    #[serde(default)]
    pub text_size: Option<i32>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub background_gradient: Option<Gradient>,
    #[serde(default)]
    pub font: Option<i32>,
    #[serde(default)]
    pub media_id: Option<Uuid>,
    #[serde(default)]
    pub start_ms: Option<i32>,
    #[serde(default)]
    pub end_ms: Option<i32>,
    #[serde(default)]
    pub music_media_id: Option<Uuid>,
    #[serde(default)]
    pub music_start_ms: Option<i32>,
    #[serde(default)]
    pub music_end_ms: Option<i32>,
    #[serde(default)]
    pub music_title: Option<String>,
    #[serde(default)]
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub sequence: Option<i32>,
    #[serde(default)]
    pub mentions: Vec<Uuid>,
    #[serde(default)]
    pub audience: Option<AudienceChoice>,
    #[serde(default)]
    pub allow_sharing: Option<bool>,
}

/// Validated content of a new status.
struct Content {
    kind: StatusKind,
    text: Option<String>,
    caption: Option<String>,
    style: Value,
    media: Option<MediaRow>,
    start_ms: Option<i32>,
    end_ms: Option<i32>,
    music: Option<MediaRow>,
    music_start_ms: Option<i32>,
    music_end_ms: Option<i32>,
    music_title: Option<String>,
}

fn segment(start: Option<i32>, end: Option<i32>, what: &str) -> Result<Option<(i32, i32)>> {
    match (start, end) {
        (None, None) => Ok(None),
        (Some(s), Some(e)) if s >= 0 && e > s && e - s <= MAX_SEGMENT_MS => Ok(Some((s, e))),
        (Some(_), Some(_)) => Err(Error::validation(format!(
            "{what} must be at most 60 seconds (startMs < endMs)"
        ))),
        _ => Err(Error::validation(format!(
            "{what} needs both startMs and endMs"
        ))),
    }
}

fn style(req: &CreateStatus) -> Result<Value> {
    let mut s = serde_json::Map::new();
    let text_style = req.text_color.is_some()
        || req.text_align.is_some()
        || req.text_size.is_some()
        || req.font.is_some();
    match req.kind {
        StatusKind::Text => {}
        StatusKind::Audio if !text_style => {}
        _ if !text_style && req.background.is_none() && req.background_gradient.is_none() => {}
        _ => {
            return Err(Error::validation(
                "Text styles apply to text statuses; a voice status may carry a background only",
            ))
        }
    }
    if let Some(c) = &req.text_color {
        if !valid_color(c) {
            return Err(Error::validation("textColor must be #rrggbb"));
        }
        s.insert("textColor".into(), json!(c.to_ascii_lowercase()));
    }
    if let Some(a) = &req.text_align {
        if !matches!(a.as_str(), "left" | "center" | "right") {
            return Err(Error::validation("textAlign must be left, center or right"));
        }
        s.insert("textAlign".into(), json!(a));
    }
    if let Some(n) = req.text_size {
        if !(0..=4).contains(&n) {
            return Err(Error::validation("textSize must be 0 to 4"));
        }
        s.insert("textSize".into(), json!(n));
    }
    if let Some(f) = req.font {
        if !(0..=15).contains(&f) {
            return Err(Error::validation("font must be 0 to 15"));
        }
        s.insert("font".into(), json!(f));
    }
    match (&req.background, &req.background_gradient) {
        (Some(_), Some(_)) => {
            return Err(Error::validation(
                "Give background or backgroundGradient, not both",
            ))
        }
        (Some(b), None) => {
            if !valid_color(b) {
                return Err(Error::validation("background must be #rrggbb"));
            }
            s.insert("background".into(), json!(b.to_ascii_lowercase()));
        }
        (None, Some(g)) => {
            if !valid_color(&g.from) || !valid_color(&g.to) || !(0..=360).contains(&g.angle) {
                return Err(Error::validation(
                    "backgroundGradient needs from / to as #rrggbb and angle 0 to 360",
                ));
            }
            s.insert(
                "backgroundGradient".into(),
                json!({ "from": g.from.to_ascii_lowercase(), "to": g.to.to_ascii_lowercase(), "angle": g.angle }),
            );
        }
        (None, None) => {}
    }
    Ok(Value::Object(s))
}

async fn content(
    state: &AppState,
    ctx: &TrustedContext,
    author: Uuid,
    req: &CreateStatus,
) -> Result<Content> {
    let caption = clean_text(&req.caption, MAX_CAPTION, "A caption")?;
    let style = style(req)?;
    let music_title = clean_text(&req.music_title, MAX_MUSIC_TITLE, "A music title")?;
    let (text, media, start, end) = match req.kind.media() {
        None => {
            if req.media_id.is_some() {
                return Err(Error::validation("A text status takes no mediaId"));
            }
            let text = clean_text(&req.text, MAX_TEXT, "A status")?
                .ok_or_else(|| Error::validation("Write something for your status"))?;
            (Some(text), None, None, None)
        }
        Some(kind) => {
            if req.text.is_some() {
                return Err(Error::validation(
                    "A photo, video or voice status carries a caption, not text",
                ));
            }
            let id = req
                .media_id
                .ok_or_else(|| Error::validation("mediaId is required"))?;
            let m = media::usable(&state.db, ctx, author, Purpose::Status, id).await?;
            if m.media_kind() != Some(kind) {
                return Err(Error::validation("The media is not of this status's kind"));
            }
            let seg = segment(req.start_ms, req.end_ms, "A video segment")?;
            let (start, end) = match kind {
                MediaKind::Video => match (seg, m.duration_ms) {
                    (Some((s, e)), d) => {
                        if d.is_some_and(|d| s >= d) {
                            return Err(Error::validation(
                                "The segment starts after the video ends",
                            ));
                        }
                        (Some(s), Some(e))
                    }
                    (None, Some(d)) if d <= MAX_SEGMENT_MS + DURATION_SLACK_MS => (None, None),
                    (None, _) => {
                        return Err(Error::validation(
                            "A video status is at most 60 seconds: give startMs and endMs",
                        ))
                    }
                },
                MediaKind::Audio => {
                    if seg.is_some() {
                        return Err(Error::validation("A voice status takes no segment"));
                    }
                    if m.duration_ms
                        .is_none_or(|d| d > MAX_SEGMENT_MS + DURATION_SLACK_MS)
                    {
                        return Err(Error::validation("A voice status is at most 60 seconds"));
                    }
                    (None, None)
                }
                MediaKind::Image => {
                    if seg.is_some() {
                        return Err(Error::validation("A photo status takes no segment"));
                    }
                    (None, None)
                }
            };
            (None, Some(m), start, end)
        }
    };
    let music_allowed = matches!(req.kind, StatusKind::Text | StatusKind::Image);
    let (music, music_start, music_end) = match req.music_media_id {
        None => {
            if req.music_start_ms.is_some() || req.music_end_ms.is_some() {
                return Err(Error::validation(
                    "musicStartMs / musicEndMs need musicMediaId",
                ));
            }
            (None, None, None)
        }
        Some(_) if !music_allowed => {
            return Err(Error::validation("Music goes with text and photo statuses"))
        }
        Some(id) => {
            let m = media::usable(&state.db, ctx, author, Purpose::Status, id).await?;
            if m.media_kind() != Some(MediaKind::Audio) {
                return Err(Error::validation("musicMediaId must be audio"));
            }
            match segment(req.music_start_ms, req.music_end_ms, "The music clip")? {
                Some((s, e)) => {
                    if m.duration_ms.is_some_and(|d| s >= d) {
                        return Err(Error::validation(
                            "The music clip starts after the music ends",
                        ));
                    }
                    (Some(m), Some(s), Some(e))
                }
                None if m
                    .duration_ms
                    .is_some_and(|d| d <= MAX_SEGMENT_MS + DURATION_SLACK_MS) =>
                {
                    (Some(m), None, None)
                }
                None => {
                    return Err(Error::validation(
                        "Music plays for at most 60 seconds: give musicStartMs and musicEndMs",
                    ))
                }
            }
        }
    };
    if music_title.is_some() && music.is_none() && req.kind != StatusKind::Audio {
        return Err(Error::validation(
            "musicTitle goes with music or a voice status",
        ));
    }
    Ok(Content {
        kind: req.kind,
        text,
        caption,
        style,
        media,
        start_ms: start,
        end_ms: end,
        music,
        music_start_ms: music_start,
        music_end_ms: music_end,
        music_title,
    })
}

/// The snapshot of an earlier live status of the same group by the same author: every item
/// of a split video / multi-photo publish shares one audience.
async fn group_snapshot<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    author: Uuid,
    group: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<(Snapshot, bool)>> {
    let row: Option<StatusRow> = db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_statuses WHERE environment_id = $1 AND author_principal_id = $2 \
               AND group_id = $3 AND expires_at > $4 ORDER BY created_at LIMIT 1"
        ),
        vec![ctx.scope.environment_id.into(), author.into(), group.into(), now.into()],
    )
    .await?;
    Ok(row.map(|r| {
        (
            Snapshot {
                mode: r.audience_mode,
                list_id: r.audience_list_id,
                label: r.audience_label,
                principals: r.audience_principals,
                excluded: r.excluded_count,
                as_of: r.audience_as_of,
            },
            r.allow_sharing,
        )
    }))
}

struct NewStatus<'a> {
    id: Uuid,
    content: &'a Content,
    snapshot: &'a Snapshot,
    allow_sharing: bool,
    group_id: Option<Uuid>,
    sequence: Option<i32>,
    origin: Option<(Uuid, Uuid)>,
    now: DateTime<Utc>,
}

async fn insert<C: ConnectionTrait>(
    txn: &C,
    ctx: &TrustedContext,
    author: Uuid,
    s: &NewStatus<'_>,
) -> Result<()> {
    let c = s.content;
    db::exec(
        txn,
        "INSERT INTO messaging_statuses (id, tenant_id, application_id, environment_id, author_principal_id, kind, words, \
           caption, style, media_id, start_ms, end_ms, music_media_id, music_start_ms, music_end_ms, music_title, group_id, \
           sequence, audience_mode, audience_list_id, audience_label, audience_principals, excluded_count, audience_as_of, \
           allow_sharing, origin_status_id, origin_author_principal_id, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, \
           $24, $25, $26, $27, $28, $29)",
        vec![
            s.id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            author.into(),
            c.kind.as_str().into(),
            c.text.clone().into(),
            c.caption.clone().into(),
            c.style.clone().into(),
            c.media.as_ref().map(|m| m.id).into(),
            c.start_ms.into(),
            c.end_ms.into(),
            c.music.as_ref().map(|m| m.id).into(),
            c.music_start_ms.into(),
            c.music_end_ms.into(),
            c.music_title.clone().into(),
            s.group_id.into(),
            s.sequence.into(),
            s.snapshot.mode.clone().into(),
            s.snapshot.list_id.into(),
            s.snapshot.label.clone().into(),
            s.snapshot.principals.clone().into(),
            s.snapshot.excluded.into(),
            s.snapshot.as_of.into(),
            s.allow_sharing.into(),
            s.origin.map(|o| o.0).into(),
            s.origin.map(|o| o.1).into(),
            s.now.into(),
            (s.now + Duration::hours(STATUS_LIFETIME_HOURS)).into(),
        ],
    )
    .await?;
    Ok(())
}

async fn live_count(state: &AppState, ctx: &TrustedContext, author: Uuid) -> Result<()> {
    let live = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_statuses WHERE environment_id = $1 AND author_principal_id = $2 AND expires_at > $3",
        vec![ctx.scope.environment_id.into(), author.into(), state.now().into()],
    )
    .await?;
    if live >= MAX_LIVE_STATUSES {
        return Err(Error::reason(
            ErrorCode::QuotaExceeded,
            "TOO_MANY_LIVE_STATUSES",
        ));
    }
    Ok(())
}

/// An idempotent replay: the status as it is now (or the stored answer once it is gone).
async fn replay(
    state: &AppState,
    ctx: &TrustedContext,
    author: Uuid,
    stored: Value,
) -> Result<(u16, Value)> {
    let id = stored["statusId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok());
    if let Some(id) = id {
        if let Some(row) = load(&state.db, ctx.scope.environment_id, id).await? {
            return Ok((200, render_one(state, author, &row).await?));
        }
    }
    Ok((200, stored))
}

/// Publish a status (`201`; an idempotent replay `200`).
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: CreateStatus,
    key: Option<String>,
) -> Result<(u16, Value)> {
    let author = acting.principal_id;
    let idem = key.map(|k| {
        Idempotent::new(
            "status.create",
            k,
            author,
            &serde_json::to_value(&req).unwrap_or(Value::Null),
        )
    });
    if let Some(idem) = &idem {
        if let Some(stored) =
            super::replayed(&state.db, ctx.scope.environment_id, idem, state.now()).await?
        {
            return replay(state, ctx, author, stored).await;
        }
    }
    if req.group_id.is_some() != req.sequence.is_some() {
        return Err(Error::validation("groupId and sequence go together"));
    }
    if req.sequence.is_some_and(|s| !(0..=999).contains(&s)) {
        return Err(Error::validation("sequence must be 0 to 999"));
    }
    if req.mentions.len() > MAX_MENTIONS {
        return Err(Error::validation("At most 20 mentions"));
    }
    live_count(state, ctx, author).await?;
    let content = content(state, ctx, author, &req).await?;
    let now = state.now();
    let (snapshot, default_sharing) = match req.group_id {
        Some(g) => match group_snapshot(&state.db, ctx, author, g, now).await? {
            Some(s) => s,
            None => resolve_audience(state, ctx, author, req.audience.clone()).await?,
        },
        None => resolve_audience(state, ctx, author, req.audience.clone()).await?,
    };
    let allow_sharing = req.allow_sharing.unwrap_or(default_sharing);
    publish(
        state,
        ctx,
        author,
        content,
        snapshot,
        allow_sharing,
        req.group_id,
        req.sequence,
        None,
        &req.mentions,
        idem,
    )
    .await
}

async fn resolve_audience(
    state: &AppState,
    ctx: &TrustedContext,
    author: Uuid,
    choice: Option<AudienceChoice>,
) -> Result<(Snapshot, bool)> {
    let (preferred, sharing) =
        audience::preference(&state.db, ctx.scope.environment_id, author).await?;
    let snapshot = audience::resolve(state, ctx, author, &choice.unwrap_or(preferred)).await?;
    Ok((snapshot, sharing))
}

#[allow(clippy::too_many_arguments)]
async fn publish(
    state: &AppState,
    ctx: &TrustedContext,
    author: Uuid,
    content: Content,
    snapshot: Snapshot,
    allow_sharing: bool,
    group_id: Option<Uuid>,
    sequence: Option<i32>,
    origin: Option<(Uuid, Uuid)>,
    mentions: &[Uuid],
    idem: Option<Idempotent>,
) -> Result<(u16, Value)> {
    // Mentions never widen the audience: only principals inside the snapshot are kept.
    let mut mentioned: Vec<Uuid> = mentions
        .iter()
        .copied()
        .filter(|p| *p != author && snapshot.principals.contains(p))
        .collect();
    mentioned.sort();
    mentioned.dedup();
    let id = Uuid::new_v4();
    let now = state.now();
    let st = state.clone();
    let ctx2 = ctx.clone();
    let env = ctx.scope.environment_id;
    let outcome = state
        .db
        .transaction::<_, Option<Value>, Error>(move |txn| {
            Box::pin(async move {
                if let Some(idem) = &idem {
                    if let Some(stored) = super::claim(txn, env, idem, now).await? {
                        return Ok(Some(stored));
                    }
                }
                insert(
                    txn,
                    &ctx2,
                    author,
                    &NewStatus {
                        id,
                        content: &content,
                        snapshot: &snapshot,
                        allow_sharing,
                        group_id,
                        sequence,
                        origin,
                        now,
                    },
                )
                .await?;
                for p in &mentioned {
                    db::exec(
                        txn,
                        "INSERT INTO messaging_status_mentions (status_id, principal_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                        vec![id.into(), (*p).into()],
                    )
                    .await?;
                    notices::enqueue(&st, txn, &ctx2, notices::Kind::Mention, id, id, *p, now).await?;
                }
                if let Some((origin_id, origin_author)) = origin {
                    let share = Uuid::new_v4();
                    db::exec(
                        txn,
                        "INSERT INTO messaging_status_shares (id, status_id, sharer_principal_id, kind, reshare_status_id, created_at) \
                         VALUES ($1, $2, $3, 'RESHARE', $4, $5)",
                        vec![share.into(), origin_id.into(), author.into(), id.into(), now.into()],
                    )
                    .await?;
                    notices::enqueue(&st, txn, &ctx2, notices::Kind::Share, share, origin_id, origin_author, now)
                        .await?;
                }
                if let Some(idem) = &idem {
                    super::remember(txn, env, idem, &json!({ "statusId": id })).await?;
                }
                Ok(None)
            })
        })
        .await
        .map_err(Error::from)?;
    if let Some(stored) = outcome {
        return replay(state, ctx, author, stored).await;
    }
    crate::metrics::inc("messaging_statuses_published_total", &[]);
    let row = load(&state.db, env, id)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok((201, render_one(state, author, &row).await?))
}

// ----------------------------------------------------------------------------- reads

/// One status the caller may see.
pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let row = visible(state, ctx, acting.principal_id, id).await?;
    render_one(state, acting.principal_id, &row).await
}

async fn live_of<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    authors: &[Uuid],
    viewer: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<StatusRow>> {
    if authors.is_empty() {
        return Ok(vec![]);
    }
    Ok(db::all(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_statuses WHERE environment_id = $1 AND author_principal_id = ANY($2) \
               AND expires_at > $3 AND (author_principal_id = $4 OR $4 = ANY(audience_principals)) \
             ORDER BY created_at LIMIT 5000"
        ),
        vec![environment_id.into(), authors.to_vec().into(), now.into(), viewer.into()],
    )
    .await?)
}

/// The Updates screen: the caller's own live statuses, then those of contacts whose
/// statuses the caller may see (unseen first, then most recent), muted authors apart.
pub async fn feed(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let me = acting.principal_id;
    let now = state.now();
    let env = ctx.scope.environment_id;
    let mut mine = live_of(&state.db, env, &[me], me, now).await?;
    display_order(&mut mine);
    let (contacts, available) = match eligible_contacts(state, ctx, me, None).await {
        Ok(c) => (c, true),
        Err(e) if e.error_code() == ErrorCode::ServiceUnavailable => {
            tracing::warn!(
                "status feed: contacts unavailable, only the caller's own statuses are listed"
            );
            (Contacts::new(), false)
        }
        Err(e) => return Err(e),
    };
    let authors: Vec<Uuid> = contacts.keys().copied().collect();
    let candidates = live_of(&state.db, env, &authors, me, now).await?;
    let origins = origins_of(state, ctx, &candidates).await?;
    let gate = Gate::with_contacts(state, ctx, me, contacts, origins).await?;
    let mut visible: Vec<StatusRow> = candidates.into_iter().filter(|s| gate.allows(s)).collect();
    display_order(&mut visible);
    let muted = audience::muted_set(&state.db, env, me).await?;
    let rendered = render(state, me, &visible).await?;
    let mut by_author: Vec<(Uuid, Vec<Value>, DateTime<Utc>, bool)> = Vec::new();
    for (row, view) in visible.iter().zip(rendered) {
        match by_author
            .iter_mut()
            .find(|a| a.0 == row.author_principal_id)
        {
            Some(a) => {
                a.2 = a.2.max(row.created_at);
                a.3 |= row.audience_mode == "CLOSE_FRIENDS";
                a.1.push(view);
            }
            None => by_author.push((
                row.author_principal_id,
                vec![view],
                row.created_at,
                row.audience_mode == "CLOSE_FRIENDS",
            )),
        }
    }
    let group = |(author, posts, last, close): (Uuid, Vec<Value>, DateTime<Utc>, bool)| {
        let all_viewed = posts.iter().all(|p| p["viewed"] == true);
        (
            all_viewed,
            last,
            json!({
                "authorPrincipalId": author,
                "posts": posts,
                "allViewed": all_viewed,
                "lastPostedAt": rfc3339(last),
                "closeFriends": close,
            }),
        )
    };
    let (mut shown, mut quiet): (Vec<_>, Vec<_>) =
        by_author.into_iter().partition(|a| !muted.contains(&a.0));
    shown.sort_by(|a, b| {
        let av = a.1.iter().all(|p| p["viewed"] == true);
        let bv = b.1.iter().all(|p| p["viewed"] == true);
        av.cmp(&bv).then(b.2.cmp(&a.2))
    });
    quiet.sort_by_key(|a| std::cmp::Reverse(a.2));
    Ok(json!({
        "mine": render(state, me, &mine).await?,
        "contacts": shown.into_iter().map(group).map(|g| g.2).collect::<Vec<_>>(),
        "muted": quiet.into_iter().map(group).map(|g| g.2).collect::<Vec<_>>(),
        "contactsAvailable": available,
    }))
}

/// Status rings for principals the caller is looking at: only those whose statuses the
/// caller may see appear (anyone else is indistinguishable from "no status").
pub async fn rings(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    principals: &[Uuid],
) -> Result<Value> {
    if principals.len() > MAX_RING_PRINCIPALS {
        return Err(Error::validation("At most 200 principals"));
    }
    let me = acting.principal_id;
    let mut wanted: Vec<Uuid> = principals.to_vec();
    wanted.sort();
    wanted.dedup();
    let now = state.now();
    let others: Vec<Uuid> = wanted.iter().copied().filter(|p| *p != me).collect();
    let contacts = eligible_contacts(state, ctx, me, Some(&others)).await?;
    let mut authors: Vec<Uuid> = contacts.keys().copied().collect();
    if wanted.contains(&me) {
        authors.push(me);
    }
    let candidates = live_of(&state.db, ctx.scope.environment_id, &authors, me, now).await?;
    let origins = origins_of(state, ctx, &candidates).await?;
    let gate = Gate::with_contacts(state, ctx, me, contacts, origins).await?;
    let visible: Vec<StatusRow> = candidates.into_iter().filter(|s| gate.allows(s)).collect();
    let ids: Vec<Uuid> = visible.iter().map(|s| s.id).collect();
    let seen: HashSet<Uuid> = db::all::<MyView, _>(
        &state.db,
        "SELECT status_id, liked, reaction FROM messaging_status_views WHERE viewer_principal_id = $1 AND status_id = ANY($2)",
        vec![me.into(), ids.clone().into()],
    )
    .await?
    .into_iter()
    .map(|v| v.status_id)
    .collect();
    // ADR 0073: the visible statuses that mention the caller (the chat list's "mentioned
    // you in a status"); only statuses the gate already allowed are considered.
    let mentioning: HashSet<Uuid> = db::all::<Mention, _>(
        &state.db,
        "SELECT status_id, principal_id FROM messaging_status_mentions WHERE principal_id = $1 AND status_id = ANY($2)",
        vec![me.into(), ids.into()],
    )
    .await?
    .into_iter()
    .map(|m| m.status_id)
    .collect();
    let mut rings: Vec<Value> = Vec::new();
    for p in wanted {
        let of: Vec<&StatusRow> = visible
            .iter()
            .filter(|s| s.author_principal_id == p)
            .collect();
        if of.is_empty() {
            continue;
        }
        let unseen = if p == me {
            0
        } else {
            of.iter().filter(|s| !seen.contains(&s.id)).count()
        };
        rings.push(json!({
            "principalId": p,
            "hasActiveStatus": true,
            "count": of.len(),
            "unseenCount": unseen,
            "closeFriends": p != me && of.iter().any(|s| s.audience_mode == "CLOSE_FRIENDS"),
            "mentionedMe": p != me && of.iter().any(|s| mentioning.contains(&s.id)),
            "mentionedMeUnseen": p != me
                && of
                    .iter()
                    .any(|s| mentioning.contains(&s.id) && !seen.contains(&s.id)),
        }));
    }
    Ok(json!({ "rings": rings }))
}

// ------------------------------------------------------------------- view / reaction

/// Mark seen (and like / unlike). One row per person, whatever device.
pub async fn view(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    liked: Option<bool>,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = visible(state, ctx, me, id).await?;
    if row.author_principal_id != me {
        db::exec(
            &state.db,
            "INSERT INTO messaging_status_views (status_id, viewer_principal_id, viewed_at, liked) VALUES ($1, $2, $3, coalesce($4, false)) \
             ON CONFLICT (status_id, viewer_principal_id) DO UPDATE SET liked = coalesce($4, messaging_status_views.liked)",
            vec![id.into(), me.into(), state.now().into(), liked.into()],
        )
        .await?;
    }
    render_one(state, me, &row).await
}

/// React with one emoji (`None` removes the reaction). Reacting also marks it seen.
pub async fn react(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    emoji: Option<String>,
) -> Result<Value> {
    let me = acting.principal_id;
    if let Some(e) = &emoji {
        if !crate::engagement::valid_reaction(e) {
            return Err(Error::validation(
                "A reaction is one emoji or a :short_code:",
            ));
        }
    }
    let row = visible(state, ctx, me, id).await?;
    if row.author_principal_id == me {
        return Err(Error::reason(ErrorCode::Conflict, "OWN_STATUS"));
    }
    db::exec(
        &state.db,
        "INSERT INTO messaging_status_views (status_id, viewer_principal_id, viewed_at, reaction, reacted_at) \
         VALUES ($1, $2, $3, $4, CASE WHEN $4::varchar IS NULL THEN NULL ELSE $3 END) \
         ON CONFLICT (status_id, viewer_principal_id) DO UPDATE SET reaction = $4, \
           reacted_at = CASE WHEN $4::varchar IS NULL THEN NULL ELSE $3 END",
        vec![id.into(), me.into(), state.now().into(), emoji.into()],
    )
    .await?;
    render_one(state, me, &row).await
}

#[derive(FromQueryResult)]
struct ViewerRow {
    status_id: Uuid,
    viewer_principal_id: Uuid,
    viewed_at: DateTime<Utc>,
    liked: bool,
    reaction: Option<String>,
    reacted_at: Option<DateTime<Utc>>,
}

async fn shares_receipts<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    principals: &[Uuid],
) -> Result<HashSet<Uuid>> {
    #[derive(FromQueryResult)]
    struct Off {
        principal_id: Uuid,
    }
    let off: HashSet<Uuid> = db::all::<Off, _>(
        db,
        "SELECT principal_id FROM messaging_presence_settings WHERE environment_id = $1 AND principal_id = ANY($2) \
           AND NOT share_read_receipts",
        vec![environment_id.into(), principals.to_vec().into()],
    )
    .await?
    .into_iter()
    .map(|o| o.principal_id)
    .collect();
    Ok(principals
        .iter()
        .copied()
        .filter(|p| !off.contains(p))
        .collect())
}

/// The viewers of the owner's `rows` (still in the snapshot and still eligible; a person
/// with read receipts off neither appears nor, as owner, sees anyone — like WhatsApp).
async fn viewers_of(
    state: &AppState,
    ctx: &TrustedContext,
    owner: Uuid,
    rows: &[StatusRow],
) -> Result<(bool, HashMap<Uuid, Vec<Value>>)> {
    let env = ctx.scope.environment_id;
    if shares_receipts(&state.db, env, &[owner]).await?.is_empty() {
        return Ok((false, HashMap::new()));
    }
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    if ids.is_empty() {
        return Ok((true, HashMap::new()));
    }
    let views: Vec<ViewerRow> = db::all(
        &state.db,
        "SELECT status_id, viewer_principal_id, viewed_at, liked, reaction, reacted_at FROM messaging_status_views \
          WHERE status_id = ANY($1) AND viewer_principal_id <> $2 ORDER BY viewed_at DESC LIMIT $3",
        vec![ids.into(), owner.into(), (MAX_VIEWERS * rows.len() as i64).into()],
    )
    .await?;
    let mut people: Vec<Uuid> = views.iter().map(|v| v.viewer_principal_id).collect();
    people.sort();
    people.dedup();
    let sharing = shares_receipts(&state.db, env, &people).await?;
    let contacts = eligible_contacts(state, ctx, owner, Some(&people)).await?;
    let blocked = blocked_with(&state.db, env, owner).await?;
    let snapshot: HashMap<Uuid, &StatusRow> = rows.iter().map(|r| (r.id, r)).collect();
    let mut out: HashMap<Uuid, Vec<Value>> = HashMap::new();
    for v in views {
        let Some(row) = snapshot.get(&v.status_id) else {
            continue;
        };
        if !row.audience_principals.contains(&v.viewer_principal_id)
            || !sharing.contains(&v.viewer_principal_id)
            || !contacts.contains_key(&v.viewer_principal_id)
            || blocked.contains(&v.viewer_principal_id)
        {
            continue;
        }
        let list = out.entry(v.status_id).or_default();
        if (list.len() as i64) < MAX_VIEWERS {
            list.push(json!({
                "principalId": v.viewer_principal_id,
                "viewedAt": rfc3339(v.viewed_at),
                "liked": v.liked,
                "reactedAt": v.reaction.as_ref().and(v.reacted_at).map(rfc3339),
                "reaction": v.reaction,
            }));
        }
    }
    Ok((true, out))
}

/// Who saw one of the caller's statuses (owner only).
pub async fn viewers(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let row = load(&state.db, ctx.scope.environment_id, id)
        .await?
        .filter(|r| r.author_principal_id == acting.principal_id && r.expires_at > state.now())
        .ok_or_else(not_found)?;
    let (receipts, mut map) =
        viewers_of(state, ctx, acting.principal_id, std::slice::from_ref(&row)).await?;
    let list = map.remove(&row.id).unwrap_or_default();
    Ok(json!({
        "statusId": row.id,
        "readReceipts": receipts,
        "viewCount": if receipts { json!(list.len()) } else { Value::Null },
        "viewers": list,
    }))
}

/// "My status": the owner's live statuses with audience, viewers and sharing.
pub async fn mine(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    let me = acting.principal_id;
    let mut rows = live_of(&state.db, ctx.scope.environment_id, &[me], me, state.now()).await?;
    display_order(&mut rows);
    let (receipts, mut viewers) = viewers_of(state, ctx, me, &rows).await?;
    let mut posts = render(state, me, &rows).await?;
    for (row, post) in rows.iter().zip(posts.iter_mut()) {
        let list = viewers.remove(&row.id).unwrap_or_default();
        post["uniqueViewerCount"] = if receipts {
            json!(list.len())
        } else {
            Value::Null
        };
        post["viewers"] = json!(list);
    }
    Ok(json!({ "readReceipts": receipts, "posts": posts }))
}

// ---------------------------------------------------------------------------- update

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateStatus {
    pub allow_sharing: bool,
}

/// The author changes "Allow sharing" of one live status of theirs (forwards and reshares
/// check it at the time they happen; earlier forwards and reshares are not undone, a
/// reshare's readers still pass the original's gate). Anyone else: `404`.
pub async fn update(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: UpdateStatus,
) -> Result<Value> {
    let env = ctx.scope.environment_id;
    let row = load(&state.db, env, id)
        .await?
        .filter(|r| r.author_principal_id == acting.principal_id && r.expires_at > state.now())
        .ok_or_else(not_found)?;
    db::exec(
        &state.db,
        "UPDATE messaging_statuses SET allow_sharing = $3 WHERE id = $1 AND environment_id = $2",
        vec![row.id.into(), env.into(), req.allow_sharing.into()],
    )
    .await?;
    let row = load(&state.db, env, id).await?.ok_or_else(not_found)?;
    render_one(state, acting.principal_id, &row).await
}

// ---------------------------------------------------------------------------- delete

/// Deletes the caller's statuses (and reshares of them), then their media nothing else uses.
pub async fn delete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    ids: &[Uuid],
) -> Result<u64> {
    if ids.is_empty() || ids.len() > MAX_BULK_DELETE {
        return Err(Error::validation("Between 1 and 100 statuses"));
    }
    let env = ctx.scope.environment_id;
    let rows: Vec<StatusRow> = load_many(&state.db, env, ids)
        .await?
        .into_iter()
        .filter(|r| r.author_principal_id == acting.principal_id)
        .collect();
    if rows.is_empty() {
        return Err(not_found());
    }
    let own: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let reshares: Vec<StatusRow> = db::all(
        &state.db,
        &format!("SELECT {COLUMNS} FROM messaging_statuses WHERE origin_status_id = ANY($1)"),
        vec![own.clone().into()],
    )
    .await?;
    let media: Vec<Uuid> = rows
        .iter()
        .chain(reshares.iter())
        .flat_map(|r| [r.media_id, r.music_media_id])
        .flatten()
        .collect();
    let n = db::exec(
        &state.db,
        "DELETE FROM messaging_statuses WHERE environment_id = $1 AND (id = ANY($2) OR origin_status_id = ANY($2))",
        vec![env.into(), own.into()],
    )
    .await?;
    media::purge_if_unused(state, env, &media).await?;
    Ok(n.min(rows.len() as u64))
}

/// The sweep: statuses past their 24 hours, and reshares whose original is gone.
pub async fn purge_expired<C: ConnectionTrait>(db: &C, now: DateTime<Utc>) -> Result<u64> {
    let mut n = db::exec(
        db,
        "DELETE FROM messaging_statuses s WHERE s.origin_status_id IS NOT NULL AND NOT EXISTS \
           (SELECT 1 FROM messaging_statuses o WHERE o.id = s.origin_status_id AND o.expires_at > $1)",
        vec![now.into()],
    )
    .await?;
    n += db::exec(
        db,
        "DELETE FROM messaging_statuses WHERE id IN (SELECT id FROM messaging_statuses WHERE expires_at <= $1 LIMIT 5000)",
        vec![now.into()],
    )
    .await?;
    // ADR 0073: reply records go with their status (foreign key); this pass also removes
    // any that outlived it (each carries its status's expiry).
    db::exec(
        db,
        "DELETE FROM messaging_status_replies WHERE id IN (SELECT id FROM messaging_status_replies WHERE expires_at <= $1 LIMIT 5000)",
        vec![now.into()],
    )
    .await?;
    Ok(n)
}

// ------------------------------------------------------------------- reply / sharing

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplyRequest {
    pub text: String,
    pub client_message_id: Uuid,
}

fn what(row: &StatusRow) -> String {
    match row.kind.as_str() {
        "TEXT" => format!(
            "\u{201c}{}\u{201d}",
            row.words
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>()
        ),
        "IMAGE" => "photo".into(),
        "VIDEO" => "video".into(),
        _ => "voice note".into(),
    }
}

/// The caller's DIRECT MANAGED conversation with `other` (created when missing, under
/// every DIRECT rule: Communications policy, blocks, the messaging policy).
async fn direct_with(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    other: Uuid,
) -> Result<Uuid> {
    let request: crate::conversations::service::CreateRequest = serde_json::from_value(json!({
        "contextClass": "DIRECT",
        "securityProfile": "MANAGED",
        "counterpartPrincipalId": other,
    }))
    .map_err(|_| Error::code(ErrorCode::InternalError))?;
    let key = format!("status-direct-{}", Uuid::new_v4().simple());
    let created = crate::conversations::service::create(state, ctx, acting, request, &key).await?;
    created.body["conversationId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(Error::code(ErrorCode::InternalError))
}

async fn send_text(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation: Uuid,
    client_message_id: Uuid,
    text: String,
) -> Result<Value> {
    let request: crate::messages::SendRequest = serde_json::from_value(json!({
        "clientMessageId": client_message_id,
        "text": text,
    }))
    .map_err(|_| Error::code(ErrorCode::InternalError))?;
    let (_, view) = crate::messages::send(state, ctx, acting, conversation, request).await?;
    Ok(view)
}

/// Reply privately: a MANAGED message in the DIRECT chat with the status's author (only
/// when the two may chat; refused like any other direct message otherwise).
pub async fn reply(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ReplyRequest,
) -> Result<Value> {
    let text = clean_text(&Some(req.text), MAX_REPLY, "A reply")?
        .ok_or_else(|| Error::validation("Write a reply"))?;
    let row = visible(state, ctx, acting.principal_id, id).await?;
    if row.author_principal_id == acting.principal_id {
        return Err(Error::reason(ErrorCode::Conflict, "OWN_STATUS"));
    }
    let conversation = direct_with(state, ctx, acting, row.author_principal_id).await?;
    let message = send_text(
        state,
        ctx,
        acting,
        conversation,
        req.client_message_id,
        format!("{REPLY_PREFIX}{}): {text}", what(&row)),
    )
    .await?;
    // ADR 0073: which message answered which status, for the author's replies list (no
    // content; a replayed send finds the record already there).
    if let Some(message_id) = message["messageId"]
        .as_str()
        .and_then(|m| Uuid::parse_str(m).ok())
    {
        db::exec(
            &state.db,
            "INSERT INTO messaging_status_replies (id, environment_id, status_id, author_principal_id, replier_principal_id, \
               conversation_id, message_id, created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (message_id) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                ctx.scope.environment_id.into(),
                row.id.into(),
                row.author_principal_id.into(),
                acting.principal_id.into(),
                conversation.into(),
                message_id.into(),
                state.now().into(),
                row.expires_at.into(),
            ],
        )
        .await?;
    }
    Ok(json!({
        "statusId": row.id,
        "conversationId": conversation,
        "messageId": message["messageId"],
    }))
}

#[derive(FromQueryResult)]
struct ReplyRecord {
    id: Uuid,
    replier_principal_id: Uuid,
    conversation_id: Uuid,
    message_id: Uuid,
    created_at: DateTime<Utc>,
}

/// What the replier wrote: the chat message without the server's "Replying to your status
/// (...): " lead-in.
pub fn reply_text(message: &str) -> &str {
    message
        .strip_prefix(REPLY_PREFIX)
        .and_then(|rest| rest.split_once("): ").map(|(_, t)| t))
        .unwrap_or(message)
}

/// The private replies to one of the caller's live statuses (author only; ADR 0073),
/// newest first. Each is read through the author's own membership of the chat it arrived
/// in: a reply whose message was deleted, expired, hidden for the author or is no longer
/// visible to them, from someone the author blocked since, or in a chat the author left,
/// is not listed. Anyone else, and any status not live, answers `404 NOT_FOUND`.
pub async fn replies(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let me = acting.principal_id;
    let env = ctx.scope.environment_id;
    let now = state.now();
    let row = load(&state.db, env, id)
        .await?
        .filter(|r| r.author_principal_id == me && r.expires_at > now)
        .ok_or_else(not_found)?;
    let records: Vec<ReplyRecord> = db::all(
        &state.db,
        "SELECT id, replier_principal_id, conversation_id, message_id, created_at FROM messaging_status_replies \
          WHERE status_id = $1 AND author_principal_id = $2 AND environment_id = $3 AND expires_at > $4 \
          ORDER BY created_at DESC, id DESC LIMIT $5",
        vec![
            row.id.into(),
            me.into(),
            env.into(),
            now.into(),
            MAX_REPLIES.into(),
        ],
    )
    .await?;
    let blocked = blocked_with(&state.db, env, me).await?;
    let ids: Vec<Uuid> = records.iter().map(|r| r.message_id).collect();
    let hidden: HashSet<Uuid> = crate::messages::hidden_for(&state.db, me, &ids)
        .await?
        .into_iter()
        .collect();
    let mut memberships: HashMap<Uuid, Option<i64>> = HashMap::new();
    let mut out = Vec::with_capacity(records.len());
    for r in records {
        if blocked.contains(&r.replier_principal_id) || hidden.contains(&r.message_id) {
            continue;
        }
        let visible_from = match memberships.get(&r.conversation_id) {
            Some(v) => *v,
            None => {
                let m = crate::conversations::access::membership(
                    &state.db,
                    r.conversation_id,
                    me,
                    false,
                )
                .await?
                .filter(|m| m.is_active())
                .map(|m| m.visible_from_sequence);
                memberships.insert(r.conversation_id, m);
                m
            }
        };
        let Some(visible_from) = visible_from else {
            continue;
        };
        let Some(message) =
            crate::messages::load_message(&state.db, r.conversation_id, r.message_id, false)
                .await?
        else {
            continue;
        };
        if !message.is_active()
            || message.environment_id != env
            || message.sender_principal_id != r.replier_principal_id
            || message.sequence < visible_from
        {
            continue;
        }
        let text = crate::messages::managed::text_of(
            state,
            &message,
            crate::messages::managed::DecryptPurpose::MemberRead,
        )
        .await
        .map(|t| reply_text(&t).to_owned());
        out.push(json!({
            "replyId": r.id,
            "principalId": r.replier_principal_id,
            "conversationId": r.conversation_id,
            "messageId": r.message_id,
            "sequence": message.sequence,
            "text": text,
            "createdAt": rfc3339(r.created_at),
        }));
    }
    Ok(json!({ "statusId": row.id, "replies": out }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardRequest {
    pub principal_ids: Vec<Uuid>,
}

fn require_sharing(row: &StatusRow) -> Result<()> {
    if row.allow_sharing {
        Ok(())
    } else {
        Err(Error::reason(ErrorCode::Conflict, "SHARING_NOT_ALLOWED"))
    }
}

/// Forward a status to contacts: each recipient must be an eligible contact of the caller
/// AND pass the gate of the status itself (forwarding never widens the audience); others
/// are skipped. A recipient gets a MANAGED direct message pointing at the status, which
/// still opens only through the gate.
pub async fn forward(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ForwardRequest,
) -> Result<Value> {
    let me = acting.principal_id;
    if req.principal_ids.is_empty() || req.principal_ids.len() > MAX_FORWARD_TARGETS {
        return Err(Error::validation("Between 1 and 20 recipients"));
    }
    let row = visible(state, ctx, me, id).await?;
    require_sharing(&row)?;
    if let Some(o) = row.origin_status_id {
        let origin = load(&state.db, ctx.scope.environment_id, o)
            .await?
            .ok_or_else(not_found)?;
        require_sharing(&origin)?;
    }
    let mut targets = req.principal_ids.clone();
    targets.sort();
    targets.dedup();
    targets.retain(|t| *t != me && *t != row.author_principal_id);
    let mine = eligible_contacts(state, ctx, me, Some(&targets)).await?;
    let (mut forwarded, mut skipped) = (0, 0);
    let root = row.origin_status_id.unwrap_or(row.id);
    let root_author = row
        .origin_author_principal_id
        .unwrap_or(row.author_principal_id);
    for t in targets {
        let allowed = mine.contains_key(&t)
            && Gate::load(state, ctx, t, std::slice::from_ref(&row))
                .await?
                .allows(&row);
        if !allowed {
            skipped += 1;
            continue;
        }
        let conversation = match direct_with(state, ctx, acting, t).await {
            Ok(c) => c,
            Err(e) if e.error_code() == ErrorCode::ServiceUnavailable => return Err(e),
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if send_text(
            state,
            ctx,
            acting,
            conversation,
            Uuid::new_v4(),
            format!("Shared a status with you: cfmeet://status/{}", row.id),
        )
        .await
        .is_err()
        {
            skipped += 1;
            continue;
        }
        let share = Uuid::new_v4();
        let now = state.now();
        let txn = state.db.begin().await?;
        db::exec(
            &txn,
            "INSERT INTO messaging_status_shares (id, status_id, sharer_principal_id, kind, target_principal_id, created_at) \
             VALUES ($1, $2, $3, 'FORWARD', $4, $5)",
            vec![share.into(), root.into(), me.into(), t.into(), now.into()],
        )
        .await?;
        notices::enqueue(
            state,
            &txn,
            ctx,
            notices::Kind::Share,
            share,
            root,
            root_author,
            now,
        )
        .await?;
        txn.commit().await?;
        forwarded += 1;
    }
    Ok(json!({ "statusId": row.id, "forwarded": forwarded, "skipped": skipped }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReshareRequest {
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub audience: Option<AudienceChoice>,
}

/// Reshare as a new status of the caller, attributed to the original publisher. Its
/// readers must ALSO pass the original's gate (never wider than the original audience).
pub async fn reshare(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: ReshareRequest,
    key: Option<String>,
) -> Result<(u16, Value)> {
    let me = acting.principal_id;
    let idem = key.map(|k| {
        let mut body = serde_json::to_value(&req).unwrap_or(Value::Null);
        body["statusId"] = json!(id);
        Idempotent::new("status.reshare", k, me, &body)
    });
    if let Some(idem) = &idem {
        if let Some(stored) =
            super::replayed(&state.db, ctx.scope.environment_id, idem, state.now()).await?
        {
            return replay(state, ctx, me, stored).await;
        }
    }
    let row = visible(state, ctx, me, id).await?;
    if row.author_principal_id == me && row.origin_status_id.is_none() {
        return Err(Error::reason(ErrorCode::Conflict, "OWN_STATUS"));
    }
    require_sharing(&row)?;
    let origin = match row.origin_status_id {
        Some(o) => {
            let origin = load(&state.db, ctx.scope.environment_id, o)
                .await?
                .ok_or_else(not_found)?;
            require_sharing(&origin)?;
            origin
        }
        None => row.clone(),
    };
    live_count(state, ctx, me).await?;
    let content = Content {
        kind: match origin.kind.as_str() {
            "IMAGE" => StatusKind::Image,
            "VIDEO" => StatusKind::Video,
            "AUDIO" => StatusKind::Audio,
            _ => StatusKind::Text,
        },
        text: origin.words.clone(),
        caption: match clean_text(&req.caption, MAX_CAPTION, "A caption")? {
            Some(c) => Some(c),
            None => origin.caption.clone(),
        },
        style: origin.style.clone(),
        media: match origin.media_id {
            Some(m) => media::load(&state.db, ctx.scope.environment_id, m).await?,
            None => None,
        },
        start_ms: origin.start_ms,
        end_ms: origin.end_ms,
        music: match origin.music_media_id {
            Some(m) => media::load(&state.db, ctx.scope.environment_id, m).await?,
            None => None,
        },
        music_start_ms: origin.music_start_ms,
        music_end_ms: origin.music_end_ms,
        music_title: origin.music_title.clone(),
    };
    let (snapshot, _) = resolve_audience(state, ctx, me, req.audience.clone()).await?;
    publish(
        state,
        ctx,
        me,
        content,
        snapshot,
        false,
        None,
        None,
        Some((origin.id, origin.author_principal_id)),
        &[],
        idem,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(author: Uuid, group: Option<Uuid>, seq: Option<i32>, at: i64) -> StatusRow {
        let t = DateTime::from_timestamp(1_800_000_000 + at, 0).unwrap();
        StatusRow {
            id: Uuid::new_v4(),
            tenant_id: Uuid::nil(),
            application_id: Uuid::nil(),
            environment_id: Uuid::nil(),
            author_principal_id: author,
            kind: "TEXT".into(),
            words: Some("x".into()),
            caption: None,
            style: json!({}),
            media_id: None,
            start_ms: None,
            end_ms: None,
            music_media_id: None,
            music_start_ms: None,
            music_end_ms: None,
            music_title: None,
            group_id: group,
            sequence: seq,
            audience_mode: "CONTACTS".into(),
            audience_list_id: None,
            audience_label: None,
            audience_principals: vec![],
            excluded_count: 0,
            audience_as_of: Some(t),
            allow_sharing: true,
            origin_status_id: None,
            origin_author_principal_id: None,
            created_at: t,
            expires_at: t + Duration::hours(24),
        }
    }

    #[test]
    fn a_group_shows_in_sequence_order_at_its_first_arrival() {
        let a = Uuid::new_v4();
        let g = Uuid::new_v4();
        // Arrived out of order: sequence 2 first, then an unrelated status, then 0 and 1.
        let s2 = row(a, Some(g), Some(2), 0);
        let other = row(a, None, None, 1);
        let s0 = row(a, Some(g), Some(0), 2);
        let s1 = row(a, Some(g), Some(1), 3);
        let mut rows = vec![other.clone(), s1.clone(), s2.clone(), s0.clone()];
        display_order(&mut rows);
        let order: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        assert_eq!(order, vec![s0.id, s1.id, s2.id, other.id]);
    }

    #[test]
    fn segments_are_at_most_a_minute() {
        assert!(segment(Some(0), Some(60_000), "x").unwrap().is_some());
        assert!(segment(Some(0), Some(60_001), "x").is_err());
        assert!(segment(Some(5), Some(5), "x").is_err());
        assert!(segment(Some(5), None, "x").is_err());
        assert!(segment(None, None, "x").unwrap().is_none());
    }
}

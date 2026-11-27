//! Updates (WhatsApp-style): status posts that last 24 hours and channels people follow.
//!
//! * **Media** (photos, videos, voice notes) is uploaded in [`CHUNK_BYTES`] pieces through
//!   the normal API (no direct bucket upload), checked by content when complete, and read
//!   back the same way. Status media is deleted with its posts.
//! * **Status**: text (background colour, font), a photo, a voice note or a video segment of
//!   at most 60 s (a longer video is uploaded once and posted as consecutive segments). Seen
//!   by the poster's CONNECTED contacts (Communications Hub) for 24 hours, then deleted —
//!   posts, views and media — by [`purge_expired`].
//! * **Channels**: anyone signed in creates one; others find it in the directory, follow it,
//!   read its posts and react with one emoji each. Only the owner posts.

use std::collections::{HashMap, HashSet};

use axum::http::HeaderMap;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement, Value as DbValue};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::actor::RegisteredActor,
    state::AppState,
};

/// Size of every uploaded piece but the last.
pub const CHUNK_BYTES: usize = 512 * 1024;
/// Request body limit of the chunk upload (a chunk plus slack).
pub const CHUNK_BODY_LIMIT: usize = CHUNK_BYTES + 4096;
pub const MAX_IMAGE_BYTES: i64 = 8 * 1024 * 1024;
pub const MAX_AUDIO_BYTES: i64 = 8 * 1024 * 1024;
pub const MAX_VIDEO_BYTES: i64 = 64 * 1024 * 1024;
/// A status video segment or voice note: at most one minute.
pub const MAX_SEGMENT_MS: i32 = 60_000;
pub const STATUS_LIFETIME_HOURS: i64 = 24;
const MAX_LIVE_POSTS: i64 = 60;
const MAX_UPLOADS_PER_DAY: i64 = 300;
const MAX_CHANNELS_PER_USER: i64 = 10;

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::domain(ErrorCode::ValidationError, msg)
}
fn not_found(what: &str) -> ApiError {
    ApiError::NotFound(format!("{what} not found"))
}
fn denied(msg: &str) -> ApiError {
    ApiError::domain(ErrorCode::PermissionDenied, msg)
}

async fn query<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<DbValue>,
) -> ApiResult<Vec<QueryResult>> {
    Ok(db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?)
}
async fn query_one<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<DbValue>,
) -> ApiResult<Option<QueryResult>> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?)
}
async fn exec<C: ConnectionTrait>(db: &C, sql: &str, values: Vec<DbValue>) -> ApiResult<u64> {
    Ok(db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?
        .rows_affected())
}

// ------------------------------------------------------------------------------ media

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}
impl MediaKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Image => "IMAGE",
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "IMAGE" => Some(Self::Image),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            _ => None,
        }
    }
    fn max_bytes(self) -> i64 {
        match self {
            Self::Image => MAX_IMAGE_BYTES,
            Self::Video => MAX_VIDEO_BYTES,
            Self::Audio => MAX_AUDIO_BYTES,
        }
    }
    fn accepts(self, content_type: &str) -> bool {
        let t = content_type.split(';').next().unwrap_or("").trim();
        match self {
            Self::Image => matches!(t, "image/jpeg" | "image/png" | "image/webp" | "image/gif"),
            Self::Video => matches!(t, "video/mp4" | "video/webm" | "video/quicktime"),
            Self::Audio => matches!(
                t,
                "audio/webm"
                    | "audio/ogg"
                    | "audio/mp4"
                    | "audio/mpeg"
                    | "audio/aac"
                    | "audio/wav"
                    | "audio/x-m4a"
            ),
        }
    }
}

/// Recognise the first bytes of an upload as the declared kind of media.
pub fn sniff_matches(kind: MediaKind, head: &[u8]) -> bool {
    let starts = |p: &[u8]| head.len() >= p.len() && &head[..p.len()] == p;
    let ftyp = head.len() >= 12 && &head[4..8] == b"ftyp";
    let webm = starts(&[0x1A, 0x45, 0xDF, 0xA3]);
    match kind {
        MediaKind::Image => {
            starts(&[0xFF, 0xD8, 0xFF])
                || starts(b"\x89PNG\r\n\x1a\n")
                || (starts(b"RIFF") && head.len() >= 12 && &head[8..12] == b"WEBP")
                || starts(b"GIF87a")
                || starts(b"GIF89a")
        }
        MediaKind::Video => ftyp || webm,
        MediaKind::Audio => {
            ftyp || webm
                || starts(b"OggS")
                || starts(b"ID3")
                || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0)
                || (starts(b"RIFF") && head.len() >= 12 && &head[8..12] == b"WAVE")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum MediaPurpose {
    Status,
    Channel,
}
impl MediaPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Status => "STATUS",
            Self::Channel => "CHANNEL",
        }
    }
}

/// An uploaded (or uploading) photo, video or voice note.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaView {
    pub media_id: Uuid,
    pub kind: MediaKind,
    pub content_type: String,
    pub size_bytes: i64,
    /// Every chunk but the last has exactly this many bytes.
    pub chunk_bytes: i64,
    pub chunk_count: i32,
    pub duration_ms: Option<i32>,
    pub complete: bool,
}

fn media_view(r: &QueryResult) -> ApiResult<MediaView> {
    let kind: String = r.try_get("", "kind")?;
    let completed: Option<DateTime<Utc>> = r.try_get("", "completed_at")?;
    Ok(MediaView {
        media_id: r.try_get("", "id")?,
        kind: MediaKind::parse(&kind).ok_or_else(|| ApiError::InternalError("kind".into()))?,
        content_type: r.try_get("", "content_type")?,
        size_bytes: r.try_get("", "size_bytes")?,
        chunk_bytes: CHUNK_BYTES as i64,
        chunk_count: r.try_get("", "chunk_count")?,
        duration_ms: r.try_get("", "duration_ms")?,
        complete: completed.is_some(),
    })
}

const MEDIA_COLUMNS: &str =
    "id, owner_user_id, purpose, kind, content_type, size_bytes, chunk_count, duration_ms, completed_at";

/// Start an upload: the media's kind, type, size and (video / voice note) duration.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMediaRequest {
    pub purpose: MediaPurpose,
    pub kind: MediaKind,
    #[schema(example = "video/mp4")]
    pub content_type: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i32>,
}

pub async fn create_media<C: ConnectionTrait>(
    db: &C,
    owner: Uuid,
    req: &CreateMediaRequest,
) -> ApiResult<MediaView> {
    if !req.kind.accepts(&req.content_type) || req.content_type.len() > 64 {
        return Err(invalid("This type of file isn't supported here"));
    }
    if req.size_bytes <= 0 || req.size_bytes > req.kind.max_bytes() {
        return Err(invalid(format!(
            "Files of this kind can be at most {} MB",
            req.kind.max_bytes() / (1024 * 1024)
        )));
    }
    if let Some(d) = req.duration_ms {
        if !(0..=3_600_000).contains(&d) {
            return Err(invalid("durationMs is out of range"));
        }
    }
    let recent = query_one(
        db,
        "SELECT count(*) AS n FROM social_media WHERE owner_user_id = $1 AND created_at > now() - interval '1 day'",
        vec![owner.into()],
    )
    .await?
    .map(|r| r.try_get::<i64>("", "n"))
    .transpose()?
    .unwrap_or(0);
    if recent >= MAX_UPLOADS_PER_DAY {
        return Err(ApiError::domain(
            ErrorCode::RateLimited,
            "Too many uploads today. Try again tomorrow.",
        ));
    }
    let chunks = ((req.size_bytes as usize).div_ceil(CHUNK_BYTES)) as i32;
    let id = Uuid::new_v4();
    let row = query_one(
        db,
        &format!(
            "INSERT INTO social_media (id, owner_user_id, purpose, kind, content_type, size_bytes, chunk_count, duration_ms) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {MEDIA_COLUMNS}"
        ),
        vec![
            id.into(),
            owner.into(),
            req.purpose.as_str().into(),
            req.kind.as_str().into(),
            req.content_type.trim().to_string().into(),
            req.size_bytes.into(),
            chunks.into(),
            req.duration_ms.into(),
        ],
    )
    .await?
    .ok_or_else(|| ApiError::InternalError("media insert".into()))?;
    media_view(&row)
}

async fn load_media<C: ConnectionTrait>(db: &C, id: Uuid) -> ApiResult<Option<QueryResult>> {
    query_one(
        db,
        &format!("SELECT {MEDIA_COLUMNS} FROM social_media WHERE id = $1"),
        vec![id.into()],
    )
    .await
}

/// Store piece `seq` of the caller's unfinished upload (exact size; re-sending replaces it).
pub async fn put_chunk<C: ConnectionTrait>(
    db: &C,
    owner: Uuid,
    media_id: Uuid,
    seq: i32,
    bytes: &[u8],
) -> ApiResult<()> {
    let row = load_media(db, media_id)
        .await?
        .ok_or_else(|| not_found("Upload"))?;
    let owner_id: Uuid = row.try_get("", "owner_user_id")?;
    if owner_id != owner {
        return Err(not_found("Upload"));
    }
    let view = media_view(&row)?;
    if view.complete {
        return Err(invalid("This upload is already complete"));
    }
    if seq < 0 || seq >= view.chunk_count {
        return Err(invalid("No such piece of this upload"));
    }
    let expected = if seq == view.chunk_count - 1 {
        view.size_bytes - CHUNK_BYTES as i64 * (view.chunk_count as i64 - 1)
    } else {
        CHUNK_BYTES as i64
    };
    if bytes.len() as i64 != expected {
        return Err(invalid(format!(
            "Piece {seq} must be {expected} bytes, not {}",
            bytes.len()
        )));
    }
    exec(
        db,
        "INSERT INTO social_media_chunks (media_id, seq, bytes) VALUES ($1, $2, $3) \
         ON CONFLICT (media_id, seq) DO UPDATE SET bytes = EXCLUDED.bytes",
        vec![media_id.into(), seq.into(), bytes.to_vec().into()],
    )
    .await?;
    Ok(())
}

/// Finish the caller's upload: every piece is there and the content is the declared kind.
pub async fn complete_media<C: ConnectionTrait>(
    db: &C,
    owner: Uuid,
    media_id: Uuid,
) -> ApiResult<MediaView> {
    let row = load_media(db, media_id)
        .await?
        .ok_or_else(|| not_found("Upload"))?;
    let owner_id: Uuid = row.try_get("", "owner_user_id")?;
    if owner_id != owner {
        return Err(not_found("Upload"));
    }
    let view = media_view(&row)?;
    if view.complete {
        return Ok(view);
    }
    let stored = query_one(
        db,
        "SELECT count(*) AS n, coalesce(sum(octet_length(bytes)), 0)::bigint AS total FROM social_media_chunks WHERE media_id = $1",
        vec![media_id.into()],
    )
    .await?
    .ok_or_else(|| ApiError::InternalError("chunk count".into()))?;
    let n: i64 = stored.try_get("", "n")?;
    let total: i64 = stored.try_get("", "total")?;
    if n != view.chunk_count as i64 || total != view.size_bytes {
        return Err(invalid(format!(
            "The upload is incomplete ({n} of {} pieces)",
            view.chunk_count
        )));
    }
    let head = query_one(
        db,
        "SELECT substring(bytes from 1 for 16) AS head FROM social_media_chunks WHERE media_id = $1 AND seq = 0",
        vec![media_id.into()],
    )
    .await?
    .map(|r| r.try_get::<Vec<u8>>("", "head"))
    .transpose()?
    .unwrap_or_default();
    if !sniff_matches(view.kind, &head) {
        exec(
            db,
            "DELETE FROM social_media WHERE id = $1",
            vec![media_id.into()],
        )
        .await?;
        return Err(invalid(
            "The file isn't a photo, video or voice note this app can show",
        ));
    }
    let row = query_one(
        db,
        &format!(
            "UPDATE social_media SET completed_at = now() WHERE id = $1 RETURNING {MEDIA_COLUMNS}"
        ),
        vec![media_id.into()],
    )
    .await?
    .ok_or_else(|| not_found("Upload"))?;
    media_view(&row)
}

/// Whether `viewer` may read media `id` (and its view). Owner always; channel media any
/// signed-in person; status media the owner's connected contacts while a post uses it.
pub async fn readable_media(
    state: &AppState,
    viewer: &RegisteredActor,
    headers: &HeaderMap,
    media_id: Uuid,
) -> ApiResult<MediaView> {
    let row = load_media(&state.db, media_id)
        .await?
        .ok_or_else(|| not_found("Media"))?;
    let owner: Uuid = row.try_get("", "owner_user_id")?;
    let purpose: String = row.try_get("", "purpose")?;
    let view = media_view(&row)?;
    if owner == viewer.user_id {
        return Ok(view);
    }
    if !view.complete {
        return Err(not_found("Media"));
    }
    let allowed = match purpose.as_str() {
        "CHANNEL" => true,
        _ => {
            let live = query_one(
                &state.db,
                "SELECT 1 AS x FROM status_posts WHERE media_id = $1 AND expires_at > now() LIMIT 1",
                vec![media_id.into()],
            )
            .await?
            .is_some();
            live && visible_owners(state, viewer, headers, &[owner])
                .await?
                .contains(&owner)
        }
    };
    if allowed {
        Ok(view)
    } else {
        Err(not_found("Media"))
    }
}

pub async fn read_chunk<C: ConnectionTrait>(
    db: &C,
    media_id: Uuid,
    seq: i32,
) -> ApiResult<Vec<u8>> {
    query_one(
        db,
        "SELECT bytes FROM social_media_chunks WHERE media_id = $1 AND seq = $2",
        vec![media_id.into(), seq.into()],
    )
    .await?
    .map(|r| r.try_get::<Vec<u8>>("", "bytes"))
    .transpose()?
    .ok_or_else(|| not_found("Piece"))
}

// ------------------------------------------------------------------------- audience

/// Of `owners`, the people whose status `viewer` may see: themself and their CONNECTED,
/// unblocked Communications Hub contacts. Without the Messaging / Hub integration only the
/// viewer themself (never a wider audience).
pub async fn visible_owners(
    state: &AppState,
    viewer: &RegisteredActor,
    headers: &HeaderMap,
    owners: &[Uuid],
) -> ApiResult<HashSet<Uuid>> {
    let mut out: HashSet<Uuid> = owners
        .iter()
        .copied()
        .filter(|o| *o == viewer.user_id)
        .collect();
    let others: Vec<Uuid> = owners
        .iter()
        .copied()
        .filter(|o| *o != viewer.user_id)
        .take(256)
        .collect();
    let Some(um) = state.meeting.core.unified.as_ref() else {
        return Ok(out);
    };
    if others.is_empty() {
        return Ok(out);
    }
    let links = query(
        &state.db,
        "SELECT principal_id, subject_id FROM communication_principal_links \
          WHERE subject_kind = 'USER' AND environment_id = $1 AND subject_id = ANY($2)",
        vec![um.environment_id().into(), others.into()],
    )
    .await?;
    let mut by_principal: HashMap<Uuid, Uuid> = HashMap::new();
    for r in &links {
        by_principal.insert(r.try_get("", "principal_id")?, r.try_get("", "subject_id")?);
    }
    if by_principal.is_empty() {
        return Ok(out);
    }
    let principals: Vec<Uuid> = by_principal.keys().copied().collect();
    match crate::services::communications::contact_names(state, viewer, headers, &principals).await
    {
        Ok(contacts) => {
            for c in contacts {
                if let Some(u) = by_principal.get(&c.principal_id) {
                    out.insert(*u);
                }
            }
        }
        // The Hub being down narrows the audience; it never widens it.
        Err(e) => tracing::warn!(error = %e, "status audience unavailable"),
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonView {
    pub user_id: Uuid,
    pub name: String,
    pub avatar_url: Option<String>,
    /// Their chat identity (to reply to a status in a chat), when known.
    pub principal_id: Option<Uuid>,
}

async fn people<C: ConnectionTrait>(
    db: &C,
    ids: &[Uuid],
    environment: Option<Uuid>,
) -> ApiResult<HashMap<Uuid, PersonView>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = query(
        db,
        "SELECT u.id, u.username, u.display_name, u.avatar_url,                 (SELECT l.principal_id FROM communication_principal_links l                   WHERE l.subject_kind = 'USER' AND l.subject_id = u.id AND l.environment_id = $2 LIMIT 1) AS principal_id            FROM users u WHERE u.id = ANY($1)",
        vec![ids.to_vec().into(), environment.into()],
    )
    .await?;
    let mut out = HashMap::new();
    for r in rows {
        let id: Uuid = r.try_get("", "id")?;
        let username: String = r.try_get("", "username")?;
        let display: Option<String> = r.try_get("", "display_name")?;
        out.insert(
            id,
            PersonView {
                user_id: id,
                name: display.filter(|d| !d.trim().is_empty()).unwrap_or(username),
                avatar_url: r.try_get("", "avatar_url")?,
                principal_id: r.try_get("", "principal_id")?,
            },
        );
    }
    Ok(out)
}

// --------------------------------------------------------------------------- status

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum PostKind {
    Text,
    Image,
    Video,
    Audio,
}
impl PostKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "TEXT",
            Self::Image => "IMAGE",
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "TEXT" => Some(Self::Text),
            "IMAGE" => Some(Self::Image),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            _ => None,
        }
    }
    fn media(self) -> Option<MediaKind> {
        match self {
            Self::Text => None,
            Self::Image => Some(MediaKind::Image),
            Self::Video => Some(MediaKind::Video),
            Self::Audio => Some(MediaKind::Audio),
        }
    }
}

/// A new status: text, or an uploaded photo / voice note / video segment (≤ 60 s).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateStatusRequest {
    pub kind: PostKind,
    /// TEXT: the words (1–700 characters).
    pub text: Option<String>,
    /// TEXT: background colour `#RRGGBB`.
    pub background: Option<String>,
    /// TEXT: font style 0–4.
    pub font: Option<i16>,
    /// IMAGE / VIDEO / AUDIO: a completed STATUS upload of the caller.
    pub media_id: Option<Uuid>,
    /// VIDEO: the segment shown, at most 60 000 ms long.
    pub start_ms: Option<i32>,
    pub end_ms: Option<i32>,
    /// Photo / video caption (at most 700 characters).
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatusPostView {
    pub post_id: Uuid,
    pub kind: PostKind,
    pub text: Option<String>,
    pub background: Option<String>,
    pub font: i16,
    pub media: Option<MediaView>,
    pub start_ms: Option<i32>,
    pub end_ms: Option<i32>,
    pub caption: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The viewer has seen it (always true for your own).
    pub viewed: bool,
    pub liked: bool,
    /// Your own posts: how many people saw it.
    pub view_count: Option<i64>,
}

fn clean(s: &Option<String>, max: usize, what: &str) -> ApiResult<Option<String>> {
    match s.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(t) if t.chars().count() > max => {
            Err(invalid(format!("{what} can be at most {max} characters")))
        }
        Some(t) => Ok(Some(t.to_string())),
    }
}

pub async fn create_status<C: ConnectionTrait>(
    db: &C,
    user: Uuid,
    req: &CreateStatusRequest,
) -> ApiResult<StatusPostView> {
    let live = query_one(
        db,
        "SELECT count(*) AS n FROM status_posts WHERE user_id = $1 AND expires_at > now()",
        vec![user.into()],
    )
    .await?
    .map(|r| r.try_get::<i64>("", "n"))
    .transpose()?
    .unwrap_or(0);
    if live >= MAX_LIVE_POSTS {
        return Err(invalid("You have too many status updates right now"));
    }
    let caption = clean(&req.caption, 700, "A caption")?;
    let (text, background, font, media_id, start, end) = match req.kind.media() {
        None => {
            let text = clean(&req.text, 700, "A status")?
                .ok_or_else(|| invalid("Write something for your status"))?;
            let bg = match req.background.as_deref() {
                None => None,
                Some(b)
                    if b.len() == 7
                        && b.starts_with('#')
                        && b[1..].chars().all(|c| c.is_ascii_hexdigit()) =>
                {
                    Some(b.to_string())
                }
                Some(_) => return Err(invalid("background must be a #RRGGBB colour")),
            };
            let font = req.font.unwrap_or(0);
            if !(0..=4).contains(&font) {
                return Err(invalid("font must be 0 to 4"));
            }
            (Some(text), bg, font, None, None, None)
        }
        Some(kind) => {
            let id = req.media_id.ok_or_else(|| invalid("mediaId is required"))?;
            let row = load_media(db, id)
                .await?
                .ok_or_else(|| not_found("Upload"))?;
            let owner: Uuid = row.try_get("", "owner_user_id")?;
            let purpose: String = row.try_get("", "purpose")?;
            let view = media_view(&row)?;
            if owner != user || purpose != "STATUS" {
                return Err(not_found("Upload"));
            }
            if !view.complete || view.kind != kind {
                return Err(invalid("The upload isn't a finished file of this kind"));
            }
            let (start, end) = match kind {
                MediaKind::Video => {
                    let (s, e) = req
                        .start_ms
                        .zip(req.end_ms)
                        .ok_or_else(|| invalid("A video status needs startMs and endMs"))?;
                    if s < 0 || e <= s || e - s > MAX_SEGMENT_MS {
                        return Err(invalid("A video status is at most 60 seconds"));
                    }
                    if view.duration_ms.is_some_and(|d| s >= d) {
                        return Err(invalid("The segment starts after the video ends"));
                    }
                    (Some(s), Some(e))
                }
                MediaKind::Audio => {
                    if view.duration_ms.is_some_and(|d| d > MAX_SEGMENT_MS + 1000) {
                        return Err(invalid("A voice status is at most 60 seconds"));
                    }
                    (None, None)
                }
                MediaKind::Image => (None, None),
            };
            (None, None, 0, Some(id), start, end)
        }
    };
    let id = Uuid::new_v4();
    let now = Utc::now();
    exec(
        db,
        "INSERT INTO status_posts (id, user_id, kind, body_text, background, font, media_id, start_ms, end_ms, caption, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            id.into(),
            user.into(),
            req.kind.as_str().into(),
            text.into(),
            background.into(),
            font.into(),
            media_id.into(),
            start.into(),
            end.into(),
            caption.into(),
            now.into(),
            (now + Duration::hours(STATUS_LIFETIME_HOURS)).into(),
        ],
    )
    .await?;
    let rows = status_rows(db, &[user], user, Some(id)).await?;
    rows.into_iter()
        .next()
        .map(|(_, p)| p)
        .ok_or_else(|| ApiError::InternalError("status read-back".into()))
}

/// Live posts of `owners` (oldest first), as `viewer` sees them.
async fn status_rows<C: ConnectionTrait>(
    db: &C,
    owners: &[Uuid],
    viewer: Uuid,
    only: Option<Uuid>,
) -> ApiResult<Vec<(Uuid, StatusPostView)>> {
    if owners.is_empty() {
        return Ok(vec![]);
    }
    let rows = query(
        db,
        "SELECT p.id, p.user_id, p.kind, p.body_text, p.background, p.font, p.media_id, p.start_ms, p.end_ms, p.caption, \
                p.created_at, p.expires_at, \
                m.id AS m_id, m.kind AS m_kind, m.content_type AS m_type, m.size_bytes AS m_size, m.chunk_count AS m_chunks, \
                m.duration_ms AS m_duration, m.completed_at AS m_completed, \
                v.viewed_at, coalesce(v.liked, false) AS liked, \
                (SELECT count(*) FROM status_views x WHERE x.post_id = p.id AND x.viewer_user_id <> p.user_id) AS views \
           FROM status_posts p \
           LEFT JOIN social_media m ON m.id = p.media_id \
           LEFT JOIN status_views v ON v.post_id = p.id AND v.viewer_user_id = $2 \
          WHERE p.user_id = ANY($1) AND p.expires_at > now() AND ($3::uuid IS NULL OR p.id = $3) \
          ORDER BY p.created_at ASC",
        vec![owners.to_vec().into(), viewer.into(), only.into()],
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let owner: Uuid = r.try_get("", "user_id")?;
        let kind: String = r.try_get("", "kind")?;
        let media = match r.try_get::<Option<Uuid>>("", "m_id")? {
            Some(mid) => {
                let mk: String = r.try_get("", "m_kind")?;
                let completed: Option<DateTime<Utc>> = r.try_get("", "m_completed")?;
                Some(MediaView {
                    media_id: mid,
                    kind: MediaKind::parse(&mk).unwrap_or(MediaKind::Image),
                    content_type: r.try_get("", "m_type")?,
                    size_bytes: r.try_get("", "m_size")?,
                    chunk_bytes: CHUNK_BYTES as i64,
                    chunk_count: r.try_get("", "m_chunks")?,
                    duration_ms: r.try_get("", "m_duration")?,
                    complete: completed.is_some(),
                })
            }
            None => None,
        };
        let viewed_at: Option<DateTime<Utc>> = r.try_get("", "viewed_at")?;
        let mine = owner == viewer;
        out.push((
            owner,
            StatusPostView {
                post_id: r.try_get("", "id")?,
                kind: PostKind::parse(&kind).unwrap_or(PostKind::Text),
                text: r.try_get("", "body_text")?,
                background: r.try_get("", "background")?,
                font: r.try_get("", "font")?,
                media,
                start_ms: r.try_get("", "start_ms")?,
                end_ms: r.try_get("", "end_ms")?,
                caption: r.try_get("", "caption")?,
                created_at: r.try_get("", "created_at")?,
                expires_at: r.try_get("", "expires_at")?,
                viewed: mine || viewed_at.is_some(),
                liked: r.try_get("", "liked")?,
                view_count: if mine {
                    Some(r.try_get("", "views")?)
                } else {
                    None
                },
            },
        ));
    }
    Ok(out)
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatusAuthorView {
    pub person: PersonView,
    pub posts: Vec<StatusPostView>,
    pub all_viewed: bool,
    pub last_posted_at: DateTime<Utc>,
}

/// The Updates screen's status row: your own posts and your contacts' (unseen first).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatusFeed {
    pub mine: Vec<StatusPostView>,
    pub contacts: Vec<StatusAuthorView>,
}

pub async fn feed(
    state: &AppState,
    viewer: &RegisteredActor,
    headers: &HeaderMap,
) -> ApiResult<StatusFeed> {
    let me = viewer.user_id;
    let mine = status_rows(&state.db, &[me], me, None)
        .await?
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    let posters: Vec<Uuid> = query(
        &state.db,
        "SELECT user_id FROM status_posts WHERE expires_at > now() AND user_id <> $1 \
          GROUP BY user_id ORDER BY max(created_at) DESC LIMIT 256",
        vec![me.into()],
    )
    .await?
    .iter()
    .map(|r| r.try_get("", "user_id"))
    .collect::<Result<_, _>>()?;
    let visible: Vec<Uuid> = visible_owners(state, viewer, headers, &posters)
        .await?
        .into_iter()
        .filter(|u| *u != me)
        .collect();
    let environment = state
        .meeting
        .core
        .unified
        .as_ref()
        .map(|u| u.environment_id());
    let names = people(&state.db, &visible, environment).await?;
    let mut grouped: HashMap<Uuid, Vec<StatusPostView>> = HashMap::new();
    for (owner, p) in status_rows(&state.db, &visible, me, None).await? {
        grouped.entry(owner).or_default().push(p);
    }
    let mut contacts: Vec<StatusAuthorView> = grouped
        .into_iter()
        .filter_map(|(owner, posts)| {
            let person = names.get(&owner)?.clone();
            let last = posts.iter().map(|p| p.created_at).max()?;
            Some(StatusAuthorView {
                all_viewed: posts.iter().all(|p| p.viewed),
                person,
                posts,
                last_posted_at: last,
            })
        })
        .collect();
    contacts.sort_by(|a, b| {
        a.all_viewed
            .cmp(&b.all_viewed)
            .then(b.last_posted_at.cmp(&a.last_posted_at))
    });
    Ok(StatusFeed { mine, contacts })
}

async fn post_owner<C: ConnectionTrait>(db: &C, post: Uuid) -> ApiResult<Uuid> {
    query_one(
        db,
        "SELECT user_id FROM status_posts WHERE id = $1 AND expires_at > now()",
        vec![post.into()],
    )
    .await?
    .map(|r| r.try_get::<Uuid>("", "user_id"))
    .transpose()?
    .ok_or_else(|| not_found("Status"))
}

/// Record that the viewer saw (and maybe liked) a contact's status.
pub async fn view_status(
    state: &AppState,
    viewer: &RegisteredActor,
    headers: &HeaderMap,
    post: Uuid,
    liked: Option<bool>,
) -> ApiResult<()> {
    let owner = post_owner(&state.db, post).await?;
    if !visible_owners(state, viewer, headers, &[owner])
        .await?
        .contains(&owner)
    {
        return Err(not_found("Status"));
    }
    exec(
        &state.db,
        "INSERT INTO status_views (post_id, viewer_user_id, liked) VALUES ($1, $2, coalesce($3, false)) \
         ON CONFLICT (post_id, viewer_user_id) DO UPDATE SET liked = coalesce($3, status_views.liked)",
        vec![post.into(), viewer.user_id.into(), liked.into()],
    )
    .await?;
    Ok(())
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatusViewerView {
    pub person: PersonView,
    pub viewed_at: DateTime<Utc>,
    pub liked: bool,
}

/// Who saw your status (newest first).
pub async fn viewers<C: ConnectionTrait>(
    db: &C,
    user: Uuid,
    post: Uuid,
) -> ApiResult<Vec<StatusViewerView>> {
    if post_owner(db, post).await? != user {
        return Err(not_found("Status"));
    }
    let rows = query(
        db,
        "SELECT viewer_user_id, viewed_at, liked FROM status_views WHERE post_id = $1 AND viewer_user_id <> $2 \
          ORDER BY viewed_at DESC LIMIT 500",
        vec![post.into(), user.into()],
    )
    .await?;
    let ids: Vec<Uuid> = rows
        .iter()
        .map(|r| r.try_get("", "viewer_user_id"))
        .collect::<Result<_, _>>()?;
    let names = people(db, &ids, None).await?;
    let mut out = vec![];
    for r in rows {
        let id: Uuid = r.try_get("", "viewer_user_id")?;
        if let Some(person) = names.get(&id) {
            out.push(StatusViewerView {
                person: person.clone(),
                viewed_at: r.try_get("", "viewed_at")?,
                liked: r.try_get("", "liked")?,
            });
        }
    }
    Ok(out)
}

pub async fn delete_status<C: ConnectionTrait>(db: &C, user: Uuid, post: Uuid) -> ApiResult<()> {
    let n = exec(
        db,
        "DELETE FROM status_posts WHERE id = $1 AND user_id = $2",
        vec![post.into(), user.into()],
    )
    .await?;
    if n == 0 {
        return Err(not_found("Status"));
    }
    Ok(())
}

/// Delete statuses past their 24 hours (with views), status media no post uses any more,
/// channel media never posted and unfinished uploads. Returns the rows removed.
pub async fn purge_expired<C: ConnectionTrait>(db: &C) -> ApiResult<u64> {
    let mut n = exec(
        db,
        "DELETE FROM status_posts WHERE expires_at <= now()",
        vec![],
    )
    .await?;
    n += exec(
        db,
        "DELETE FROM social_media m WHERE m.purpose = 'STATUS' AND m.created_at < now() - interval '2 hours' \
           AND NOT EXISTS (SELECT 1 FROM status_posts p WHERE p.media_id = m.id)",
        vec![],
    )
    .await?;
    n += exec(
        db,
        "DELETE FROM social_media m WHERE m.purpose = 'CHANNEL' AND m.created_at < now() - interval '1 day' \
           AND NOT EXISTS (SELECT 1 FROM social_channel_posts p WHERE p.media_id = m.id)",
        vec![],
    )
    .await?;
    n += exec(
        db,
        "DELETE FROM social_media WHERE completed_at IS NULL AND created_at < now() - interval '1 day'",
        vec![],
    )
    .await?;
    Ok(n)
}

// -------------------------------------------------------------------------- channels

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub channel_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner: PersonView,
    pub follower_count: i64,
    pub following: bool,
    pub is_owner: bool,
    pub unread_count: i64,
    pub last_post: Option<ChannelPostView>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReactionCount {
    pub emoji: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPostView {
    pub post_id: Uuid,
    pub channel_id: Uuid,
    pub kind: PostKind,
    pub text: Option<String>,
    pub media: Option<MediaView>,
    pub created_at: DateTime<Utc>,
    pub reactions: Vec<ReactionCount>,
    pub my_reaction: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelRequest {
    /// 1–80 characters.
    pub name: Option<String>,
    /// At most 500 characters (`""` clears it).
    pub description: Option<String>,
}

const CHANNEL_SELECT: &str = "SELECT c.id, c.owner_user_id, c.name, c.description, c.created_at, \
        (SELECT count(*) FROM social_channel_followers f WHERE f.channel_id = c.id) AS followers, \
        mf.last_read_at, (mf.user_id IS NOT NULL) AS following, \
        (SELECT count(*) FROM social_channel_posts p WHERE p.channel_id = c.id AND mf.user_id IS NOT NULL \
            AND p.created_at > mf.last_read_at AND p.author_user_id <> $1) AS unread, \
        (SELECT max(p.created_at) FROM social_channel_posts p WHERE p.channel_id = c.id) AS last_at \
   FROM social_channels c \
   LEFT JOIN social_channel_followers mf ON mf.channel_id = c.id AND mf.user_id = $1";

async fn channel_views<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    rows: Vec<QueryResult>,
) -> ApiResult<Vec<ChannelView>> {
    let owners: Vec<Uuid> = rows
        .iter()
        .map(|r| r.try_get("", "owner_user_id"))
        .collect::<Result<_, _>>()?;
    let names = people(db, &owners, None).await?;
    let ids: Vec<Uuid> = rows
        .iter()
        .map(|r| r.try_get("", "id"))
        .collect::<Result<_, _>>()?;
    let latest = latest_posts(db, me, &ids).await?;
    let mut out = vec![];
    for r in rows {
        let id: Uuid = r.try_get("", "id")?;
        let owner: Uuid = r.try_get("", "owner_user_id")?;
        out.push(ChannelView {
            channel_id: id,
            name: r.try_get("", "name")?,
            description: r.try_get("", "description")?,
            owner: names.get(&owner).cloned().unwrap_or(PersonView {
                user_id: owner,
                name: "CF Meet".into(),
                avatar_url: None,
                principal_id: None,
            }),
            follower_count: r.try_get("", "followers")?,
            following: r.try_get("", "following")?,
            is_owner: owner == me,
            unread_count: r.try_get("", "unread")?,
            last_post: latest.get(&id).cloned(),
            created_at: r.try_get("", "created_at")?,
        });
    }
    Ok(out)
}

async fn latest_posts<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    channels: &[Uuid],
) -> ApiResult<HashMap<Uuid, ChannelPostView>> {
    if channels.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = query(
        db,
        &format!(
            "SELECT DISTINCT ON (p.channel_id) {POST_COLUMNS} {POST_FROM} WHERE p.channel_id = ANY($2) \
             ORDER BY p.channel_id, p.created_at DESC"
        ),
        vec![me.into(), channels.to_vec().into()],
    )
    .await?;
    let mut out = HashMap::new();
    for r in rows {
        let p = post_view(&r)?;
        out.insert(p.channel_id, p);
    }
    Ok(out)
}

/// The directory: channels by followers, optionally matching `q`.
pub async fn directory<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    q: Option<&str>,
) -> ApiResult<Vec<ChannelView>> {
    let pattern = q.map(str::trim).filter(|s| !s.is_empty()).map(|s| {
        format!(
            "%{}%",
            s.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let rows = query(
        db,
        &format!(
            "{CHANNEL_SELECT} WHERE ($2::text IS NULL OR c.name ILIKE $2 OR c.description ILIKE $2) \
             ORDER BY followers DESC, c.name LIMIT 50"
        ),
        vec![me.into(), pattern.into()],
    )
    .await?;
    channel_views(db, me, rows).await
}

/// Channels you follow or own, latest activity first.
pub async fn following<C: ConnectionTrait>(db: &C, me: Uuid) -> ApiResult<Vec<ChannelView>> {
    let rows = query(
        db,
        &format!(
            "{CHANNEL_SELECT} WHERE mf.user_id IS NOT NULL OR c.owner_user_id = $1 \
             ORDER BY coalesce((SELECT max(p.created_at) FROM social_channel_posts p WHERE p.channel_id = c.id), c.created_at) DESC LIMIT 200"
        ),
        vec![me.into()],
    )
    .await?;
    channel_views(db, me, rows).await
}

pub async fn channel<C: ConnectionTrait>(db: &C, me: Uuid, id: Uuid) -> ApiResult<ChannelView> {
    let rows = query(
        db,
        &format!("{CHANNEL_SELECT} WHERE c.id = $2"),
        vec![me.into(), id.into()],
    )
    .await?;
    channel_views(db, me, rows)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| not_found("Channel"))
}

fn channel_fields(
    req: &ChannelRequest,
    creating: bool,
) -> ApiResult<(Option<String>, Option<Option<String>>)> {
    let name = match req.name.as_deref().map(str::trim) {
        None if creating => return Err(invalid("Give the channel a name")),
        None => None,
        Some("") => return Err(invalid("Give the channel a name")),
        Some(n) if n.chars().count() > 80 => {
            return Err(invalid("A channel name is at most 80 characters"))
        }
        Some(n) => Some(n.to_string()),
    };
    let description = match req.description.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(d) if d.chars().count() > 500 => {
            return Err(invalid("A description is at most 500 characters"))
        }
        Some(d) => Some(Some(d.to_string())),
    };
    Ok((name, description))
}

pub async fn create_channel<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    req: &ChannelRequest,
) -> ApiResult<ChannelView> {
    let (name, description) = channel_fields(req, true)?;
    let owned = query_one(
        db,
        "SELECT count(*) AS n FROM social_channels WHERE owner_user_id = $1",
        vec![me.into()],
    )
    .await?
    .map(|r| r.try_get::<i64>("", "n"))
    .transpose()?
    .unwrap_or(0);
    if owned >= MAX_CHANNELS_PER_USER {
        return Err(invalid("You can own at most 10 channels"));
    }
    let id = Uuid::new_v4();
    exec(
        db,
        "INSERT INTO social_channels (id, owner_user_id, name, description) VALUES ($1, $2, $3, $4)",
        vec![id.into(), me.into(), name.into(), description.flatten().into()],
    )
    .await?;
    exec(
        db,
        "INSERT INTO social_channel_followers (channel_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        vec![id.into(), me.into()],
    )
    .await?;
    channel(db, me, id).await
}

async fn require_owner<C: ConnectionTrait>(db: &C, me: Uuid, id: Uuid) -> ApiResult<()> {
    let owner = query_one(
        db,
        "SELECT owner_user_id FROM social_channels WHERE id = $1",
        vec![id.into()],
    )
    .await?
    .map(|r| r.try_get::<Uuid>("", "owner_user_id"))
    .transpose()?
    .ok_or_else(|| not_found("Channel"))?;
    if owner != me {
        return Err(denied("Only the channel's owner can do this"));
    }
    Ok(())
}

pub async fn update_channel<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    id: Uuid,
    req: &ChannelRequest,
) -> ApiResult<ChannelView> {
    require_owner(db, me, id).await?;
    let (name, description) = channel_fields(req, false)?;
    exec(
        db,
        "UPDATE social_channels SET name = coalesce($2, name), \
            description = CASE WHEN $3 THEN $4 ELSE description END, updated_at = now() WHERE id = $1",
        vec![
            id.into(),
            name.into(),
            description.is_some().into(),
            description.flatten().into(),
        ],
    )
    .await?;
    channel(db, me, id).await
}

pub async fn delete_channel<C: ConnectionTrait>(db: &C, me: Uuid, id: Uuid) -> ApiResult<()> {
    require_owner(db, me, id).await?;
    exec(
        db,
        "DELETE FROM social_channels WHERE id = $1",
        vec![id.into()],
    )
    .await?;
    Ok(())
}

pub async fn set_following<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    id: Uuid,
    follow: bool,
) -> ApiResult<ChannelView> {
    let exists = query_one(
        db,
        "SELECT 1 AS x FROM social_channels WHERE id = $1",
        vec![id.into()],
    )
    .await?
    .is_some();
    if !exists {
        return Err(not_found("Channel"));
    }
    if follow {
        exec(
            db,
            "INSERT INTO social_channel_followers (channel_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            vec![id.into(), me.into()],
        )
        .await?;
    } else {
        exec(
            db,
            "DELETE FROM social_channel_followers WHERE channel_id = $1 AND user_id = $2",
            vec![id.into(), me.into()],
        )
        .await?;
    }
    channel(db, me, id).await
}

const POST_COLUMNS: &str = "p.id, p.channel_id, p.kind, p.body_text, p.created_at, \
    m.id AS m_id, m.kind AS m_kind, m.content_type AS m_type, m.size_bytes AS m_size, m.chunk_count AS m_chunks, \
    m.duration_ms AS m_duration, m.completed_at AS m_completed, \
    (SELECT emoji FROM social_channel_reactions r WHERE r.post_id = p.id AND r.user_id = $1) AS mine, \
    (SELECT coalesce(json_agg(json_build_object('emoji', x.emoji, 'count', x.n) ORDER BY x.n DESC), '[]'::json)::text \
       FROM (SELECT emoji, count(*) AS n FROM social_channel_reactions r WHERE r.post_id = p.id GROUP BY emoji) x) AS reactions";
const POST_FROM: &str = "FROM social_channel_posts p LEFT JOIN social_media m ON m.id = p.media_id";

fn post_view(r: &QueryResult) -> ApiResult<ChannelPostView> {
    let kind: String = r.try_get("", "kind")?;
    let media = match r.try_get::<Option<Uuid>>("", "m_id")? {
        Some(mid) => {
            let mk: String = r.try_get("", "m_kind")?;
            let completed: Option<DateTime<Utc>> = r.try_get("", "m_completed")?;
            Some(MediaView {
                media_id: mid,
                kind: MediaKind::parse(&mk).unwrap_or(MediaKind::Image),
                content_type: r.try_get("", "m_type")?,
                size_bytes: r.try_get("", "m_size")?,
                chunk_bytes: CHUNK_BYTES as i64,
                chunk_count: r.try_get("", "m_chunks")?,
                duration_ms: r.try_get("", "m_duration")?,
                complete: completed.is_some(),
            })
        }
        None => None,
    };
    let reactions: String = r.try_get("", "reactions")?;
    let reactions: Vec<serde_json::Value> = serde_json::from_str(&reactions).unwrap_or_default();
    Ok(ChannelPostView {
        post_id: r.try_get("", "id")?,
        channel_id: r.try_get("", "channel_id")?,
        kind: PostKind::parse(&kind).unwrap_or(PostKind::Text),
        text: r.try_get("", "body_text")?,
        media,
        created_at: r.try_get("", "created_at")?,
        reactions: reactions
            .into_iter()
            .filter_map(|v| {
                Some(ReactionCount {
                    emoji: v["emoji"].as_str()?.to_string(),
                    count: v["count"].as_i64()?,
                })
            })
            .collect(),
        my_reaction: r.try_get("", "mine")?,
    })
}

/// A channel's posts, newest first (before `before`); reading marks them read.
pub async fn posts<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    id: Uuid,
    before: Option<DateTime<Utc>>,
) -> ApiResult<Vec<ChannelPostView>> {
    let exists = query_one(
        db,
        "SELECT 1 AS x FROM social_channels WHERE id = $1",
        vec![id.into()],
    )
    .await?
    .is_some();
    if !exists {
        return Err(not_found("Channel"));
    }
    let rows = query(
        db,
        &format!(
            "SELECT {POST_COLUMNS} {POST_FROM} WHERE p.channel_id = $2 AND ($3::timestamptz IS NULL OR p.created_at < $3) \
             ORDER BY p.created_at DESC LIMIT 30"
        ),
        vec![me.into(), id.into(), before.into()],
    )
    .await?;
    if before.is_none() {
        exec(
            db,
            "UPDATE social_channel_followers SET last_read_at = now() WHERE channel_id = $1 AND user_id = $2",
            vec![id.into(), me.into()],
        )
        .await?;
    }
    rows.iter().map(post_view).collect()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelPostRequest {
    /// Words, or a photo / video caption (at most 4000 characters).
    pub text: Option<String>,
    /// A completed CHANNEL upload of the owner.
    pub media_id: Option<Uuid>,
}

pub async fn create_post<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    id: Uuid,
    req: &ChannelPostRequest,
) -> ApiResult<ChannelPostView> {
    require_owner(db, me, id).await?;
    let text = clean(&req.text, 4000, "A post")?;
    let kind = match req.media_id {
        None => {
            if text.is_none() {
                return Err(invalid("Write something or add a photo or video"));
            }
            PostKind::Text
        }
        Some(mid) => {
            let row = load_media(db, mid)
                .await?
                .ok_or_else(|| not_found("Upload"))?;
            let owner: Uuid = row.try_get("", "owner_user_id")?;
            let purpose: String = row.try_get("", "purpose")?;
            let view = media_view(&row)?;
            if owner != me || purpose != "CHANNEL" {
                return Err(not_found("Upload"));
            }
            if !view.complete {
                return Err(invalid("The upload isn't finished"));
            }
            match view.kind {
                MediaKind::Image => PostKind::Image,
                MediaKind::Video => PostKind::Video,
                MediaKind::Audio => PostKind::Audio,
            }
        }
    };
    let post = Uuid::new_v4();
    exec(
        db,
        "INSERT INTO social_channel_posts (id, channel_id, author_user_id, body_text, media_id, kind) VALUES ($1, $2, $3, $4, $5, $6)",
        vec![
            post.into(),
            id.into(),
            me.into(),
            text.into(),
            req.media_id.into(),
            kind.as_str().into(),
        ],
    )
    .await?;
    let row = query_one(
        db,
        &format!("SELECT {POST_COLUMNS} {POST_FROM} WHERE p.id = $2"),
        vec![me.into(), post.into()],
    )
    .await?
    .ok_or_else(|| ApiError::InternalError("post read-back".into()))?;
    post_view(&row)
}

pub async fn delete_post<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    id: Uuid,
    post: Uuid,
) -> ApiResult<()> {
    require_owner(db, me, id).await?;
    let n = exec(
        db,
        "DELETE FROM social_channel_posts WHERE id = $1 AND channel_id = $2",
        vec![post.into(), id.into()],
    )
    .await?;
    if n == 0 {
        return Err(not_found("Post"));
    }
    Ok(())
}

/// Your one reaction to a post (`None` removes it).
pub async fn react<C: ConnectionTrait>(
    db: &C,
    me: Uuid,
    id: Uuid,
    post: Uuid,
    emoji: Option<&str>,
) -> ApiResult<ChannelPostView> {
    let exists = query_one(
        db,
        "SELECT 1 AS x FROM social_channel_posts WHERE id = $1 AND channel_id = $2",
        vec![post.into(), id.into()],
    )
    .await?
    .is_some();
    if !exists {
        return Err(not_found("Post"));
    }
    match emoji.map(str::trim) {
        Some(e)
            if !e.is_empty()
                && e.chars().count() <= 8
                && e.len() <= 16
                && !e
                    .chars()
                    .any(|c| c.is_ascii_alphanumeric() || c.is_whitespace()) =>
        {
            exec(
                db,
                "INSERT INTO social_channel_reactions (post_id, user_id, emoji) VALUES ($1, $2, $3) \
                 ON CONFLICT (post_id, user_id) DO UPDATE SET emoji = EXCLUDED.emoji, reacted_at = now()",
                vec![post.into(), me.into(), e.to_string().into()],
            )
            .await?;
        }
        Some(_) => return Err(invalid("A reaction is one emoji")),
        None => {
            exec(
                db,
                "DELETE FROM social_channel_reactions WHERE post_id = $1 AND user_id = $2",
                vec![post.into(), me.into()],
            )
            .await?;
        }
    }
    let row = query_one(
        db,
        &format!("SELECT {POST_COLUMNS} {POST_FROM} WHERE p.id = $2"),
        vec![me.into(), post.into()],
    )
    .await?
    .ok_or_else(|| not_found("Post"))?;
    post_view(&row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_is_recognised_by_content() {
        assert!(sniff_matches(MediaKind::Image, &[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(sniff_matches(MediaKind::Image, b"\x89PNG\r\n\x1a\n0000"));
        assert!(!sniff_matches(MediaKind::Image, b"<svg xmlns="));
        assert!(sniff_matches(MediaKind::Video, b"\0\0\0\x18ftypmp42"));
        assert!(sniff_matches(
            MediaKind::Video,
            &[0x1A, 0x45, 0xDF, 0xA3, 0, 0]
        ));
        assert!(!sniff_matches(MediaKind::Video, b"GIF89a00000000"));
        assert!(sniff_matches(MediaKind::Audio, b"OggS\0\0"));
        assert!(sniff_matches(MediaKind::Audio, b"ID3\x04"));
    }

    #[test]
    fn content_types_follow_the_kind() {
        assert!(MediaKind::Video.accepts("video/webm;codecs=vp9"));
        assert!(!MediaKind::Video.accepts("image/png"));
        assert!(MediaKind::Audio.accepts("audio/webm"));
        assert!(!MediaKind::Image.accepts("image/svg+xml"));
    }
}

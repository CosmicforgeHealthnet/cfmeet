//! Status and channel media: photos (≤ 8 MiB), voice notes / music (≤ 8 MiB) and videos
//! (≤ 64 MiB) in protected object storage, one object per part.
//!
//! * **Start** (`POST …/media`): kind, content type, size, duration. The answer says how
//!   many parts of `partBytes` to send (the last one carries the rest).
//! * **Part** (`POST …/media/{id}/parts/{part}`, body `{sha256}`): a signed single-object
//!   upload of exactly that part (size and SHA-256 are signed, the store refuses anything
//!   else). Asking again for the same part and digest is idempotent; a stored part cannot
//!   be replaced by different bytes. Parts are independent, so a client may send several
//!   at once.
//! * **Resume** (`GET …/media/{id}`): the owner sees `storedParts`, checked against the store.
//! * **Complete**: every part stored, the content recognised as the declared kind (a file
//!   that is not is deleted). Only COMPLETE media of the caller can be posted.
//! * **Cancel** (`DELETE …/media/{id}`): the owner removes an upload no live status / post uses.
//! * **Read** (`POST …/media/{id}/download-grants`): short-lived signed reads of every part,
//!   minted only after the reader passed the same gate as the status or channel post.
//!
//! The sweep removes uploads not completed within 24 hours, completed media no live status
//! (or channel post) uses after 24 hours, and status media as soon as its last status ends.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::not_found;
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    limits::{self, Window},
    state::AppState,
};

/// Size of every part but the last.
pub const PART_BYTES: i64 = 4 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: i64 = 8 * 1024 * 1024;
pub const MAX_AUDIO_BYTES: i64 = 8 * 1024 * 1024;
pub const MAX_VIDEO_BYTES: i64 = 64 * 1024 * 1024;
/// Uploads started per principal and hour.
pub const UPLOADS_PER_HOUR: i64 = 120;
/// An unfinished upload, or finished media nothing uses, is removed after this.
pub const UNUSED_TTL_HOURS: i64 = 24;
/// Signed part uploads stay valid this long.
pub const UPLOAD_TTL: Duration = Duration::from_secs(3600);
/// Signed part reads stay valid this long (re-requested by the client when needed).
pub const READ_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Status,
    Channel,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Status => "STATUS",
            Purpose::Channel => "CHANNEL",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "IMAGE",
            MediaKind::Video => "VIDEO",
            MediaKind::Audio => "AUDIO",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "IMAGE" => Some(MediaKind::Image),
            "VIDEO" => Some(MediaKind::Video),
            "AUDIO" => Some(MediaKind::Audio),
            _ => None,
        }
    }

    fn max_bytes(self) -> i64 {
        match self {
            MediaKind::Image => MAX_IMAGE_BYTES,
            MediaKind::Video => MAX_VIDEO_BYTES,
            MediaKind::Audio => MAX_AUDIO_BYTES,
        }
    }

    fn accepts(self, content_type: &str) -> bool {
        let t = content_type.split(';').next().unwrap_or("").trim();
        match self {
            MediaKind::Image => {
                matches!(t, "image/jpeg" | "image/png" | "image/webp" | "image/gif")
            }
            MediaKind::Video => matches!(t, "video/mp4" | "video/webm" | "video/quicktime"),
            MediaKind::Audio => matches!(
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

/// Recognises the first bytes of an upload as the declared kind of media.
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

#[derive(Debug, Clone, FromQueryResult)]
pub struct MediaRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub owner_principal_id: Uuid,
    pub purpose: String,
    pub kind: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub part_bytes: i32,
    pub part_count: i32,
    pub duration_ms: Option<i32>,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl MediaRow {
    pub fn complete(&self) -> bool {
        self.state == "COMPLETE"
    }

    pub fn media_kind(&self) -> Option<MediaKind> {
        MediaKind::parse(&self.kind)
    }

    fn part_size(&self, part: i32) -> i64 {
        if part == self.part_count - 1 {
            self.size_bytes - self.part_bytes as i64 * (self.part_count as i64 - 1)
        } else {
            self.part_bytes as i64
        }
    }
}

const COLUMNS: &str =
    "id, tenant_id, application_id, environment_id, owner_principal_id, purpose, kind, \
    media_type, size_bytes, part_bytes, part_count, duration_ms, state, created_at, completed_at";

pub fn object_key(environment_id: Uuid, media_id: Uuid, part: i32) -> String {
    format!(
        "social/{}/{}/{part}",
        environment_id.simple(),
        media_id.simple()
    )
}

/// The public view (no object key, no URL). `stored_parts` only for the uploading owner.
pub fn view(row: &MediaRow, stored_parts: Option<&[i32]>) -> Value {
    let mut v = json!({
        "mediaId": row.id,
        "kind": row.kind,
        "contentType": row.media_type,
        "sizeBytes": row.size_bytes,
        "partBytes": row.part_bytes,
        "partCount": row.part_count,
        "durationMs": row.duration_ms,
        "state": row.state,
        "complete": row.complete(),
        "createdAt": rfc3339(row.created_at),
    });
    if let Some(parts) = stored_parts {
        v["storedParts"] = json!(parts);
        if !row.complete() {
            v["uploadExpiresAt"] = json!(rfc3339(
                row.created_at + chrono::Duration::hours(UNUSED_TTL_HOURS)
            ));
        }
    }
    v
}

fn storage_error(e: cf_object_storage::StorageError) -> Error {
    match e {
        cf_object_storage::StorageError::NotConfigured => {
            Error::reason(ErrorCode::FeatureDisabled, "MEDIA_STORAGE_NOT_CONFIGURED")
        }
        cf_object_storage::StorageError::Integrity => Error::code(ErrorCode::IntegrityCheckFailed),
        _ => Error::code(ErrorCode::StorageUnavailable),
    }
}

fn require_storage(state: &AppState) -> Result<()> {
    if state.storage.configured() {
        Ok(())
    } else {
        Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "MEDIA_STORAGE_NOT_CONFIGURED",
        ))
    }
}

/// Start an upload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMedia {
    pub kind: MediaKind,
    pub content_type: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub duration_ms: Option<i32>,
}

pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    purpose: Purpose,
    req: CreateMedia,
) -> Result<Value> {
    require_storage(state)?;
    let content_type = req.content_type.trim().to_ascii_lowercase();
    if content_type.len() > 64 || !req.kind.accepts(&content_type) {
        return Err(Error::validation("This type of file isn't supported here"));
    }
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if req.size_bytes <= 0 || req.size_bytes > req.kind.max_bytes() {
        return Err(Error::validation(format!(
            "Files of this kind can be at most {} MB",
            req.kind.max_bytes() / (1024 * 1024)
        )));
    }
    if let Some(d) = req.duration_ms {
        if !(0..=3_600_000).contains(&d) {
            return Err(Error::validation("durationMs is out of range"));
        }
    }
    if req.kind == MediaKind::Video && req.duration_ms.is_none() {
        return Err(Error::validation("A video needs its durationMs"));
    }
    let now = state.now();
    let key = format!(
        "social-media:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(&state.db, &key, Window::Hour, UPLOADS_PER_HOUR, now).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let parts = ((req.size_bytes + PART_BYTES - 1) / PART_BYTES) as i32;
    let id = Uuid::new_v4();
    db::exec(
        &state.db,
        "INSERT INTO messaging_social_media (id, tenant_id, application_id, environment_id, owner_principal_id, purpose, \
           kind, media_type, size_bytes, part_bytes, part_count, duration_ms, state, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'UPLOADING', $13)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
            purpose.as_str().into(),
            req.kind.as_str().into(),
            content_type.into(),
            req.size_bytes.into(),
            (PART_BYTES as i32).into(),
            parts.into(),
            req.duration_ms.into(),
            now.into(),
        ],
    )
    .await?;
    let row = load(&state.db, ctx.scope.environment_id, id)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    crate::metrics::inc(
        "messaging_social_media_started_total",
        &[("purpose", purpose.as_str())],
    );
    Ok(view(&row, Some(&[])))
}

pub async fn load<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    id: Uuid,
) -> Result<Option<MediaRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_social_media WHERE id = $1 AND environment_id = $2"
        ),
        vec![id.into(), environment_id.into()],
    )
    .await?)
}

/// The caller's own media of `purpose` (anything else is 404).
async fn owned(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    purpose: Purpose,
    id: Uuid,
) -> Result<MediaRow> {
    load(&state.db, ctx.scope.environment_id, id)
        .await?
        .filter(|m| m.owner_principal_id == acting.principal_id && m.purpose == purpose.as_str())
        .ok_or_else(not_found)
}

/// Completed media of the caller of `purpose` and `kind`, for a new status / post.
pub async fn usable<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    owner: Uuid,
    purpose: Purpose,
    id: Uuid,
) -> Result<MediaRow> {
    let row = load(db, ctx.scope.environment_id, id)
        .await?
        .filter(|m| m.owner_principal_id == owner && m.purpose == purpose.as_str())
        .ok_or_else(not_found)?;
    if !row.complete() {
        return Err(Error::reason(
            ErrorCode::AttachmentNotReady,
            "MEDIA_NOT_COMPLETE",
        ));
    }
    Ok(row)
}

#[derive(FromQueryResult)]
struct PartRow {
    part: i32,
    size_bytes: i32,
    sha256: String,
    stored_at: Option<DateTime<Utc>>,
}

async fn parts<C: ConnectionTrait>(db: &C, media: Uuid) -> Result<Vec<PartRow>> {
    Ok(db::all(
        db,
        "SELECT part, size_bytes, sha256, stored_at FROM messaging_social_media_parts WHERE media_id = $1 ORDER BY part",
        vec![media.into()],
    )
    .await?)
}

/// The parts the store holds (declared parts not yet seen stored are checked with HEAD).
async fn stored_parts(state: &AppState, row: &MediaRow) -> Result<Vec<i32>> {
    let mut out = Vec::new();
    for p in parts(&state.db, row.id).await? {
        if p.stored_at.is_some() {
            out.push(p.part);
            continue;
        }
        let meta = state
            .storage
            .head(&object_key(row.environment_id, row.id, p.part))
            .await
            .map_err(storage_error)?;
        if meta.is_some_and(|m| m.size == p.size_bytes as u64) {
            db::exec(
                &state.db,
                "UPDATE messaging_social_media_parts SET stored_at = $3 WHERE media_id = $1 AND part = $2 AND stored_at IS NULL",
                vec![row.id.into(), p.part.into(), state.now().into()],
            )
            .await?;
            out.push(p.part);
        }
    }
    Ok(out)
}

/// The caller's view of media: the owner (with the stored parts of an upload), or a reader
/// the status / channel gate admits.
pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    purpose: Purpose,
    id: Uuid,
) -> Result<Value> {
    let row = load(&state.db, ctx.scope.environment_id, id)
        .await?
        .filter(|m| m.purpose == purpose.as_str())
        .ok_or_else(not_found)?;
    if row.owner_principal_id == acting.principal_id {
        let stored = if row.complete() {
            (0..row.part_count).collect()
        } else {
            require_storage(state)?;
            stored_parts(state, &row).await?
        };
        return Ok(view(&row, Some(&stored)));
    }
    if !readable(state, ctx, acting.principal_id, &row).await? {
        return Err(not_found());
    }
    Ok(view(&row, None))
}

/// Ask to upload one part.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartRequest {
    /// Lower-case hex SHA-256 of the part's bytes.
    pub sha256: String,
    /// The part's size (optional; must match the server's split when given).
    #[serde(default)]
    pub size_bytes: Option<i64>,
}

pub async fn part_upload(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    purpose: Purpose,
    id: Uuid,
    part: i32,
    req: PartRequest,
) -> Result<Value> {
    require_storage(state)?;
    let row = owned(state, ctx, acting, purpose, id).await?;
    if row.complete() {
        return Err(Error::reason(ErrorCode::Conflict, "MEDIA_ALREADY_COMPLETE"));
    }
    if part < 0 || part >= row.part_count {
        return Err(Error::validation("No such part of this upload"));
    }
    let sha = req.sha256.trim().to_ascii_lowercase();
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::validation("sha256 must be 64 hex characters"));
    }
    let size = row.part_size(part);
    if req.size_bytes.is_some_and(|s| s != size) {
        return Err(Error::validation(format!(
            "Part {part} must be {size} bytes"
        )));
    }
    let now = state.now();
    let existing: Option<PartRow> = db::one(
        &state.db,
        "SELECT part, size_bytes, sha256, stored_at FROM messaging_social_media_parts WHERE media_id = $1 AND part = $2",
        vec![row.id.into(), part.into()],
    )
    .await?;
    let key = object_key(row.environment_id, row.id, part);
    let stored = match &existing {
        Some(p) if p.stored_at.is_some() => true,
        Some(_) => state
            .storage
            .head(&key)
            .await
            .map_err(storage_error)?
            .is_some_and(|m| m.size == size as u64),
        None => false,
    };
    if stored {
        let same = existing.as_ref().is_some_and(|p| p.sha256 == sha);
        if !same {
            return Err(Error::reason(ErrorCode::Conflict, "PART_ALREADY_STORED"));
        }
        db::exec(
            &state.db,
            "UPDATE messaging_social_media_parts SET stored_at = coalesce(stored_at, $3) WHERE media_id = $1 AND part = $2",
            vec![row.id.into(), part.into(), now.into()],
        )
        .await?;
        return Ok(
            json!({ "part": part, "sizeBytes": size, "sha256": sha, "stored": true, "upload": Value::Null }),
        );
    }
    db::exec(
        &state.db,
        "INSERT INTO messaging_social_media_parts (media_id, part, size_bytes, sha256, declared_at) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (media_id, part) DO UPDATE SET sha256 = EXCLUDED.sha256, declared_at = EXCLUDED.declared_at \
           WHERE messaging_social_media_parts.stored_at IS NULL",
        vec![row.id.into(), part.into(), (size as i32).into(), sha.clone().into(), now.into()],
    )
    .await?;
    let w = state
        .storage
        .signed_write(&key, UPLOAD_TTL, now, size as u64, &sha, &row.media_type)
        .map_err(storage_error)?;
    Ok(json!({
        "part": part,
        "sizeBytes": size,
        "sha256": sha,
        "stored": false,
        "upload": {
            "method": w.method,
            "url": w.url,
            "headers": w.headers.iter().map(|(n, v)| json!({ "name": n, "value": v })).collect::<Vec<_>>(),
            "expiresAt": rfc3339(w.expires_at),
        },
    }))
}

pub async fn complete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    purpose: Purpose,
    id: Uuid,
) -> Result<Value> {
    require_storage(state)?;
    let row = owned(state, ctx, acting, purpose, id).await?;
    if row.complete() {
        let all: Vec<i32> = (0..row.part_count).collect();
        return Ok(view(&row, Some(&all)));
    }
    let stored = stored_parts(state, &row).await?;
    if stored.len() != row.part_count as usize {
        return Err(Error::Code {
            code: ErrorCode::AttachmentNotReady,
            reason: Some("PARTS_MISSING"),
            message: Some(format!(
                "The upload is incomplete ({} of {} parts)",
                stored.len(),
                row.part_count
            )),
        });
    }
    let first = state
        .storage
        .get(
            &object_key(row.environment_id, row.id, 0),
            row.part_size(0) as usize,
        )
        .await
        .map_err(storage_error)?
        .unwrap_or_default();
    let kind = row
        .media_kind()
        .ok_or(Error::code(ErrorCode::InternalError))?;
    if !sniff_matches(kind, &first[..first.len().min(32)]) {
        purge(state, &row).await?;
        return Err(Error::Code {
            code: ErrorCode::ContentRejected,
            reason: Some("MEDIA_NOT_RECOGNISED"),
            message: Some("The file isn't a photo, video or voice note this app can show".into()),
        });
    }
    db::exec(
        &state.db,
        "UPDATE messaging_social_media SET state = 'COMPLETE', completed_at = $2 WHERE id = $1 AND state = 'UPLOADING'",
        vec![row.id.into(), state.now().into()],
    )
    .await?;
    let row = load(&state.db, ctx.scope.environment_id, id)
        .await?
        .ok_or_else(not_found)?;
    let all: Vec<i32> = (0..row.part_count).collect();
    Ok(view(&row, Some(&all)))
}

/// Whether media is used by a live status (or a channel post).
async fn in_use<C: ConnectionTrait>(db: &C, row: &MediaRow, now: DateTime<Utc>) -> Result<bool> {
    let n = if row.purpose == Purpose::Channel.as_str() {
        db::scalar_i64(
            db,
            "SELECT count(*) FROM messaging_channel_posts WHERE media_id = $1",
            vec![row.id.into()],
        )
        .await?
    } else {
        db::scalar_i64(
            db,
            "SELECT count(*) FROM messaging_statuses WHERE (media_id = $1 OR music_media_id = $1) AND expires_at > $2",
            vec![row.id.into(), now.into()],
        )
        .await?
    };
    Ok(n > 0)
}

/// Cancel an upload / remove unused media of the caller.
pub async fn cancel(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    purpose: Purpose,
    id: Uuid,
) -> Result<Value> {
    let row = owned(state, ctx, acting, purpose, id).await?;
    if in_use(&state.db, &row, state.now()).await? {
        return Err(Error::reason(ErrorCode::Conflict, "MEDIA_IN_USE"));
    }
    purge(state, &row).await?;
    Ok(json!({ "mediaId": id, "deleted": true }))
}

/// Deletes the stored bytes of every part and then the rows. A store failure keeps the
/// rows (the sweep retries).
pub async fn purge(state: &AppState, row: &MediaRow) -> Result<()> {
    if state.storage.configured() {
        for part in 0..row.part_count {
            state
                .storage
                .delete(&object_key(row.environment_id, row.id, part))
                .await
                .map_err(storage_error)?;
        }
    }
    db::exec(
        &state.db,
        "DELETE FROM messaging_social_media WHERE id = $1",
        vec![row.id.into()],
    )
    .await?;
    Ok(())
}

/// Removes media nothing live uses any more: `ids` after their statuses ended.
pub async fn purge_if_unused(state: &AppState, environment_id: Uuid, ids: &[Uuid]) -> Result<()> {
    let now = state.now();
    for id in ids {
        if let Some(row) = load(&state.db, environment_id, *id).await? {
            if !in_use(&state.db, &row, now).await? {
                if let Err(e) = purge(state, &row).await {
                    tracing::warn!(error = %e, "status media purge deferred to the sweep");
                }
            }
        }
    }
    Ok(())
}

/// The sweep's media pass (at most `limit` media).
pub async fn purge_unused(state: &AppState, now: DateTime<Utc>, limit: i64) -> Result<u64> {
    let cutoff = now - chrono::Duration::hours(UNUSED_TTL_HOURS);
    let rows: Vec<MediaRow> = db::all(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_social_media m WHERE \
               (m.state = 'UPLOADING' AND m.created_at < $1) \
            OR (m.purpose = 'STATUS' AND m.state = 'COMPLETE' AND NOT EXISTS (SELECT 1 FROM messaging_statuses s \
                  WHERE (s.media_id = m.id OR s.music_media_id = m.id) AND s.expires_at > $2) \
                AND m.created_at < $1) \
            OR (m.purpose = 'CHANNEL' AND m.state = 'COMPLETE' AND m.created_at < $1 \
                AND NOT EXISTS (SELECT 1 FROM messaging_channel_posts p WHERE p.media_id = m.id)) \
             ORDER BY m.created_at LIMIT $3"
        ),
        vec![cutoff.into(), now.into(), limit.into()],
    )
    .await?;
    let mut n = 0;
    for row in rows {
        match purge(state, &row).await {
            Ok(()) => n += 1,
            Err(e) => {
                tracing::warn!(error = %e, "media purge failed; retried next pass");
                break;
            }
        }
    }
    Ok(n)
}

/// Whether `viewer` may read `row` (not the owner): channel media is public to the
/// environment once complete; status media needs a live status using it that the viewer
/// passes the gate for.
pub async fn readable(
    state: &AppState,
    ctx: &TrustedContext,
    viewer: Uuid,
    row: &MediaRow,
) -> Result<bool> {
    if !row.complete() {
        return Ok(false);
    }
    if row.purpose == Purpose::Channel.as_str() {
        let n = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_channel_posts WHERE media_id = $1",
            vec![row.id.into()],
        )
        .await?;
        return Ok(n > 0);
    }
    let statuses =
        super::status::using_media(&state.db, row.environment_id, row.id, state.now()).await?;
    if statuses.is_empty() {
        return Ok(false);
    }
    let gate = super::status::Gate::load(state, ctx, viewer, &statuses).await?;
    Ok(statuses.iter().any(|s| gate.allows(s)))
}

/// Short-lived signed reads of every part, after the gate.
pub async fn download_grants(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    purpose: Purpose,
    id: Uuid,
) -> Result<Value> {
    require_storage(state)?;
    let row = load(&state.db, ctx.scope.environment_id, id)
        .await?
        .filter(|m| m.purpose == purpose.as_str())
        .ok_or_else(not_found)?;
    let allowed = if row.owner_principal_id == acting.principal_id {
        row.complete()
    } else {
        readable(state, ctx, acting.principal_id, &row).await?
    };
    if !allowed {
        return Err(not_found());
    }
    let now = state.now();
    let mut parts = Vec::with_capacity(row.part_count as usize);
    let mut expires = now;
    for part in 0..row.part_count {
        let signed = state
            .storage
            .signed_read(&object_key(row.environment_id, row.id, part), READ_TTL, now)
            .map_err(storage_error)?;
        expires = signed.expires_at;
        parts.push(json!({ "part": part, "sizeBytes": row.part_size(part), "url": signed.url }));
    }
    crate::metrics::inc(
        "messaging_social_media_grants_total",
        &[("purpose", purpose.as_str())],
    );
    Ok(json!({
        "mediaId": row.id,
        "contentType": row.media_type,
        "sizeBytes": row.size_bytes,
        "expiresAt": rfc3339(expires),
        "parts": parts,
    }))
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

    #[test]
    fn keys_are_valid_object_keys() {
        let k = object_key(Uuid::new_v4(), Uuid::new_v4(), 3);
        assert!(cf_object_storage::valid_key(&k), "{k}");
    }
}

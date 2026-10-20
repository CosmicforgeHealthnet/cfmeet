//! Server-side derivatives of MANAGED attachments (§80): a THUMBNAIL of every READY
//! MANAGED PNG / JPEG image, and an INSPECTION (measured container, duration and
//! dimensions, [`super::media_inspect`]) of every READY MANAGED image, voice note and
//! video of an inspectable type.
//!
//! * Only [`super::processing::eligible`] attachments (MANAGED and READY — scanned where
//!   policy requires it) are processed; the database refuses a derivative of anything
//!   else (migration `m20261118_000016`). PRIVATE_E2EE attachments are ciphertext and are
//!   never read for processing: their thumbnails are made, encrypted and referenced by
//!   the sending client ([`super::processing::E2eeDerivative`]).
//! * Untrusted input is decoded defensively: the stored object is re-checked against its
//!   verified SHA-256, dimensions are read from the header and refused above
//!   [`MAX_PIXELS`] before any pixel is decoded, the decoder runs with explicit
//!   allocation / dimension limits on a blocking thread under a timeout, and only PNG /
//!   JPEG decoders are compiled in.
//! * The thumbnail is a re-encoded PNG of at most [`THUMBNAIL_MAX_EDGE`] pixels per edge:
//!   no metadata (EXIF location, comments) of the original survives.
//! * A derivative lives and dies with its attachment: deleting the attachment queues the
//!   derivative object for deletion too (legal holds apply to both).

use std::{io::Cursor, time::Duration};

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{processing, AttachmentRow, ATTACHMENT_COLUMNS};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    events::bounded_error,
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

pub const THUMBNAIL_MAX_EDGE: u32 = 320;
/// Largest source object read for processing.
pub const MAX_SOURCE_BYTES: usize = 25 * 1024 * 1024;
/// Largest source image (pixels) decoded.
pub const MAX_PIXELS: u64 = 40_000_000;
const MAX_EDGE: u32 = 16_384;
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
const DECODE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: i32 = 3;
const PROCESSABLE: &[&str] = &["image/png", "image/jpeg"];

/// A rendered thumbnail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
}

fn limits() -> image::Limits {
    let mut l = image::Limits::default();
    l.max_image_width = Some(MAX_EDGE);
    l.max_image_height = Some(MAX_EDGE);
    l.max_alloc = Some(MAX_ALLOC);
    l
}

/// Renders a thumbnail of untrusted image bytes (a stable error code on refusal).
pub fn render_thumbnail(
    bytes: &[u8],
    media_type: &str,
) -> std::result::Result<Thumbnail, &'static str> {
    let format = match media_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => return Err("UNSUPPORTED_FORMAT"),
    };
    let reader = |b: &[u8]| {
        let mut r = image::ImageReader::with_format(Cursor::new(b.to_vec()), format);
        r.limits(limits());
        r
    };
    let (w, h) = reader(bytes).into_dimensions().map_err(|_| "UNDECODABLE")?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("TOO_MANY_PIXELS");
    }
    let img = reader(bytes).decode().map_err(|e| match e {
        image::ImageError::Limits(_) => "DECODE_LIMIT",
        _ => "UNDECODABLE",
    })?;
    let thumb = img.thumbnail(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE);
    let mut png = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|_| "ENCODE_FAILED")?;
    Ok(Thumbnail {
        width: thumb.width(),
        height: thumb.height(),
        source_width: w,
        source_height: h,
        png,
    })
}

/// Deterministic per attachment: a retried render overwrites it, and deleting the
/// attachment always deletes it (no derivative object can be orphaned).
pub fn thumbnail_key(environment_id: Uuid, attachment_id: Uuid) -> String {
    format!(
        "drv/{environment_id}/thumbnail/{}.png",
        attachment_id.simple()
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DerivativeReport {
    pub enqueued: u64,
    pub ready: usize,
    pub failed: usize,
    pub retried: usize,
}

#[derive(FromQueryResult)]
struct Due {
    id: Uuid,
    attachment_id: Uuid,
    environment_id: Uuid,
    kind: String,
    attempts: i32,
}

/// One processing pass: enqueue READY MANAGED images without a thumbnail, then render
/// due ones under a lease (safe on several instances).
pub async fn process_due(state: &AppState, batch: i64) -> Result<DerivativeReport> {
    let mut report = DerivativeReport::default();
    if !state.attachments.media_processing || !state.storage.configured() {
        return Ok(report);
    }
    let now = state.now();
    report.enqueued = db::exec(
        &state.db,
        "INSERT INTO messaging_attachment_derivatives (id, attachment_id, environment_id, security_profile, kind, status, \
           next_attempt_at, created_at) \
         SELECT gen_random_uuid(), a.id, a.environment_id, 'MANAGED', 'THUMBNAIL', 'PENDING', $1, $1 \
           FROM messaging_attachments a \
          WHERE a.state = 'READY' AND a.security_profile = 'MANAGED' AND a.kind = 'IMAGE' AND a.media_type = ANY($2) \
            AND NOT EXISTS (SELECT 1 FROM messaging_attachment_derivatives d WHERE d.attachment_id = a.id AND d.kind = 'THUMBNAIL') \
          LIMIT $3 ON CONFLICT DO NOTHING",
        vec![
            now.into(),
            PROCESSABLE.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
            batch.into(),
        ],
    )
    .await?;
    report.enqueued += db::exec(
        &state.db,
        "INSERT INTO messaging_attachment_derivatives (id, attachment_id, environment_id, security_profile, kind, status, \
           next_attempt_at, created_at) \
         SELECT gen_random_uuid(), a.id, a.environment_id, 'MANAGED', 'INSPECTION', 'PENDING', $1, $1 \
           FROM messaging_attachments a \
          WHERE a.state = 'READY' AND a.security_profile = 'MANAGED' AND a.kind IN ('IMAGE', 'VOICE_NOTE', 'VIDEO') \
            AND a.media_type = ANY($2) \
            AND NOT EXISTS (SELECT 1 FROM messaging_attachment_derivatives d WHERE d.attachment_id = a.id AND d.kind = 'INSPECTION') \
          LIMIT $3 ON CONFLICT DO NOTHING",
        vec![
            now.into(),
            INSPECTABLE.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
            batch.into(),
        ],
    )
    .await?;
    let token = Uuid::new_v4();
    let due: Vec<Due> = db::all(
        &state.db,
        "WITH due AS (SELECT id FROM messaging_attachment_derivatives WHERE status = 'PENDING' AND next_attempt_at <= $1 \
             AND (lease_expires_at IS NULL OR lease_expires_at <= $1) ORDER BY next_attempt_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
         UPDATE messaging_attachment_derivatives d SET lease_token = $3, lease_expires_at = $4 FROM due WHERE d.id = due.id \
         RETURNING d.id, d.attachment_id, d.environment_id, d.kind, d.attempts",
        vec![
            now.into(),
            batch.into(),
            token.into(),
            (now + chrono::Duration::seconds(120)).into(),
        ],
    )
    .await?;
    for d in due {
        if d.kind == "INSPECTION" {
            match inspect_one(state, d.attachment_id).await {
                Ok(metadata) => {
                    let updated = db::exec(
                        &state.db,
                        "UPDATE messaging_attachment_derivatives SET status = 'READY', metadata = $3, error_code = NULL, \
                           attempts = attempts + 1, lease_token = NULL, lease_expires_at = NULL, completed_at = $4 \
                         WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                        vec![d.id.into(), token.into(), metadata.into(), state.now().into()],
                    )
                    .await?;
                    if updated == 1 {
                        report.ready += 1;
                        crate::metrics::inc(
                            "messaging_attachment_derivatives_total",
                            &[("outcome", "ready")],
                        );
                    }
                }
                Err(RenderError::Transient(code)) => {
                    retry_or_fail(state, &d, token, code, code, &mut report).await?;
                }
                Err(RenderError::Final(code)) => {
                    fail(state, &d, token, code).await?;
                    report.failed += 1;
                }
            }
            continue;
        }
        match render_one(state, d.attachment_id).await {
            Ok(t) => {
                let key = thumbnail_key(d.environment_id, d.attachment_id);
                let digest = hex::encode(Sha256::digest(&t.png));
                let size = t.png.len() as i64;
                if let Err(e) = state.storage.put(&key, t.png, "image/png").await {
                    retry_or_fail(
                        state,
                        &d,
                        token,
                        "STORAGE_UNAVAILABLE",
                        &e.to_string(),
                        &mut report,
                    )
                    .await?;
                    continue;
                }
                let updated = db::exec(
                    &state.db,
                    "UPDATE messaging_attachment_derivatives SET status = 'READY', object_key = $3, media_type = 'image/png', \
                       width = $4, height = $5, source_width = $6, source_height = $7, bytes = $8, sha256 = $9, error_code = NULL, \
                       attempts = attempts + 1, lease_token = NULL, lease_expires_at = NULL, completed_at = $10 \
                     WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
                    vec![
                        d.id.into(),
                        token.into(),
                        key.into(),
                        (t.width as i32).into(),
                        (t.height as i32).into(),
                        (t.source_width as i32).into(),
                        (t.source_height as i32).into(),
                        size.into(),
                        digest.into(),
                        state.now().into(),
                    ],
                )
                .await?;
                if updated == 0 {
                    // The attachment was deleted while rendering: remove what was written.
                    let _ = state
                        .storage
                        .delete(&thumbnail_key(d.environment_id, d.attachment_id))
                        .await;
                    continue;
                }
                report.ready += 1;
                crate::metrics::inc(
                    "messaging_attachment_derivatives_total",
                    &[("outcome", "ready")],
                );
            }
            Err(RenderError::Transient(code)) => {
                retry_or_fail(state, &d, token, code, code, &mut report).await?;
            }
            Err(RenderError::Final(code)) => {
                fail(state, &d, token, code).await?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

enum RenderError {
    Transient(&'static str),
    Final(&'static str),
}

async fn render_one(
    state: &AppState,
    attachment_id: Uuid,
) -> std::result::Result<Thumbnail, RenderError> {
    let row: Option<AttachmentRow> = db::one(
        &state.db,
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE id = $1"),
        vec![attachment_id.into()],
    )
    .await
    .map_err(|_| RenderError::Transient("DATABASE_UNAVAILABLE"))?;
    let row = row.ok_or(RenderError::Final("ATTACHMENT_GONE"))?;
    // The only gate: MANAGED and READY (never PRIVATE_E2EE ciphertext).
    let req = processing::eligible(&row, processing::Derivative::Thumbnail)
        .map_err(|_| RenderError::Final("NOT_ELIGIBLE"))?;
    let bytes = match state.storage.get(&req.object_key, MAX_SOURCE_BYTES).await {
        Ok(Some(b)) => b,
        Ok(None) => return Err(RenderError::Final("OBJECT_MISSING")),
        Err(cf_object_storage::StorageError::Integrity) => {
            return Err(RenderError::Final("SOURCE_TOO_LARGE"))
        }
        Err(_) => return Err(RenderError::Transient("STORAGE_UNAVAILABLE")),
    };
    // The stored bytes must be exactly what was verified at finalization.
    if hex::encode(Sha256::digest(&bytes)) != row.declared_sha256.to_ascii_lowercase() {
        return Err(RenderError::Final("INTEGRITY_MISMATCH"));
    }
    let media_type = req.media_type.clone();
    let job = tokio::task::spawn_blocking(move || render_thumbnail(&bytes, &media_type));
    match tokio::time::timeout(DECODE_TIMEOUT, job).await {
        Ok(Ok(Ok(t))) => Ok(t),
        Ok(Ok(Err(code))) => Err(RenderError::Final(code)),
        Ok(Err(_)) => Err(RenderError::Final("DECODER_PANICKED")),
        Err(_) => Err(RenderError::Final("DECODE_TIMEOUT")),
    }
}

/// Media types inspected server-side (MANAGED only).
pub const INSPECTABLE: &[&str] = &[
    "image/png",
    "image/jpeg",
    "video/mp4",
    "video/quicktime",
    "video/webm",
    "audio/mp4",
    "audio/webm",
    "audio/ogg",
    "audio/wav",
    "audio/mpeg",
    "audio/aac",
    "audio/amr",
];
/// Largest object read for inspection (larger: `SOURCE_TOO_LARGE`, never a guess).
pub const MAX_INSPECT_BYTES: usize = 64 * 1024 * 1024;

/// Reads a READY MANAGED attachment and measures it ([`super::media_inspect`]). The
/// result compares the measured duration with the client's declaration.
async fn inspect_one(
    state: &AppState,
    attachment_id: Uuid,
) -> std::result::Result<Value, RenderError> {
    let row: Option<AttachmentRow> = db::one(
        &state.db,
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM messaging_attachments WHERE id = $1"),
        vec![attachment_id.into()],
    )
    .await
    .map_err(|_| RenderError::Transient("DATABASE_UNAVAILABLE"))?;
    let row = row.ok_or(RenderError::Final("ATTACHMENT_GONE"))?;
    let req = processing::eligible(&row, processing::Derivative::Inspection)
        .map_err(|_| RenderError::Final("NOT_ELIGIBLE"))?;
    let bytes = match state.storage.get(&req.object_key, MAX_INSPECT_BYTES).await {
        Ok(Some(b)) => b,
        Ok(None) => return Err(RenderError::Final("OBJECT_MISSING")),
        Err(cf_object_storage::StorageError::Integrity) => {
            return Err(RenderError::Final("SOURCE_TOO_LARGE"))
        }
        Err(_) => return Err(RenderError::Transient("STORAGE_UNAVAILABLE")),
    };
    if hex::encode(Sha256::digest(&bytes)) != row.declared_sha256.to_ascii_lowercase() {
        return Err(RenderError::Final("INTEGRITY_MISMATCH"));
    }
    let media_type = req.media_type.clone();
    let job =
        tokio::task::spawn_blocking(move || super::media_inspect::inspect(&bytes, &media_type));
    let facts = match tokio::time::timeout(DECODE_TIMEOUT, job).await {
        Ok(Ok(Ok(f))) => f,
        Ok(Ok(Err(code))) => return Err(RenderError::Final(code)),
        Ok(Err(_)) => return Err(RenderError::Final("DECODER_PANICKED")),
        Err(_) => return Err(RenderError::Final("DECODE_TIMEOUT")),
    };
    let declared = row
        .media_metadata
        .as_ref()
        .and_then(|m| m.get("durationMs"))
        .and_then(Value::as_i64);
    let matches = match (declared, facts.duration_ms) {
        (Some(d), Some(m)) => Some(super::media_inspect::duration_matches(d, m)),
        _ => None,
    };
    if matches == Some(false) {
        crate::metrics::inc("messaging_attachment_duration_mismatch_total", &[]);
    }
    Ok(json!({
        "container": facts.container,
        "durationMs": facts.duration_ms,
        "width": facts.width,
        "height": facts.height,
        "hasVideo": facts.has_video,
        "hasAudio": facts.has_audio,
        "declaredDurationMs": declared,
        "durationMatchesDeclared": matches,
    }))
}

async fn fail(state: &AppState, d: &Due, token: Uuid, code: &str) -> Result<()> {
    db::exec(
        &state.db,
        "UPDATE messaging_attachment_derivatives SET status = 'FAILED', error_code = $3, attempts = attempts + 1, \
           lease_token = NULL, lease_expires_at = NULL, completed_at = $4 WHERE id = $1 AND lease_token = $2",
        vec![d.id.into(), token.into(), code.into(), state.now().into()],
    )
    .await?;
    crate::metrics::inc(
        "messaging_attachment_derivatives_total",
        &[("outcome", "failed")],
    );
    Ok(())
}

async fn retry_or_fail(
    state: &AppState,
    d: &Due,
    token: Uuid,
    code: &'static str,
    detail: &str,
    report: &mut DerivativeReport,
) -> Result<()> {
    if d.attempts + 1 >= MAX_ATTEMPTS {
        report.failed += 1;
        return fail(state, d, token, code).await;
    }
    tracing::warn!(error = %bounded_error(detail), "derivative rendering will be retried");
    db::exec(
        &state.db,
        "UPDATE messaging_attachment_derivatives SET attempts = attempts + 1, next_attempt_at = $3, error_code = $4, \
           lease_token = NULL, lease_expires_at = NULL WHERE id = $1 AND lease_token = $2",
        vec![
            d.id.into(),
            token.into(),
            (state.now() + chrono::Duration::seconds(30 * i64::from(d.attempts + 1))).into(),
            code.into(),
        ],
    )
    .await?;
    report.retried += 1;
    Ok(())
}

#[derive(FromQueryResult)]
struct DerivativeRow {
    kind: String,
    status: String,
    object_key: Option<String>,
    media_type: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    source_width: Option<i32>,
    source_height: Option<i32>,
    bytes: Option<i64>,
    sha256: Option<String>,
    error_code: Option<String>,
    metadata: Option<Value>,
    completed_at: Option<DateTime<Utc>>,
}

async fn rows(state: &AppState, attachment_id: Uuid) -> Result<Vec<DerivativeRow>> {
    Ok(db::all(
        &state.db,
        "SELECT kind, status, object_key, media_type, width, height, source_width, source_height, bytes, sha256, error_code, metadata, completed_at \
           FROM messaging_attachment_derivatives WHERE attachment_id = $1 AND status <> 'DELETED' ORDER BY kind",
        vec![attachment_id.into()],
    )
    .await?)
}

/// The server-made derivatives of one attachment (empty for PRIVATE_E2EE).
pub async fn views(state: &AppState, attachment_id: Uuid) -> Result<Value> {
    Ok(Value::Array(
        rows(state, attachment_id)
            .await?
            .iter()
            .map(|d| {
                json!({
                    "kind": d.kind,
                    "status": d.status,
                    "mediaType": d.media_type,
                    "width": d.width,
                    "height": d.height,
                    "sourceWidth": d.source_width,
                    "sourceHeight": d.source_height,
                    "size": d.bytes,
                    "sha256": d.sha256,
                    "errorCode": d.error_code,
                    "metadata": d.metadata,
                    "completedAt": d.completed_at.map(rfc3339),
                })
            })
            .collect(),
    ))
}

/// `POST /v1/conversations/{id}/attachments/{attachmentId}/thumbnail-grants`: the same
/// authorization as a download grant of the original.
pub async fn thumbnail_grant(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    attachment_id: Uuid,
) -> Result<Value> {
    super::require_storage(state)?;
    let row = super::authorize_read(state, ctx, acting, conversation_id, attachment_id).await?;
    let d = rows(state, row.id)
        .await?
        .into_iter()
        .find(|d| d.kind == "THUMBNAIL" && d.status == "READY")
        .ok_or(Error::reason(
            ErrorCode::AttachmentNotReady,
            "THUMBNAIL_NOT_READY",
        ))?;
    let key = d.object_key.ok_or(Error::code(ErrorCode::InternalError))?;
    let now = state.now();
    let signed = state
        .storage
        .signed_read_as(
            &key,
            state.attachments.grant_ttl,
            now,
            &cf_object_storage::ResponseOverrides {
                content_type: Some("image/png".into()),
                content_disposition: Some("inline; filename=\"thumbnail.png\"".into()),
            },
        )
        .map_err(super::storage_error)?;
    Ok(json!({
        "attachmentId": row.id,
        "kind": "THUMBNAIL",
        "method": "GET",
        "url": signed.url,
        "expiresAt": rfc3339(signed.expires_at),
        "mediaType": "image/png",
        "size": d.bytes,
        "sha256": d.sha256,
        "width": d.width,
        "height": d.height,
        "residualAccessSeconds": state.attachments.grant_ttl.as_secs(),
    }))
}

/// Queues the deletion of an attachment's derivative objects (inside the attachment's
/// deletion transaction); the deletion worker honours legal holds on the attachment.
pub async fn delete_for_attachment<C: sea_orm::ConnectionTrait>(
    db: &C,
    row: &AttachmentRow,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    if row.security_profile != "MANAGED" {
        return Ok(());
    }
    #[derive(FromQueryResult)]
    struct Old {
        previous: String,
        kind: String,
    }
    let ended: Vec<Old> = db::all(
        db,
        "WITH old AS (SELECT id, status FROM messaging_attachment_derivatives WHERE attachment_id = $1 AND status <> 'DELETED' FOR UPDATE) \
         UPDATE messaging_attachment_derivatives d SET status = 'DELETED', object_key = NULL, metadata = NULL, completed_at = $2 \
           FROM old WHERE d.id = old.id RETURNING old.status AS previous, d.kind",
        vec![row.id.into(), now.into()],
    )
    .await?;
    // Only a produced thumbnail has an object to delete (an in-flight render notices the
    // deletion and removes its own output: `process_due`).
    if ended
        .iter()
        .any(|o| o.previous == "READY" && o.kind == "THUMBNAIL")
    {
        super::lifecycle::enqueue_deletion(
            db,
            &thumbnail_key(row.environment_id, row.id),
            row.environment_id,
            Some(row.id),
            reason,
            now,
        )
        .await?;
    }
    Ok(())
}
